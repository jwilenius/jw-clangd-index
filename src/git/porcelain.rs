//! Parsers of git's NUL-separated (`-z`) machine-readable output.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const FIELD_SEPARATOR: u8 = 0;
const WORKTREE_PREFIX: &[u8] = b"worktree ";
const ATTRIBUTE_SEPARATOR: u8 = b' ';

/// One record of `git worktree list --porcelain -z`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    /// `None` for a bare repository.
    pub head: Option<String>,
    pub is_bare: bool,
    /// The worktree directory is gone; `git worktree prune` would remove the entry.
    pub is_prunable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChangeKind {
    Added,
    Deleted,
    /// Content, type or merge state changed.
    Modified,
}

/// One entry of `git diff --name-status -z --no-renames`; `path` is repository-relative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub kind: FileChangeKind,
    pub path: PathBuf,
}

/// Records are lists of NUL-terminated attribute lines, each record ended by an empty line.
pub fn parse_worktree_list(output: &[u8]) -> Result<Vec<WorktreeEntry>> {
    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;
    for field in output.split(|&byte| byte == FIELD_SEPARATOR) {
        if field.is_empty() {
            entries.extend(current.take());
            continue;
        }
        if let Some(path) = field.strip_prefix(WORKTREE_PREFIX) {
            entries.extend(current.replace(new_worktree_entry(path)));
            continue;
        }
        let entry = current.as_mut().context("worktree attribute before its `worktree` line")?;
        apply_worktree_attribute(entry, field);
    }
    entries.extend(current);
    Ok(entries)
}

fn new_worktree_entry(path: &[u8]) -> WorktreeEntry {
    WorktreeEntry { path: bytes_to_path(path), head: None, is_bare: false, is_prunable: false }
}

/// Attributes this tool does not need (`branch`, `detached`, `locked`) are ignored.
fn apply_worktree_attribute(entry: &mut WorktreeEntry, field: &[u8]) {
    let mut parts = field.splitn(2, |&byte| byte == ATTRIBUTE_SEPARATOR);
    let name = parts.next().unwrap_or_default();
    let value = parts.next();
    match name {
        b"HEAD" => entry.head = value.map(|oid| String::from_utf8_lossy(oid).into_owned()),
        b"bare" => entry.is_bare = true,
        b"prunable" => entry.is_prunable = true,
        _ => {}
    }
}

/// Alternating status and path fields; `--no-renames` rules out the three-field rename records.
pub fn parse_name_status(output: &[u8]) -> Result<Vec<FileChange>> {
    let fields: Vec<&[u8]> = output.split(|&byte| byte == FIELD_SEPARATOR).filter(|field| !field.is_empty()).collect();
    fields
        .chunks(2)
        .map(|record| match record {
            [status, path] => Ok(FileChange { kind: parse_change_kind(status)?, path: bytes_to_path(path) }),
            _ => bail!("name-status output ends without a path"),
        })
        .collect()
}

fn parse_change_kind(status: &[u8]) -> Result<FileChangeKind> {
    match status.first() {
        Some(b'A') => Ok(FileChangeKind::Added),
        Some(b'D') => Ok(FileChangeKind::Deleted),
        Some(b'M' | b'T' | b'U') => Ok(FileChangeKind::Modified),
        _ => bail!("unexpected diff status {:?}", String::from_utf8_lossy(status)),
    }
}

/// NUL-terminated paths, as `--name-only -z` and `ls-files -z` print them.
pub fn parse_path_list(output: &[u8]) -> Vec<PathBuf> {
    output.split(|&byte| byte == FIELD_SEPARATOR).filter(|field| !field.is_empty()).map(bytes_to_path).collect()
}

fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree(path: &str, head: Option<&str>) -> WorktreeEntry {
        WorktreeEntry { path: PathBuf::from(path), head: head.map(str::to_string), is_bare: false, is_prunable: false }
    }

    #[test]
    fn parses_worktree_records_with_space_in_path_and_detached_head() {
        let output = b"worktree /x/main\0HEAD aaaa\0branch refs/heads/main\0\0\
                       worktree /x/my wt\0HEAD bbbb\0detached\0\0";

        let entries = parse_worktree_list(output).unwrap();

        assert_eq!(entries, vec![worktree("/x/main", Some("aaaa")), worktree("/x/my wt", Some("bbbb"))]);
    }

    #[test]
    fn parses_bare_and_prunable_and_locked_entries() {
        let output = b"worktree /x/repo.git\0bare\0\0\
                       worktree /x/gone\0HEAD cccc\0branch refs/heads/gone\0prunable gitdir file points to non-existent location\0\0\
                       worktree /x/locked\0HEAD dddd\0detached\0locked reason\0\0";

        let entries = parse_worktree_list(output).unwrap();

        let expected = vec![
            WorktreeEntry { is_bare: true, ..worktree("/x/repo.git", None) },
            WorktreeEntry { is_prunable: true, ..worktree("/x/gone", Some("cccc")) },
            worktree("/x/locked", Some("dddd")),
        ];
        assert_eq!(entries, expected);
    }

    #[test]
    fn keeps_last_record_without_trailing_empty_line() {
        let entries = parse_worktree_list(b"worktree /x/main\0HEAD aaaa").unwrap();

        assert_eq!(entries, vec![worktree("/x/main", Some("aaaa"))]);
    }

    #[test]
    fn rejects_attribute_before_worktree_line() {
        assert!(parse_worktree_list(b"HEAD aaaa\0\0").is_err());
    }

    #[test]
    fn parses_name_status_records() {
        let output = b"M\0src/a b.cpp\0D\0include/old.h\0A\0include/new.h\0T\0link\0";

        let changes = parse_name_status(output).unwrap();

        let change = |kind, path: &str| FileChange { kind, path: PathBuf::from(path) };
        let expected = vec![
            change(FileChangeKind::Modified, "src/a b.cpp"),
            change(FileChangeKind::Deleted, "include/old.h"),
            change(FileChangeKind::Added, "include/new.h"),
            change(FileChangeKind::Modified, "link"),
        ];
        assert_eq!(changes, expected);
    }

    #[test]
    fn rejects_rename_and_truncated_name_status() {
        assert!(parse_name_status(b"R100\0old\0new\0").is_err());
        assert!(parse_name_status(b"M\0").is_err());
    }

    #[test]
    fn parses_path_list() {
        assert_eq!(parse_path_list(b"a b.h\0dir/c.cpp\0"), vec![PathBuf::from("a b.h"), PathBuf::from("dir/c.cpp")]);
        assert_eq!(parse_path_list(b""), Vec::<PathBuf>::new());
    }
}
