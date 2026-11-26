//! The `relocate` command: relocates the index of a donor worktree into a target worktree, using git
//! to choose the donor and skip deleted files.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::cdb_relocation::{CdbOutcome, CdbRelocationRequest, find_cdb, relocate_cdb};
use super::donor_selection::find_donor;
use super::index_relocation::{INDEX_RELATIVE_DIR, IndexRelocationReport, IndexRelocationRequest, relocate_index};
use super::path_map::{PathMap, RootMapping};
use crate::format::string_table::TableCompression;
use crate::git::GitRepo;
use crate::git::worktree_changes::find_deleted_files;

/// Shard failures logged one by one; the rest are only counted.
const MAX_LOGGED_FAILURES: usize = 10;

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
    pub elapsed: Duration,
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
        deleted_target_paths: deleted_files,
    })?;
    warn_about_index(&index, &donor.root);
    let cdb = place_cdb(request, &donor.root, &target_root, &path_map)?;

    Ok(WorktreeRelocationReport {
        donor_root: donor.root,
        donor_selection: donor.selection,
        target_root,
        is_dry_run: request.is_dry_run,
        index,
        cdb,
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
    let untracked = git.untracked_files()?;
    Ok(find_deleted_files(&toplevel, &file_changes, &untracked))
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

fn place_cdb(request: &WorktreeRelocationRequest, donor_root: &Path, target_root: &Path, path_map: &PathMap) -> Result<CdbOutcome> {
    if request.is_relocate_cdb {
        let outcome = relocate_cdb(&CdbRelocationRequest {
            donor_root,
            target_root,
            path_map,
            is_overwrite: request.is_overwrite,
            is_dry_run: request.is_dry_run,
        })?;
        if outcome == CdbOutcome::DonorCdbMissing {
            log::warn!("the donor has no compile_commands.json to relocate; clangd will not use the index until the target has one");
        }
        return Ok(outcome);
    }
    let is_target_cdb_present = find_cdb(target_root).is_some();
    if !is_target_cdb_present {
        log::warn!(
            "{} has no compile_commands.json or build/compile_commands.json, so clangd will not use the relocated index \
             until one exists; pass --relocate-cdb to copy the donor's",
            target_root.display()
        );
    }
    Ok(CdbOutcome::NotRequested { is_target_cdb_present })
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
    }
}
