//! Queries of a git repository by running the `git` executable; parsing lives in [`porcelain`].

pub mod porcelain;
pub mod worktree_changes;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

use porcelain::{FileChange, WorktreeEntry, parse_name_status, parse_path_list, parse_worktree_list};

/// `git config --get` exits with this code when the key is not set.
const CONFIG_KEY_MISSING_EXIT_CODE: i32 = 1;
/// `git ls-files --error-unmatch` exits with this code when the index lacks the path.
const PATH_UNTRACKED_EXIT_CODE: i32 = 1;
/// git exits with this code on every fatal error, including outside a repository.
const FATAL_EXIT_CODE: i32 = 128;
/// git's message outside a repository, in the C locale.
const NOT_A_REPOSITORY_MESSAGE: &str = "not a git repository";
/// Variables git exports to hooks that point a git command at a repository, index or configuration
/// regardless of `-C`; removed so every command reads the repository of its own directory.
pub const REPOSITORY_ENV_VARS: [&str; 11] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CEILING_DIRECTORIES",
];

/// A repository or worktree, addressed by any directory inside it (`git -C <dir>`).
pub struct GitRepo {
    dir: PathBuf,
}

impl GitRepo {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf() }
    }

    /// Root of the worktree that contains the directory.
    pub fn toplevel(&self) -> Result<PathBuf> {
        Ok(output_path(&self.run(&["rev-parse", "--show-toplevel"])?))
    }

    /// Object ID of `HEAD`.
    pub fn head(&self) -> Result<String> {
        let output = self.run(&["rev-parse", "--verify", "HEAD"])?;
        Ok(String::from_utf8_lossy(&output).trim_end().to_string())
    }

    /// The directory shared by all worktrees (the main `.git`), absolute.
    pub fn common_dir(&self) -> Result<PathBuf> {
        Ok(output_path(&self.run(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?))
    }

    /// `core.hooksPath` with `~` expanded, as configured (maybe relative); `None` when unset.
    pub fn hooks_path(&self) -> Result<Option<PathBuf>> {
        let args = ["config", "--type=path", "--get", "core.hooksPath"];
        let output = self.output(&args)?;
        let is_unset = output.status.code() == Some(CONFIG_KEY_MISSING_EXIT_CODE) && output.stdout.is_empty();
        if is_unset {
            return Ok(None);
        }
        Ok(Some(output_path(&self.checked(&args, output)?)))
    }

    /// All worktrees of the repository; the main worktree comes first.
    pub fn worktrees(&self) -> Result<Vec<WorktreeEntry>> {
        parse_worktree_list(&self.run(&["worktree", "list", "--porcelain", "-z"])?)
    }

    /// Repository-relative paths of the files that differ between two commits.
    pub fn changed_files_between(&self, from: &str, to: &str) -> Result<Vec<PathBuf>> {
        Ok(parse_path_list(&self.run(&["diff", "--name-only", "-z", "--no-renames", from, to])?))
    }

    /// Tracked files whose working-tree state (staged or not) differs from commit `base`.
    pub fn working_tree_changes(&self, base: &str) -> Result<Vec<FileChange>> {
        parse_name_status(&self.run(&["diff", "--name-status", "-z", "--no-renames", base])?)
    }

    /// Whether the directory is inside a git repository; an error when git cannot tell, such as when it
    /// is not installed or the repository is unreadable.
    pub fn is_in_repository(&self) -> Result<bool> {
        let args = ["rev-parse", "--git-dir"];
        // The C locale keeps the message that tells "outside a repository" from other fatal errors in English.
        let output = self.command(&args).env("LC_ALL", "C").output().context("running git (is it installed?)")?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let is_outside_repository = output.status.code() == Some(FATAL_EXIT_CODE) && stderr.contains(NOT_A_REPOSITORY_MESSAGE);
        if is_outside_repository {
            return Ok(false);
        }
        self.checked(&args, output)?;
        Ok(true)
    }

    /// Whether the index tracks the file at `path`, absolute or relative to the directory.
    pub fn is_tracked(&self, path: &Path) -> Result<bool> {
        let path = path.to_str().with_context(|| format!("{} is not UTF-8", path.display()))?;
        let args = ["--literal-pathspecs", "ls-files", "--error-unmatch", "--", path];
        let output = self.output(&args)?;
        if output.status.code() == Some(PATH_UNTRACKED_EXIT_CODE) {
            return Ok(false);
        }
        self.checked(&args, output)?;
        Ok(true)
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let output = self.output(args)?;
        self.checked(args, output)
    }

    fn output(&self, args: &[&str]) -> Result<Output> {
        self.command(args).output().context("running git (is it installed?)")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        for name in REPOSITORY_ENV_VARS {
            command.env_remove(name);
        }
        command.arg("-C").arg(&self.dir).args(args);
        command
    }

    fn checked(&self, args: &[&str], output: Output) -> Result<Vec<u8>> {
        if output.status.success() {
            return Ok(output.stdout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("`git {}` in {} failed ({}): {}", args.join(" "), self.dir.display(), output.status, stderr.trim());
    }
}

/// The path printed on the single output line of a git command.
fn output_path(output: &[u8]) -> PathBuf {
    let line = output.strip_suffix(b"\n").unwrap_or(output);
    PathBuf::from(OsString::from_vec(line.to_vec()))
}
