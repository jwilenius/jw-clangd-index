//! The `relocate` command: relocates the index of a donor worktree into a target worktree, using git
//! to choose the donor and skip deleted files, and file digests to predict what clangd reindexes.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::cdb_relocation::{CDB_RELATIVE_PATHS, CdbOutcome, CdbRelocationRequest, LinkSkipReason, find_cdb, relocate_cdb};
use super::donor_selection::find_donor;
use super::index_relocation::{INDEX_RELATIVE_DIR, IndexRelocationReport, IndexRelocationRequest, relocate_index};
use super::path_map::{PathMap, RootMapping};
use super::reindex_prediction::{
    IncludeGraph, ReindexPrediction, graph_string_map_probing, graph_tus, load_cdb_main_files, load_external_nodes, load_include_graph,
    load_nested_project_nodes, predict_reindex, without_nested_main_files,
};
use crate::format::string_table::TableCompression;
use crate::git::GitRepo;
use crate::git::worktree_changes::find_deleted_files;

/// Shard failures logged one by one; the rest are only counted.
const MAX_LOGGED_FAILURES: usize = 10;
/// Predicted TUs listed in the summary.
const MAX_LISTED_TUS: usize = 10;

pub struct WorktreeRelocationRequest {
    /// Donor worktree root; chosen among the repository's worktrees when `None`.
    pub donor_root: Option<PathBuf>,
    /// Target worktree root; the worktree of the current directory when `None`.
    pub target_root: Option<PathBuf>,
    /// Mappings applied besides donor root → target root (out-of-tree build directories).
    pub extra_mappings: Vec<RootMapping>,
    pub compression: TableCompression,
    pub is_dry_run: bool,
    pub is_overwrite: bool,
    pub is_relocate_cdb: bool,
    /// clangd's index of the files outside every project (`<cache directory>/clangd/index`), read for
    /// the prediction; `None` when unknown, so such files count as having no shard.
    pub user_index_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DonorSelection {
    Explicit,
    Automatic { changed_files: usize },
}

#[derive(Clone, Debug)]
pub struct WorktreeRelocationReport {
    pub donor_root: PathBuf,
    pub donor_selection: DonorSelection,
    pub target_root: PathBuf,
    pub is_dry_run: bool,
    pub index: IndexRelocationReport,
    pub cdb: CdbOutcome,
    pub prediction: PredictionOutcome,
    pub elapsed: Duration,
}

/// A failed prediction leaves the relocation standing, since the shards are written by then.
#[derive(Clone, Debug)]
pub enum PredictionOutcome {
    Predicted(TargetPrediction),
    Unavailable { reason: String },
}

#[derive(Clone, Debug)]
pub struct TargetPrediction {
    pub reindex: ReindexPrediction,
    /// False when no usable compilation database lists the main files, so the prediction walks the TUs
    /// of the index and holds only once the target has a database.
    pub has_cdb_main_files: bool,
}

/// The report as `relocate --json` prints it, with complete path lists where the text summary truncates.
#[derive(Debug, Serialize)]
pub struct RelocationSummary<'a> {
    pub is_dry_run: bool,
    pub donor_root: &'a Path,
    /// Files that differ between the donor's and the target's `HEAD`; `None` when the donor was given.
    pub donor_changed_files: Option<usize>,
    pub target_root: &'a Path,
    pub shards: ShardSummary,
    pub cdb: CdbSummary<'a>,
    /// `None` when the prediction failed.
    pub reindex: Option<ReindexSummary<'a>>,
    /// Why the prediction failed; `None` when `reindex` holds it.
    pub reindex_failure: Option<&'a str>,
    pub elapsed_seconds: f64,
}

#[derive(Debug, Serialize)]
pub struct ShardSummary {
    pub relocated: usize,
    pub kept_existing: usize,
    pub skipped_deleted: usize,
    pub skipped_unsupported_version: usize,
    pub failed: usize,
}

#[derive(Debug, Serialize)]
pub struct CdbSummary<'a> {
    pub action: CdbAction,
    /// The database kept or written (or that would be, in a dry run), or the link skipped.
    pub path: Option<&'a Path>,
    /// Why the link was skipped (a [`LinkSkipReason::code`]), the database kept as git tracks it, or
    /// relocating failed (the error message); `None` for the other actions.
    pub reason: Option<&'a str>,
}

/// [`CdbOutcome`] without its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CdbAction {
    /// `--no-relocate-cdb` was given and the target has a database.
    TargetHasOne,
    /// `--no-relocate-cdb` was given and the target has none, so clangd ignores the index.
    TargetHasNone,
    DonorHasNone,
    KeptExisting,
    Written,
    /// Not written, since git tracks the target's database.
    KeptTracked,
    SkippedLink,
    Failed,
}

/// The [`CdbSummary::reason`] of [`CdbAction::KeptTracked`].
const TRACKED_REASON: &str = "tracked_by_git";

#[derive(Debug, Serialize)]
pub struct ReindexSummary<'a> {
    /// False when the prediction holds only once the target has a compilation database.
    pub has_cdb_main_files: bool,
    pub reindexed_tus: &'a [PathBuf],
    pub stale_files: usize,
    pub main_files_without_shard: &'a [PathBuf],
}

struct ResolvedDonor {
    /// Canonical root.
    root: PathBuf,
    /// The root as given or listed by git, made absolute; shards may hold paths under it rather than
    /// under the canonical root when it goes through a symlink.
    given_root: PathBuf,
    /// `None` when the donor's `HEAD` is unknown, so git cannot compare it with the target.
    head: Option<String>,
    selection: DonorSelection,
}

/// A compilation database listing the target's main files, and the mapping its paths still need
/// (`None` when they are the target's already).
struct MainFileSource<'a> {
    cdb_path: PathBuf,
    path_map: Option<&'a PathMap>,
}

/// The main files clangd loads, and whether a compilation database listed them.
struct MainFiles {
    paths: Vec<PathBuf>,
    is_from_cdb: bool,
}

pub fn relocate_worktree(request: &WorktreeRelocationRequest) -> Result<WorktreeRelocationReport> {
    let started = Instant::now();
    let target_root = resolve_target_root(request.target_root.as_deref())?;
    let donor = resolve_donor(request.donor_root.as_deref(), &target_root)?;
    let path_map = build_path_map(&donor, &target_root, &request.extra_mappings)?;
    let deleted_files = load_deleted_files(donor.head.as_deref(), &target_root);

    let index = relocate_index(&IndexRelocationRequest {
        donor_dir: donor.root.join(INDEX_RELATIVE_DIR),
        target_dir: target_root.join(INDEX_RELATIVE_DIR),
        path_map: path_map.clone(),
        compression: request.compression,
        is_overwrite: request.is_overwrite,
        is_dry_run: request.is_dry_run,
        deleted_target_paths: deleted_files.clone(),
    })?;
    warn_about_index(&index, &donor.root);
    let cdb = place_cdb(request, &donor.root, &target_root, &path_map);
    let prediction = predict(request, &donor.root, &target_root, &path_map, &cdb, &deleted_files)
        .map_or_else(|error| unavailable_prediction(&error), PredictionOutcome::Predicted);

    Ok(WorktreeRelocationReport {
        donor_root: donor.root,
        donor_selection: donor.selection,
        target_root,
        is_dry_run: request.is_dry_run,
        index,
        cdb,
        prediction,
        elapsed: started.elapsed(),
    })
}

fn resolve_target_root(explicit_root: Option<&Path>) -> Result<PathBuf> {
    let Some(explicit_root) = explicit_root else {
        let toplevel = GitRepo::new(Path::new(".")).toplevel().context("finding the target worktree (pass --to TARGET_ROOT)")?;
        return fs::canonicalize(&toplevel).with_context(|| format!("target root {}", toplevel.display()));
    };
    let root = fs::canonicalize(explicit_root).with_context(|| format!("target root {}", explicit_root.display()))?;
    ensure_worktree_root(&root, "--to")?;
    Ok(root)
}

fn resolve_donor(explicit_root: Option<&Path>, target_root: &Path) -> Result<ResolvedDonor> {
    let Some(explicit_root) = explicit_root else {
        let target_head = GitRepo::new(target_root).head().context("reading the target's HEAD")?;
        let donor = find_donor(target_root, &target_head)?;
        let selection = DonorSelection::Automatic { changed_files: donor.changed_files };
        return Ok(ResolvedDonor { root: donor.root, given_root: donor.listed_root, head: Some(donor.head), selection });
    };
    let root = fs::canonicalize(explicit_root).with_context(|| format!("donor root {}", explicit_root.display()))?;
    ensure_worktree_root(&root, "--from")?;
    // Collecting the components drops `.` and a trailing slash, which `PathMap` roots must not have.
    let given_root = std::path::absolute(explicit_root)?.components().collect();
    let head = match GitRepo::new(&root).head() {
        Ok(head) => Some(head),
        Err(error) => {
            log::warn!("cannot read the donor's HEAD: {error:#}");
            None
        }
    };
    Ok(ResolvedDonor { root, given_root, head, selection: DonorSelection::Explicit })
}

/// A directory inside a worktree is refused, since only shards under it would be relocated, to the
/// wrong place; a directory outside any worktree, such as a copied index, is accepted.
fn ensure_worktree_root(root: &Path, option: &str) -> Result<()> {
    let Ok(toplevel) = GitRepo::new(root).toplevel() else {
        return Ok(());
    };
    let toplevel = fs::canonicalize(&toplevel).with_context(|| format!("worktree root {}", toplevel.display()))?;
    if toplevel != root {
        bail!("{option} {} is inside the git worktree {}; pass the worktree root", root.display(), toplevel.display());
    }
    Ok(())
}

/// Donor root → target root first, so an equal `--map` root cannot override it; the donor root as
/// given too when it differs from the canonical one.
fn build_path_map(donor: &ResolvedDonor, target_root: &Path, extra_mappings: &[RootMapping]) -> Result<PathMap> {
    let new_root = target_root.as_os_str().as_bytes().to_vec();
    let mut mappings = vec![RootMapping { old_root: donor.root.as_os_str().as_bytes().to_vec(), new_root: new_root.clone() }];
    if donor.given_root != donor.root {
        mappings.push(RootMapping { old_root: donor.given_root.as_os_str().as_bytes().to_vec(), new_root });
    }
    mappings.extend(extra_mappings.iter().cloned());
    PathMap::new(&mappings)
}

/// Empty, with a warning, when git cannot compare the target's working tree with the donor's `HEAD`.
fn load_deleted_files(donor_head: Option<&str>, target_root: &Path) -> HashSet<PathBuf> {
    let Some(donor_head) = donor_head else {
        return HashSet::new();
    };
    match read_deleted_files(donor_head, target_root) {
        Ok(deleted_files) => deleted_files,
        Err(error) => {
            log::warn!("cannot compare the target with the donor's HEAD {donor_head}: {error:#}");
            log::warn!("shards of deleted files are relocated too");
            HashSet::new()
        }
    }
}

fn read_deleted_files(donor_head: &str, target_root: &Path) -> Result<HashSet<PathBuf>> {
    let git = GitRepo::new(target_root);
    let toplevel = git.toplevel()?;
    let toplevel = fs::canonicalize(&toplevel).with_context(|| format!("target worktree {}", toplevel.display()))?;
    let file_changes = git.working_tree_changes(donor_head)?;
    Ok(find_deleted_files(&toplevel, &file_changes))
}

fn warn_about_index(index: &IndexRelocationReport, donor_root: &Path) {
    warn_shard_failures(index);
    warn_unmapped_index(index, donor_root);
}

fn warn_shard_failures(index: &IndexRelocationReport) {
    for failure in index.failures.iter().take(MAX_LOGGED_FAILURES) {
        log::warn!("{}: {}", failure.donor_shard.display(), failure.reason);
    }
    let unlogged = index.failures.len().saturating_sub(MAX_LOGGED_FAILURES);
    if unlogged > 0 {
        log::warn!("{unlogged} more shards failed");
    }
}

/// A relocation that changes no path leaves the target's index pointing into the donor.
fn warn_unmapped_index(index: &IndexRelocationReport, donor_root: &Path) {
    let read_shards = index.relocated + index.skipped_existing + index.skipped_deleted;
    if read_shards > 0 && index.rewritten_strings == 0 {
        log::warn!("no path in the donor's {read_shards} shards is under {}; the copies are unchanged", donor_root.display());
    }
}

/// A failure is reported instead of returned, since the shards are written by then.
fn place_cdb(request: &WorktreeRelocationRequest, donor_root: &Path, target_root: &Path, path_map: &PathMap) -> CdbOutcome {
    if request.is_relocate_cdb {
        let outcome = relocate_cdb(&CdbRelocationRequest {
            donor_root,
            target_root,
            path_map,
            is_overwrite: request.is_overwrite,
            is_dry_run: request.is_dry_run,
        })
        .unwrap_or_else(|error| CdbOutcome::Failed { reason: format!("{error:#}") });
        warn_about_cdb(&outcome, target_root);
        return outcome;
    }
    let is_target_cdb_present = find_cdb(target_root).is_some();
    if !is_target_cdb_present {
        log::warn!(
            "{} has no compile_commands.json or build/compile_commands.json, so clangd will not use the relocated index \
             until one exists; drop --no-relocate-cdb to copy the donor's",
            target_root.display()
        );
    }
    CdbOutcome::NotRequested { is_target_cdb_present }
}

fn warn_about_cdb(outcome: &CdbOutcome, target_root: &Path) {
    match outcome {
        CdbOutcome::DonorCdbMissing => {
            log::warn!("the donor has no compile_commands.json to relocate; clangd will not use the index until the target has one")
        }
        CdbOutcome::SkippedLink { link, reason } => {
            log::warn!(
                "{} {}, so the relocated compile_commands.json is not written through it{}",
                link.display(),
                describe_skip_reason(*reason),
                describe_missing_cdb(target_root)
            )
        }
        CdbOutcome::Failed { reason } => {
            log::warn!("cannot relocate compile_commands.json: {reason}{}", describe_missing_cdb(target_root))
        }
        CdbOutcome::KeptTracked(path) => {
            log::warn!("git tracks {}, so the relocated compile_commands.json does not replace it", path.display())
        }
        CdbOutcome::NotRequested { .. } | CdbOutcome::KeptExisting(_) | CdbOutcome::Written(_) => {}
    }
}

/// What a target without a compilation database means, appended to a warning; empty when it has one.
fn describe_missing_cdb(target_root: &Path) -> &'static str {
    if find_cdb(target_root).is_none() { "; clangd will not use the index until the target has one" } else { "" }
}

fn unavailable_prediction(error: &anyhow::Error) -> PredictionOutcome {
    let reason = format!("{error:#}");
    log::warn!("cannot predict what clangd reindexes: {reason}");
    PredictionOutcome::Unavailable { reason }
}

fn predict(
    request: &WorktreeRelocationRequest,
    donor_root: &Path,
    target_root: &Path,
    path_map: &PathMap,
    cdb: &CdbOutcome,
    deleted_files: &HashSet<PathBuf>,
) -> Result<TargetPrediction> {
    let nested_dirs = nested_project_dirs(target_root, target_cdb_path(cdb, target_root).as_deref());
    let mut graph = load_target_graph(request.is_dry_run, donor_root, target_root, path_map, deleted_files)?;
    for nested_dir in &nested_dirs {
        load_nested_project_nodes(&mut graph, nested_dir)?;
    }
    if let Some(user_index_dir) = &request.user_index_dir {
        let external_nodes = load_external_nodes(&graph, target_root, user_index_dir)?;
        graph.nodes.extend(external_nodes);
    }
    let main_file_source = find_main_file_source(cdb, request.is_dry_run, donor_root, target_root, path_map);
    let main_files = load_main_files(main_file_source, &graph);
    let governed_main_files = without_nested_main_files(main_files.paths, &nested_dirs);
    Ok(TargetPrediction { reindex: predict_reindex(&graph, &governed_main_files), has_cdb_main_files: main_files.is_from_cdb })
}

/// The main files of `source`, or the TUs of `graph` when there is no usable compilation database.
fn load_main_files(source: Option<MainFileSource>, graph: &IncludeGraph) -> MainFiles {
    let Some(source) = source else {
        return MainFiles { paths: graph_tus(graph), is_from_cdb: false };
    };
    match load_cdb_main_files(&source.cdb_path, source.path_map, graph_string_map_probing(graph)) {
        Ok(paths) => MainFiles { paths, is_from_cdb: true },
        Err(error) => {
            log::warn!("predicting from the TUs in the index instead: {error:#}");
            MainFiles { paths: graph_tus(graph), is_from_cdb: false }
        }
    }
}

/// The directories under `target_root` that clangd treats as projects of their own: the parents other
/// than the root of [`CDB_RELATIVE_PATHS`], such as an in-tree `build/`, that hold a compilation
/// database or will hold `target_cdb_path`.
fn nested_project_dirs(target_root: &Path, target_cdb_path: Option<&Path>) -> Vec<PathBuf> {
    let cdb_paths = CDB_RELATIVE_PATHS.iter().map(|relative_path| target_root.join(relative_path));
    let nested_cdb_paths = cdb_paths.filter(|cdb_path| cdb_path.parent() != Some(target_root));
    let present_cdb_paths = nested_cdb_paths.filter(|cdb_path| cdb_path.is_file() || Some(cdb_path.as_path()) == target_cdb_path);
    present_cdb_paths.filter_map(|cdb_path| cdb_path.parent().map(Path::to_path_buf)).collect()
}

/// After a real run the target's shards form the graph; a dry run wrote none, so the donor's shards
/// are read with paths mapped instead, leaving out those of deleted files as a real run does.
fn load_target_graph(
    is_dry_run: bool,
    donor_root: &Path,
    target_root: &Path,
    path_map: &PathMap,
    deleted_files: &HashSet<PathBuf>,
) -> Result<IncludeGraph> {
    if !is_dry_run {
        return load_include_graph(&target_root.join(INDEX_RELATIVE_DIR), &PathMap::new(&[])?);
    }
    let mut graph = load_include_graph(&donor_root.join(INDEX_RELATIVE_DIR), path_map)?;
    graph.nodes.retain(|path, _| !deleted_files.contains(path));
    Ok(graph)
}

/// The compilation database clangd will read in the target; in a dry run that writes one, the donor's
/// with paths still to map. `None` when the target has none.
fn find_main_file_source<'a>(
    cdb: &CdbOutcome,
    is_dry_run: bool,
    donor_root: &Path,
    target_root: &Path,
    path_map: &'a PathMap,
) -> Option<MainFileSource<'a>> {
    let is_unwritten_cdb = is_dry_run && matches!(cdb, CdbOutcome::Written(_));
    if is_unwritten_cdb {
        return find_cdb(donor_root)
            .map(|relative_path| MainFileSource { cdb_path: donor_root.join(relative_path), path_map: Some(path_map) });
    }
    target_cdb_path(cdb, target_root).map(|cdb_path| MainFileSource { cdb_path, path_map: None })
}

/// The compilation database the target has, or will have once a dry run's database is written.
fn target_cdb_path(cdb: &CdbOutcome, target_root: &Path) -> Option<PathBuf> {
    match cdb {
        CdbOutcome::Written(cdb_path) | CdbOutcome::KeptExisting(cdb_path) => Some(cdb_path.clone()),
        CdbOutcome::NotRequested { is_target_cdb_present: true }
        | CdbOutcome::SkippedLink { .. }
        | CdbOutcome::KeptTracked(_)
        | CdbOutcome::Failed { .. } => find_cdb(target_root).map(|relative_path| target_root.join(relative_path)),
        CdbOutcome::NotRequested { is_target_cdb_present: false } | CdbOutcome::DonorCdbMissing => None,
    }
}

impl WorktreeRelocationReport {
    pub fn summary(&self) -> RelocationSummary<'_> {
        let donor_changed_files = match self.donor_selection {
            DonorSelection::Explicit => None,
            DonorSelection::Automatic { changed_files } => Some(changed_files),
        };
        RelocationSummary {
            is_dry_run: self.is_dry_run,
            donor_root: &self.donor_root,
            donor_changed_files,
            target_root: &self.target_root,
            shards: ShardSummary {
                relocated: self.index.relocated,
                kept_existing: self.index.skipped_existing,
                skipped_deleted: self.index.skipped_deleted,
                skipped_unsupported_version: self.index.skipped_unsupported_version,
                failed: self.index.failures.len(),
            },
            cdb: summarize_cdb(&self.cdb),
            reindex: match &self.prediction {
                PredictionOutcome::Predicted(prediction) => Some(summarize_prediction(prediction)),
                PredictionOutcome::Unavailable { .. } => None,
            },
            reindex_failure: match &self.prediction {
                PredictionOutcome::Predicted(_) => None,
                PredictionOutcome::Unavailable { reason } => Some(reason),
            },
            elapsed_seconds: self.elapsed.as_secs_f64(),
        }
    }
}

fn summarize_prediction(prediction: &TargetPrediction) -> ReindexSummary<'_> {
    ReindexSummary {
        has_cdb_main_files: prediction.has_cdb_main_files,
        reindexed_tus: &prediction.reindex.reindexed_tus,
        stale_files: prediction.reindex.stale_files,
        main_files_without_shard: &prediction.reindex.main_files_without_shard,
    }
}

fn summarize_cdb(cdb: &CdbOutcome) -> CdbSummary<'_> {
    match cdb {
        CdbOutcome::NotRequested { is_target_cdb_present: true } => {
            CdbSummary { action: CdbAction::TargetHasOne, path: None, reason: None }
        }
        CdbOutcome::NotRequested { is_target_cdb_present: false } => {
            CdbSummary { action: CdbAction::TargetHasNone, path: None, reason: None }
        }
        CdbOutcome::DonorCdbMissing => CdbSummary { action: CdbAction::DonorHasNone, path: None, reason: None },
        CdbOutcome::KeptExisting(path) => CdbSummary { action: CdbAction::KeptExisting, path: Some(path), reason: None },
        CdbOutcome::Written(path) => CdbSummary { action: CdbAction::Written, path: Some(path), reason: None },
        CdbOutcome::KeptTracked(path) => CdbSummary { action: CdbAction::KeptTracked, path: Some(path), reason: Some(TRACKED_REASON) },
        CdbOutcome::SkippedLink { link, reason } => {
            CdbSummary { action: CdbAction::SkippedLink, path: Some(link), reason: Some(reason.code()) }
        }
        CdbOutcome::Failed { reason } => CdbSummary { action: CdbAction::Failed, path: None, reason: Some(reason) },
    }
}

impl fmt::Display for WorktreeRelocationReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_dry_run {
            writeln!(formatter, "dry run: nothing was written")?;
        }
        writeln!(formatter, "donor:    {}{}", self.donor_root.display(), describe_selection(self.donor_selection))?;
        writeln!(formatter, "target:   {}", self.target_root.display())?;
        writeln!(formatter, "shards:   {}", describe_shards(&self.index))?;
        writeln!(formatter, "cdb:      {}", describe_cdb(&self.cdb, self.is_dry_run))?;
        match &self.prediction {
            PredictionOutcome::Predicted(prediction) => write_prediction(formatter, prediction, &self.target_root)?,
            PredictionOutcome::Unavailable { reason } => writeln!(formatter, "reindex:  not predicted: {reason}")?,
        }
        writeln!(formatter, "elapsed:  {:.3} s", self.elapsed.as_secs_f64())
    }
}

fn describe_selection(selection: DonorSelection) -> String {
    match selection {
        DonorSelection::Explicit => String::new(),
        DonorSelection::Automatic { changed_files } => format!(" (closest worktree, {changed_files} files differ)"),
    }
}

fn describe_shards(index: &IndexRelocationReport) -> String {
    format!(
        "{} relocated, {} kept existing, {} of deleted files skipped, {} unsupported version, {} failed",
        index.relocated,
        index.skipped_existing,
        index.skipped_deleted,
        index.skipped_unsupported_version,
        index.failures.len()
    )
}

fn describe_cdb(cdb: &CdbOutcome, is_dry_run: bool) -> String {
    match cdb {
        CdbOutcome::NotRequested { is_target_cdb_present: true } => "not relocated (target has one)".to_string(),
        CdbOutcome::NotRequested { is_target_cdb_present: false } => "not relocated; target has none".to_string(),
        CdbOutcome::DonorCdbMissing => "donor has none".to_string(),
        CdbOutcome::KeptExisting(path) => format!("kept existing {}", path.display()),
        CdbOutcome::Written(path) if is_dry_run => format!("would write {}", path.display()),
        CdbOutcome::Written(path) => format!("wrote {}", path.display()),
        CdbOutcome::KeptTracked(path) => format!("kept {}, which git tracks", path.display()),
        CdbOutcome::Failed { reason } => format!("failed: {reason}"),
        CdbOutcome::SkippedLink { link, reason } => format!("not written: {} {}", link.display(), describe_skip_reason(*reason)),
    }
}

fn describe_skip_reason(reason: LinkSkipReason) -> &'static str {
    match reason {
        LinkSkipReason::PointsAtDonorCdb => "links to the donor's database",
        LinkSkipReason::PointsIntoDonor => "links into the donor worktree",
        LinkSkipReason::LeavesTarget => "links outside the target",
    }
}

fn write_prediction(formatter: &mut fmt::Formatter<'_>, prediction: &TargetPrediction, target_root: &Path) -> fmt::Result {
    let reindex = &prediction.reindex;
    let condition = if prediction.has_cdb_main_files { "" } else { " once a compile database exists" };
    writeln!(
        formatter,
        "reindex:  {} TUs predicted{condition} ({} stale files), {} CDB entries without a shard",
        reindex.reindexed_tus.len(),
        reindex.stale_files,
        reindex.main_files_without_shard.len()
    )?;
    for tu in reindex.reindexed_tus.iter().take(MAX_LISTED_TUS) {
        writeln!(formatter, "          {}", tu.strip_prefix(target_root).unwrap_or(tu).display())?;
    }
    let unlisted = reindex.reindexed_tus.len().saturating_sub(MAX_LISTED_TUS);
    if unlisted > 0 {
        writeln!(formatter, "          ... and {unlisted} more")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_without_prediction() -> WorktreeRelocationReport {
        WorktreeRelocationReport {
            donor_root: PathBuf::from("/work/main"),
            donor_selection: DonorSelection::Explicit,
            target_root: PathBuf::from("/work/wt"),
            is_dry_run: false,
            index: IndexRelocationReport {
                relocated: 3,
                skipped_existing: 0,
                skipped_deleted: 0,
                skipped_unsupported_version: 0,
                failures: Vec::new(),
                rewritten_strings: 9,
                elapsed: Duration::ZERO,
            },
            cdb: CdbOutcome::Failed { reason: "parsing compilation database".to_string() },
            prediction: PredictionOutcome::Unavailable { reason: "reading /work/wt/.cache/clangd/index".to_string() },
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn summary_reports_failed_cdb_and_unavailable_prediction() {
        let report = report_without_prediction();

        let summary = serde_json::to_value(report.summary()).unwrap();

        let expected_cdb = serde_json::json!({ "action": "failed", "path": null, "reason": "parsing compilation database" });
        assert_eq!(summary["cdb"], expected_cdb);
        assert_eq!(summary["reindex"], serde_json::Value::Null);
        assert_eq!(summary["reindex_failure"], "reading /work/wt/.cache/clangd/index");
    }

    #[test]
    fn text_summary_reports_failed_cdb_and_unavailable_prediction() {
        let text = report_without_prediction().to_string();

        assert!(text.contains("cdb:      failed: parsing compilation database\n"), "{text}");
        assert!(text.contains("reindex:  not predicted: reading /work/wt/.cache/clangd/index\n"), "{text}");
    }
}
