//! Files the donor's `HEAD` has and the target worktree lacks, as absolute target paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::porcelain::{FileChange, FileChangeKind};

/// `changes` and `untracked` hold repository-relative paths, resolved against `target_root`.
///
/// A file removed only from the index (`git rm --cached`) shows as deleted and as untracked; it still
/// exists, so it is not deleted.
pub fn find_deleted_files(target_root: &Path, changes: &[FileChange], untracked: &[PathBuf]) -> HashSet<PathBuf> {
    let untracked: HashSet<&Path> = untracked.iter().map(PathBuf::as_path).collect();
    changes
        .iter()
        .filter(|change| change.kind == FileChangeKind::Deleted && !untracked.contains(change.path.as_path()))
        .map(|change| target_root.join(&change.path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(kind: FileChangeKind, path: &str) -> FileChange {
        FileChange { kind, path: PathBuf::from(path) }
    }

    #[test]
    fn finds_deleted_files_as_absolute_target_paths() {
        let changes = [
            change(FileChangeKind::Modified, "src/a.cpp"),
            change(FileChangeKind::Deleted, "include/old file.h"),
            change(FileChangeKind::Added, "include/new.h"),
        ];

        let deleted = find_deleted_files(Path::new("/wt"), &changes, &[]);

        assert_eq!(deleted, HashSet::from([PathBuf::from("/wt/include/old file.h")]));
    }

    #[test]
    fn keeps_file_removed_only_from_index() {
        let changes = [change(FileChangeKind::Deleted, "src/kept.cpp")];
        let untracked = [PathBuf::from("src/kept.cpp")];

        assert!(find_deleted_files(Path::new("/wt"), &changes, &untracked).is_empty());
    }
}
