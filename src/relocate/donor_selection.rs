//! Chooses the donor worktree automatically: the indexed worktree closest to the target.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::index_relocation::{INDEX_RELATIVE_DIR, list_shards};
use crate::git::GitRepo;
use crate::git::porcelain::WorktreeEntry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DonorCandidate {
    /// Canonical worktree root.
    pub root: PathBuf,
    /// The root as `git worktree list` prints it, which may go through a symlink.
    pub listed_root: PathBuf,
    pub head: String,
    /// Files that differ between the candidate's and the target's `HEAD`.
    pub changed_files: usize,
    /// Listed first by `git worktree list`.
    pub is_main_worktree: bool,
}

/// The worktree of `target_root`'s repository that holds a clangd index and whose `HEAD` differs from
/// `target_head` in the fewest files.
pub fn find_donor(target_root: &Path, target_head: &str) -> Result<DonorCandidate> {
    let git = GitRepo::new(target_root);
    let mut candidates = Vec::new();
    for (position, entry) in git.worktrees()?.iter().enumerate() {
        let is_main_worktree = position == 0;
        if let Some(candidate) = inspect_worktree(&git, entry, target_root, target_head, is_main_worktree) {
            candidates.push(candidate);
        }
    }
    let Some(donor) = choose_donor(candidates) else {
        bail!("no other worktree of {} has a clangd index in {INDEX_RELATIVE_DIR}; pass --from DONOR_ROOT", target_root.display());
    };
    Ok(donor)
}

/// Fewest changed files wins; ties go to the main worktree, then to the smallest path.
pub fn choose_donor(candidates: Vec<DonorCandidate>) -> Option<DonorCandidate> {
    candidates.into_iter().min_by(compare_donors)
}

fn compare_donors(left: &DonorCandidate, right: &DonorCandidate) -> Ordering {
    left.changed_files
        .cmp(&right.changed_files)
        .then_with(|| right.is_main_worktree.cmp(&left.is_main_worktree))
        .then_with(|| left.root.cmp(&right.root))
}

/// `None` (with the reason logged) unless the worktree is another live, indexed checkout.
fn inspect_worktree(
    git: &GitRepo,
    entry: &WorktreeEntry,
    target_root: &Path,
    target_head: &str,
    is_main_worktree: bool,
) -> Option<DonorCandidate> {
    if entry.is_bare || entry.is_prunable {
        return None;
    }
    let head = entry.head.as_ref()?;
    let root = fs::canonicalize(&entry.path).ok()?;
    let is_target = root == target_root;
    if is_target || !has_shards(&root) {
        return None;
    }
    let changed_files = match git.changed_files_between(head, target_head) {
        Ok(files) => files.len(),
        Err(error) => {
            log::warn!("skipping donor candidate {}: {error:#}", root.display());
            return None;
        }
    };
    log::debug!("donor candidate {}: {changed_files} changed files", root.display());
    Some(DonorCandidate { root, listed_root: entry.path.clone(), head: head.clone(), changed_files, is_main_worktree })
}

fn has_shards(worktree_root: &Path) -> bool {
    let index_dir = worktree_root.join(INDEX_RELATIVE_DIR);
    index_dir.is_dir() && list_shards(&index_dir).is_ok_and(|shards| !shards.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(root: &str, changed_files: usize, is_main_worktree: bool) -> DonorCandidate {
        DonorCandidate {
            root: PathBuf::from(root),
            listed_root: PathBuf::from(root),
            head: "aaaa".to_string(),
            changed_files,
            is_main_worktree,
        }
    }

    fn chosen_root(candidates: Vec<DonorCandidate>) -> Option<PathBuf> {
        choose_donor(candidates).map(|donor| donor.root)
    }

    #[test]
    fn chooses_fewest_changed_files() {
        let candidates = vec![candidate("/x/main", 9, true), candidate("/x/b", 2, false), candidate("/x/a", 5, false)];

        assert_eq!(chosen_root(candidates), Some(PathBuf::from("/x/b")));
    }

    #[test]
    fn prefers_main_worktree_on_tie() {
        let candidates = vec![candidate("/x/a", 3, false), candidate("/x/main", 3, true)];

        assert_eq!(chosen_root(candidates), Some(PathBuf::from("/x/main")));
    }

    #[test]
    fn prefers_smaller_path_on_tie_between_linked_worktrees() {
        let candidates = vec![candidate("/x/main", 7, true), candidate("/x/c", 1, false), candidate("/x/b", 1, false)];

        assert_eq!(chosen_root(candidates), Some(PathBuf::from("/x/b")));
    }

    #[test]
    fn chooses_nothing_without_candidates() {
        assert_eq!(chosen_root(Vec::new()), None);
    }
}
