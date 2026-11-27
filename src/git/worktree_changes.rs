//! Files the donor's `HEAD` has and the target worktree lacks, as absolute target paths.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::porcelain::{FileChange, FileChangeKind};

/// `changes` hold repository-relative paths, resolved against `target_root`.
///
/// A deleted change counts only if nothing exists at the path: a file removed only from the index
/// (`git rm --cached`) shows as deleted, even when ignored, yet still exists. A dangling symlink exists too.
pub fn find_deleted_files(target_root: &Path, changes: &[FileChange]) -> HashSet<PathBuf> {
    changes
        .iter()
        .filter(|change| change.kind == FileChangeKind::Deleted)
        .map(|change| target_root.join(&change.path))
        .filter(|path| fs::symlink_metadata(path).is_err())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn change(kind: FileChangeKind, path: &str) -> FileChange {
        FileChange { kind, path: PathBuf::from(path) }
    }

    #[test]
    fn finds_deleted_files_as_absolute_target_paths() {
        let target = TempDir::new().unwrap();
        let changes = [
            change(FileChangeKind::Modified, "src/a.cpp"),
            change(FileChangeKind::Deleted, "include/old file.h"),
            change(FileChangeKind::Added, "include/new.h"),
        ];

        let deleted = find_deleted_files(target.path(), &changes);

        assert_eq!(deleted, HashSet::from([target.path().join("include/old file.h")]));
    }

    #[test]
    fn keeps_file_removed_only_from_index() {
        let target = TempDir::new().unwrap();
        fs::write(target.path().join("kept.cpp"), "").unwrap();
        let changes = [change(FileChangeKind::Deleted, "kept.cpp")];

        assert!(find_deleted_files(target.path(), &changes).is_empty());
    }

    #[test]
    fn keeps_dangling_symlink() {
        let target = TempDir::new().unwrap();
        std::os::unix::fs::symlink("missing.h", target.path().join("link.h")).unwrap();
        let changes = [change(FileChangeKind::Deleted, "link.h")];

        assert!(find_deleted_files(target.path(), &changes).is_empty());
    }
}
