//! Copies the donor's compilation database (`compile_commands.json`) to the target with paths rewritten.
//!
//! clangd uses a project's index only from the directory where it found the compilation database, and
//! that file usually lives in an untracked build directory that a fresh worktree lacks.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::path_map::PathMap;
use crate::atomic_write::{WriteOutcome, write_atomically, write_unless_exists};
use crate::git::GitRepo;

/// Where clangd looks for a compilation database inside a directory, in its search order.
pub const CDB_RELATIVE_PATHS: [&str; 2] = ["compile_commands.json", "build/compile_commands.json"];
/// Symlink hops followed before a chain is treated as a loop; Linux allows 40.
const MAX_SYMLINK_DEPTH: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CdbOutcome {
    /// `--no-relocate-cdb` was given.
    NotRequested {
        is_target_cdb_present: bool,
    },
    DonorCdbMissing,
    /// The target already has a compilation database, which is kept.
    KeptExisting(PathBuf),
    /// Written to this path (or would be, in a dry run).
    Written(PathBuf),
    /// Not written, since git tracks the file at this path, which the write would modify.
    KeptTracked(PathBuf),
    /// Not written, since `link` leads where this tool must not write; the link is kept as it is.
    SkippedLink {
        link: PathBuf,
        reason: LinkSkipReason,
    },
    /// Relocating failed, such as on invalid donor JSON; the target's database is as it was.
    Failed {
        reason: String,
    },
}

/// Why the target's database path is not written through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkSkipReason {
    /// It leads to the donor's own database, which the write would replace.
    PointsAtDonorCdb,
    /// It leads into the donor worktree, which is nested in the target, such as under `.claude/worktrees`.
    PointsIntoDonor,
    /// It leads outside the target root, or to where that cannot be told, such as through a dangling
    /// directory symlink.
    LeavesTarget,
}

impl LinkSkipReason {
    /// The reason as `relocate --json` prints it.
    pub fn code(self) -> &'static str {
        match self {
            Self::PointsAtDonorCdb => "points_at_donor_cdb",
            Self::PointsIntoDonor => "points_into_donor",
            Self::LeavesTarget => "leaves_target",
        }
    }
}

pub struct CdbRelocationRequest<'a> {
    pub donor_root: &'a Path,
    pub target_root: &'a Path,
    pub path_map: &'a PathMap,
    pub is_overwrite: bool,
    pub is_dry_run: bool,
}

/// The first of [`CDB_RELATIVE_PATHS`] under `root` that is a readable file, through any symlinks.
pub fn find_cdb(root: &Path) -> Option<&'static str> {
    CDB_RELATIVE_PATHS.into_iter().find(|relative_path| is_readable_file(&root.join(relative_path)))
}

fn is_readable_file(path: &Path) -> bool {
    fs::File::open(path).and_then(|file| file.metadata()).is_ok_and(|metadata| metadata.is_file())
}

/// Writes the rewritten donor database to the same relative path in the target; a readable target
/// database at any clangd location is kept unless `is_overwrite`.
///
/// When that path is a symlink, often a tracked one into the build directory, the database is written
/// where the symlink points, so the symlink stays and a dangling one starts to resolve. A path that
/// leads to the donor's database, into the donor or outside the target is not written at all, nor is a
/// file git tracks, which the write would modify.
pub fn relocate_cdb(request: &CdbRelocationRequest) -> Result<CdbOutcome> {
    let Some(donor_relative_path) = find_cdb(request.donor_root) else {
        return Ok(CdbOutcome::DonorCdbMissing);
    };
    if let Some(existing_relative_path) = find_cdb(request.target_root)
        && !request.is_overwrite
    {
        return Ok(CdbOutcome::KeptExisting(request.target_root.join(existing_relative_path)));
    }

    let donor_cdb = request.donor_root.join(donor_relative_path);
    let target_link = request.target_root.join(donor_relative_path);
    let target_cdb = resolve_symlinks(&target_link)?;
    if let Some(reason) = find_link_skip_reason(&target_cdb, &donor_cdb, request)? {
        return Ok(CdbOutcome::SkippedLink { link: target_link, reason });
    }
    if is_tracked_in_target(&target_cdb, request.target_root)? {
        return Ok(CdbOutcome::KeptTracked(target_cdb));
    }

    let json = fs::read(&donor_cdb).with_context(|| format!("reading {}", donor_cdb.display()))?;
    let rewritten = rewrite_cdb(&json, request.path_map).with_context(|| format!("rewriting {}", donor_cdb.display()))?;
    if rewritten.rewritten_strings == 0 {
        log::warn!("{}: no path is under a mapped donor root; the copy is unchanged", donor_cdb.display());
    }
    if request.is_dry_run {
        return Ok(CdbOutcome::Written(target_cdb));
    }
    write_cdb(&target_cdb, &rewritten.bytes, request.is_overwrite)
}

pub struct RewrittenCdb {
    pub bytes: Vec<u8>,
    /// JSON strings the path map changed.
    pub rewritten_strings: usize,
}

/// Rewrites every JSON string value (`directory`, `file`, `command`, `arguments`, `output`) through
/// `path_map`; object keys and other values stay as they are.
pub fn rewrite_cdb(json: &[u8], path_map: &PathMap) -> Result<RewrittenCdb> {
    let mut cdb: Value = serde_json::from_slice(json).context("parsing compilation database")?;
    let rewritten_strings = rewrite_json_strings(&mut cdb, path_map)?;
    let mut bytes = serde_json::to_vec_pretty(&cdb)?;
    bytes.push(b'\n');
    Ok(RewrittenCdb { bytes, rewritten_strings })
}

/// Returns how many strings changed.
fn rewrite_json_strings(value: &mut Value, path_map: &PathMap) -> Result<usize> {
    match value {
        Value::String(text) => {
            let Some(rewritten) = path_map.rewrite(text.as_bytes()) else {
                return Ok(0);
            };
            *text = String::from_utf8(rewritten).context("rewritten string is not UTF-8")?;
            Ok(1)
        }
        Value::Array(items) => items.iter_mut().map(|item| rewrite_json_strings(item, path_map)).sum(),
        Value::Object(fields) => fields.values_mut().map(|field| rewrite_json_strings(field, path_map)).sum(),
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(0),
    }
}

/// The path `path` finally points to when it is a symlink, resolving each relative link against the
/// directory holding it; `path` itself otherwise. The final path need not exist.
fn resolve_symlinks(path: &Path) -> Result<PathBuf> {
    let mut resolved = path.to_path_buf();
    // One more check than hops, since the path after the last allowed hop must still be looked at.
    for _ in 0..=MAX_SYMLINK_DEPTH {
        let is_symlink = fs::symlink_metadata(&resolved).is_ok_and(|metadata| metadata.file_type().is_symlink());
        if !is_symlink {
            return Ok(resolved);
        }
        let link_target = fs::read_link(&resolved).with_context(|| format!("reading symlink {}", resolved.display()))?;
        let link_dir = resolved.parent().context("symlink path has no parent")?;
        resolved = link_dir.join(link_target);
    }
    bail!("{}: more than {MAX_SYMLINK_DEPTH} levels of symlinks", path.display())
}

/// `None` when `target_cdb`, the resolved write path, is inside the target and not inside the donor; a
/// donor nested in the target is the closer root of its files, a target nested in the donor of its own.
fn find_link_skip_reason(target_cdb: &Path, donor_cdb: &Path, request: &CdbRelocationRequest) -> Result<Option<LinkSkipReason>> {
    let Some(real_target_cdb) = resolve_real_path(target_cdb) else {
        return Ok(Some(LinkSkipReason::LeavesTarget));
    };
    let real_donor_cdb = fs::canonicalize(donor_cdb).with_context(|| format!("resolving {}", donor_cdb.display()))?;
    if real_target_cdb == real_donor_cdb {
        return Ok(Some(LinkSkipReason::PointsAtDonorCdb));
    }
    let real_target_root = canonicalize_root(request.target_root)?;
    if !real_target_cdb.starts_with(&real_target_root) {
        return Ok(Some(LinkSkipReason::LeavesTarget));
    }
    let real_donor_root = canonicalize_root(request.donor_root)?;
    let is_donor_nested_in_target = real_donor_root.starts_with(&real_target_root);
    if is_donor_nested_in_target && real_target_cdb.starts_with(&real_donor_root) {
        return Ok(Some(LinkSkipReason::PointsIntoDonor));
    }
    Ok(None)
}

/// A target outside git counts as untracked; any other git failure is an error, so the file is not
/// overwritten.
fn is_tracked_in_target(target_cdb: &Path, target_root: &Path) -> Result<bool> {
    let git = GitRepo::new(target_root);
    if !git.is_in_repository()? {
        log::debug!("{} is not in a git repository, so nothing in it is tracked", target_root.display());
        return Ok(false);
    }
    git.is_tracked(target_cdb)
}

fn canonicalize_root(root: &Path) -> Result<PathBuf> {
    fs::canonicalize(root).with_context(|| format!("resolving {}", root.display()))
}

/// `path` with every symlink resolved, though its last components need not exist: the deepest existing
/// ancestor is canonicalized and the missing names are appended. `None` when a missing part is a
/// dangling symlink or `..`, so where it leads cannot be told.
fn resolve_real_path(path: &Path) -> Option<PathBuf> {
    let mut missing_names: Vec<&OsStr> = Vec::new();
    let mut ancestor = path;
    loop {
        if let Ok(real_ancestor) = fs::canonicalize(ancestor) {
            return Some(missing_names.iter().rev().fold(real_ancestor, |real_path, name| real_path.join(name)));
        }
        let is_present_but_unresolvable = fs::symlink_metadata(ancestor).is_ok();
        if is_present_but_unresolvable {
            return None;
        }
        missing_names.push(ancestor.file_name()?);
        ancestor = ancestor.parent()?;
    }
}

fn write_cdb(target_cdb: &Path, bytes: &[u8], is_overwrite: bool) -> Result<CdbOutcome> {
    let parent = target_cdb.parent().context("compilation database path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    if is_overwrite {
        write_atomically(target_cdb, bytes, None)?;
        return Ok(CdbOutcome::Written(target_cdb.to_path_buf()));
    }
    match write_unless_exists(target_cdb, bytes)? {
        WriteOutcome::Written => Ok(CdbOutcome::Written(target_cdb.to_path_buf())),
        WriteOutcome::TargetExists => Ok(CdbOutcome::KeptExisting(target_cdb.to_path_buf())),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::relocate::path_map::RootMapping;

    fn path_map() -> PathMap {
        PathMap::new(&[RootMapping { old_root: b"/x/proj".to_vec(), new_root: b"/y/wt".to_vec() }]).unwrap()
    }

    fn rewrite(cdb: &Value) -> Value {
        let rewritten = rewrite_cdb(&serde_json::to_vec(cdb).unwrap(), &path_map()).unwrap();
        serde_json::from_slice(&rewritten.bytes).unwrap()
    }

    fn write_donor_cdb(donor_root: &Path) {
        let cdb = json!([{ "directory": "/x/proj/build", "file": "/x/proj/src/a.cpp", "command": "c++ -c /x/proj/src/a.cpp" }]);
        fs::create_dir_all(donor_root.join("build")).unwrap();
        fs::write(donor_root.join("build/compile_commands.json"), serde_json::to_vec(&cdb).unwrap()).unwrap();
    }

    /// The root `compile_commands.json` of `root` as a relative symlink into `build/`, as a tracked one is.
    fn link_root_cdb_into_build(root: &Path) -> PathBuf {
        let root_link = root.join("compile_commands.json");
        fs::create_dir_all(root).unwrap();
        std::os::unix::fs::symlink("build/compile_commands.json", &root_link).unwrap();
        root_link
    }

    #[test]
    fn rewrites_arguments_form() {
        let cdb = json!([{
            "directory": "/x/proj/build",
            "file": "/x/proj/src/a.cpp",
            "arguments": ["c++", "-I/x/proj/include", "-I/x/proj-other/include", "-c", "/x/proj/src/a.cpp"],
            "output": "/x/proj/build/a.o"
        }]);

        let expected = json!([{
            "directory": "/y/wt/build",
            "file": "/y/wt/src/a.cpp",
            "arguments": ["c++", "-I/y/wt/include", "-I/x/proj-other/include", "-c", "/y/wt/src/a.cpp"],
            "output": "/y/wt/build/a.o"
        }]);
        assert_eq!(rewrite(&cdb), expected);
    }

    #[test]
    fn rewrites_command_form() {
        let cdb = json!([{
            "directory": "/x/proj/build",
            "file": "../src/a.cpp",
            "command": "c++ -DROOT=\"/x/proj\" -I/x/proj/include -c ../src/a.cpp"
        }]);

        let expected = json!([{
            "directory": "/y/wt/build",
            "file": "../src/a.cpp",
            "command": "c++ -DROOT=\"/y/wt\" -I/y/wt/include -c ../src/a.cpp"
        }]);
        assert_eq!(rewrite(&cdb), expected);
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(rewrite_cdb(b"[{", &path_map()).is_err());
    }

    #[test]
    fn counts_rewritten_strings() {
        let cdb = json!([{ "directory": "/x/proj/build", "file": "a.cpp", "arguments": ["c++", "-I/x/proj/include"] }]);

        let rewritten = rewrite_cdb(&serde_json::to_vec(&cdb).unwrap(), &path_map()).unwrap();

        assert_eq!(rewritten.rewritten_strings, 2);
    }

    #[test]
    fn writes_through_dangling_symlink_and_keeps_it() {
        let temp = tempfile::tempdir().unwrap();
        let donor_root = temp.path().join("donor");
        let target_root = temp.path().join("target");
        write_donor_cdb(&donor_root);
        link_root_cdb_into_build(&donor_root);
        let target_link = link_root_cdb_into_build(&target_root);
        let path_map = path_map();
        let request = CdbRelocationRequest {
            donor_root: &donor_root,
            target_root: &target_root,
            path_map: &path_map,
            is_overwrite: true,
            is_dry_run: false,
        };

        let outcome = relocate_cdb(&request).unwrap();

        let written = target_root.join("build/compile_commands.json");
        assert_eq!(outcome, CdbOutcome::Written(written.clone()));
        assert!(fs::symlink_metadata(&target_link).unwrap().file_type().is_symlink());
        assert!(fs::read_to_string(&written).unwrap().contains("/y/wt/src/a.cpp"));
    }

    /// Relocates with overwriting into `target_root`, whose `build/compile_commands.json` is a symlink
    /// to `link_target`.
    fn relocate_through_link(donor_root: &Path, target_root: &Path, link_target: &Path) -> (PathBuf, CdbOutcome) {
        let target_link = target_root.join("build/compile_commands.json");
        fs::create_dir_all(target_link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(link_target, &target_link).unwrap();
        let path_map = path_map();
        let request = CdbRelocationRequest { donor_root, target_root, path_map: &path_map, is_overwrite: true, is_dry_run: false };

        (target_link, relocate_cdb(&request).unwrap())
    }

    /// Relocates into `target_root`, whose root `compile_commands.json` reaches the dangling
    /// `build/compile_commands.json` through a chain of `hops` relative symlinks.
    fn relocate_through_link_chain(temp: &Path, hops: usize) -> Result<CdbOutcome> {
        let donor_root = temp.join("donor");
        let target_root = temp.join("target");
        write_donor_cdb(&donor_root);
        link_root_cdb_into_build(&donor_root);
        fs::create_dir_all(&target_root).unwrap();
        let link_names: Vec<String> =
            std::iter::once("compile_commands.json".to_string()).chain((1..hops).map(|hop| format!("link-{hop}"))).collect();
        let link_targets = link_names.iter().skip(1).map(String::as_str).chain(["build/compile_commands.json"]);
        for (link_name, link_target) in link_names.iter().zip(link_targets) {
            std::os::unix::fs::symlink(link_target, target_root.join(link_name)).unwrap();
        }
        let path_map = path_map();

        relocate_cdb(&CdbRelocationRequest {
            donor_root: &donor_root,
            target_root: &target_root,
            path_map: &path_map,
            is_overwrite: true,
            is_dry_run: false,
        })
    }

    #[test]
    fn follows_as_many_symlink_hops_as_linux() {
        let temp = tempfile::tempdir().unwrap();

        let outcome = relocate_through_link_chain(temp.path(), MAX_SYMLINK_DEPTH).unwrap();

        assert_eq!(outcome, CdbOutcome::Written(temp.path().join("target/build/compile_commands.json")));
    }

    #[test]
    fn rejects_symlink_chain_beyond_hop_limit() {
        let temp = tempfile::tempdir().unwrap();

        let error = relocate_through_link_chain(temp.path(), MAX_SYMLINK_DEPTH + 1).unwrap_err();

        assert!(error.to_string().contains("levels of symlinks"), "{error:#}");
        assert!(!temp.path().join("target/build").exists());
    }

    #[test]
    fn fails_without_writing_when_git_cannot_read_the_target_index() {
        let temp = tempfile::tempdir().unwrap();
        let donor_root = temp.path().join("donor");
        let target_root = temp.path().join("target");
        write_donor_cdb(&donor_root);
        fs::create_dir_all(&target_root).unwrap();
        let git_init = std::process::Command::new("git").args(["init", "--quiet"]).current_dir(&target_root).status().unwrap();
        assert!(git_init.success());
        fs::write(target_root.join(".git/index"), "corrupt").unwrap();
        let path_map = path_map();
        let request = CdbRelocationRequest {
            donor_root: &donor_root,
            target_root: &target_root,
            path_map: &path_map,
            is_overwrite: true,
            is_dry_run: false,
        };

        let error = relocate_cdb(&request).unwrap_err();

        assert!(error.to_string().contains("ls-files"), "{error:#}");
        assert!(!target_root.join("build/compile_commands.json").exists());
    }

    #[test]
    fn skips_link_to_donor_cdb() {
        let temp = tempfile::tempdir().unwrap();
        let donor_root = temp.path().join("donor");
        let target_root = temp.path().join("target");
        write_donor_cdb(&donor_root);
        let donor_cdb = donor_root.join("build/compile_commands.json");
        let donor_json = fs::read(&donor_cdb).unwrap();

        let (target_link, outcome) = relocate_through_link(&donor_root, &target_root, &donor_cdb);

        assert_eq!(outcome, CdbOutcome::SkippedLink { link: target_link.clone(), reason: LinkSkipReason::PointsAtDonorCdb });
        assert_eq!(fs::read(&donor_cdb).unwrap(), donor_json);
        assert!(fs::symlink_metadata(&target_link).unwrap().file_type().is_symlink());
    }

    #[test]
    fn skips_link_that_leaves_target() {
        let temp = tempfile::tempdir().unwrap();
        let donor_root = temp.path().join("donor");
        let target_root = temp.path().join("target");
        let outside_dir = temp.path().join("outside");
        write_donor_cdb(&donor_root);
        fs::create_dir_all(&outside_dir).unwrap();

        let link_target = Path::new("../../outside/compile_commands.json");
        let (target_link, outcome) = relocate_through_link(&donor_root, &target_root, link_target);

        assert_eq!(outcome, CdbOutcome::SkippedLink { link: target_link, reason: LinkSkipReason::LeavesTarget });
        assert_eq!(fs::read_dir(&outside_dir).unwrap().count(), 0);
    }

    /// Relocates from a donor at `donor_relative_root` under the target, whose database is its root
    /// `compile_commands.json`, into the target, whose root `compile_commands.json` links to `link_target`.
    fn relocate_from_nested_donor(target_root: &Path, donor_relative_root: &str, link_target: &str) -> (PathBuf, CdbOutcome) {
        let donor_root = target_root.join(donor_relative_root);
        write_donor_cdb(&donor_root);
        fs::rename(donor_root.join("build/compile_commands.json"), donor_root.join("compile_commands.json")).unwrap();
        let target_link = target_root.join("compile_commands.json");
        std::os::unix::fs::symlink(link_target, &target_link).unwrap();
        let path_map = path_map();
        let request =
            CdbRelocationRequest { donor_root: &donor_root, target_root, path_map: &path_map, is_overwrite: true, is_dry_run: false };

        (target_link, relocate_cdb(&request).unwrap())
    }

    #[test]
    fn skips_link_into_donor_nested_in_target() {
        let temp = tempfile::tempdir().unwrap();
        let target_root = temp.path().join("target");

        let (target_link, outcome) =
            relocate_from_nested_donor(&target_root, ".claude/worktrees/x", ".claude/worktrees/x/build/compile_commands.json");

        assert_eq!(outcome, CdbOutcome::SkippedLink { link: target_link, reason: LinkSkipReason::PointsIntoDonor });
        assert!(!target_root.join(".claude/worktrees/x/build/compile_commands.json").exists());
    }

    #[test]
    fn writes_target_cdb_outside_nested_donor() {
        let temp = tempfile::tempdir().unwrap();
        let target_root = temp.path().join("target");

        let (_, outcome) = relocate_from_nested_donor(&target_root, ".claude/worktrees/x", "build/compile_commands.json");

        assert_eq!(outcome, CdbOutcome::Written(target_root.join("build/compile_commands.json")));
    }

    #[test]
    fn writes_into_target_nested_in_donor() {
        let temp = tempfile::tempdir().unwrap();
        let donor_root = temp.path().join("donor");
        let target_root = donor_root.join(".claude/worktrees/y");
        write_donor_cdb(&donor_root);

        let (_, outcome) = relocate_through_link(&donor_root, &target_root, Path::new("cdb.json"));

        assert_eq!(outcome, CdbOutcome::Written(target_root.join("build/cdb.json")));
    }
}
