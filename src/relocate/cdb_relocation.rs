//! Copies the donor's compilation database (`compile_commands.json`) to the target with paths rewritten.
//!
//! clangd uses a project's index only from the directory where it found the compilation database, and
//! that file usually lives in an untracked build directory that a fresh worktree lacks.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::path_map::PathMap;
use crate::atomic_write::{WriteOutcome, write_atomically, write_unless_exists};

/// Where clangd looks for a compilation database inside a directory, in its search order.
pub const CDB_RELATIVE_PATHS: [&str; 2] = ["compile_commands.json", "build/compile_commands.json"];
/// Symlink hops followed before a chain is treated as a loop; Linux allows 40.
const MAX_SYMLINK_DEPTH: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CdbOutcome {
    /// `--relocate-cdb` was not given.
    NotRequested {
        is_target_cdb_present: bool,
    },
    DonorCdbMissing,
    /// The target already has a compilation database, which is kept.
    KeptExisting(PathBuf),
    /// Written to this path (or would be, in a dry run).
    Written(PathBuf),
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
/// where the symlink points, so the symlink stays and a dangling one starts to resolve.
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
    let json = fs::read(&donor_cdb).with_context(|| format!("reading {}", donor_cdb.display()))?;
    let rewritten = rewrite_cdb(&json, request.path_map).with_context(|| format!("rewriting {}", donor_cdb.display()))?;
    if rewritten.rewritten_strings == 0 {
        log::warn!("{}: no path is under a mapped donor root; the copy is unchanged", donor_cdb.display());
    }
    let target_cdb = resolve_symlinks(&request.target_root.join(donor_relative_path))?;
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
    for _ in 0..MAX_SYMLINK_DEPTH {
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
        fs::create_dir_all(&target_root).unwrap();
        let target_link = target_root.join("compile_commands.json");
        std::os::unix::fs::symlink("build/compile_commands.json", &target_link).unwrap();
        let path_map = path_map();
        let request = CdbRelocationRequest {
            donor_root: &donor_root,
            target_root: &target_root,
            path_map: &path_map,
            is_overwrite: false,
            is_dry_run: false,
        };

        let outcome = relocate_cdb(&request).unwrap();

        let written = target_root.join("build/compile_commands.json");
        assert_eq!(outcome, CdbOutcome::Written(written.clone()));
        assert!(fs::symlink_metadata(&target_link).unwrap().file_type().is_symlink());
        assert!(fs::read_to_string(&written).unwrap().contains("/y/wt/src/a.cpp"));
    }
}
