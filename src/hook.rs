//! The `install-hook` command: a git `post-checkout` hook that relocates an index into every new worktree.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::atomic_write::write_atomically;
use crate::git::GitRepo;

/// Identifies a hook this tool wrote, which it may replace.
pub const HOOK_MARKER: &str = "# Installed by jw-clangd-index install-hook; reinstalling replaces this file.";
const HOOK_NAME: &str = "post-checkout";
const HOOK_MODE: u32 = 0o755;
/// Lengths of a SHA-1 and a SHA-256 object ID.
const OBJECT_ID_LENGTHS: [usize; 2] = [40, 64];
const DEFAULT_HOOKS_DIR: &str = "hooks";

pub struct HookInstallRequest {
    /// Any directory inside the repository.
    pub repo: PathBuf,
    /// Absolute path of the `jw-clangd-index` executable the hook runs.
    pub binary: PathBuf,
    pub is_relocate_cdb: bool,
    /// Replace a `post-checkout` hook this tool did not write.
    pub is_force: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookInstallDecision {
    Create,
    ReplaceOwn,
    ReplaceForeign,
    Refuse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookInstallReport {
    pub hook_path: PathBuf,
    pub decision: HookInstallDecision,
}

pub fn install_hook(request: &HookInstallRequest) -> Result<HookInstallReport> {
    let git = GitRepo::new(&request.repo);
    let hooks_dir = find_hooks_dir(&git)?;
    let hook_path = hooks_dir.join(HOOK_NAME);
    let binary = request.binary.to_str().with_context(|| format!("executable path {} is not UTF-8", request.binary.display()))?;
    let script = hook_script(binary, request.is_relocate_cdb);

    let decision = decide_hook_install(read_existing_hook(&hook_path)?.as_deref(), request.is_force);
    if decision == HookInstallDecision::Refuse {
        bail!(
            "{} exists and was not installed by jw-clangd-index; add these lines to it yourself, \
             or pass --force to replace it:\n{}",
            hook_path.display(),
            hook_body(binary, request.is_relocate_cdb)
        );
    }
    fs::create_dir_all(&hooks_dir).with_context(|| format!("creating {}", hooks_dir.display()))?;
    write_atomically(&hook_path, script.as_bytes(), Some(HOOK_MODE))?;
    Ok(HookInstallReport { hook_path, decision })
}

/// `core.hooksPath` when set, else `hooks` in the common git directory, so every worktree shares the
/// hook. Only a relative `core.hooksPath` needs the worktree root, which a bare repository lacks.
fn find_hooks_dir(git: &GitRepo) -> Result<PathBuf> {
    match git.hooks_path()? {
        None => Ok(git.common_dir()?.join(DEFAULT_HOOKS_DIR)),
        Some(hooks_path) if hooks_path.is_absolute() => Ok(hooks_path),
        // git runs hooks in the worktree root and resolves a relative hooks path against it.
        Some(hooks_path) => Ok(git.toplevel()?.join(hooks_path)),
    }
}

pub fn decide_hook_install(existing_hook: Option<&[u8]>, is_force: bool) -> HookInstallDecision {
    let Some(existing_hook) = existing_hook else {
        return HookInstallDecision::Create;
    };
    let is_own_hook = String::from_utf8_lossy(existing_hook).contains(HOOK_MARKER);
    match (is_own_hook, is_force) {
        (true, _) => HookInstallDecision::ReplaceOwn,
        (false, true) => HookInstallDecision::ReplaceForeign,
        (false, false) => HookInstallDecision::Refuse,
    }
}

pub fn hook_script(binary: &str, is_relocate_cdb: bool) -> String {
    format!("#!/bin/sh\n{HOOK_MARKER}\n{}", hook_body(binary, is_relocate_cdb))
}

/// git passes the previous `HEAD` as `$1`; it is the null object ID only on a fresh checkout, so branch
/// switches do nothing. Of the fresh checkouts only `git worktree add` creates a linked worktree, whose
/// git directory differs from the common one; a clone or the first checkout of an unborn branch in
/// the main worktree has no other worktree to take an index from. The hook never fails the checkout.
fn hook_body(binary: &str, is_relocate_cdb: bool) -> String {
    let null_ids: Vec<String> = OBJECT_ID_LENGTHS.iter().map(|&length| "0".repeat(length)).collect();
    format!(
        "# Relocates the clangd index of the closest worktree into a newly created linked worktree.\n\
         case \"$1\" in\n\
         {}) ;;\n\
         *) exit 0 ;;\n\
         esac\n\
         [ \"$(git rev-parse --absolute-git-dir)\" = \"$(git rev-parse --path-format=absolute --git-common-dir)\" ] && exit 0\n\
         {} || true\n",
        null_ids.join("|"),
        relocate_command_line(binary, is_relocate_cdb)
    )
}

fn relocate_command_line(binary: &str, is_relocate_cdb: bool) -> String {
    let cdb_flag = if is_relocate_cdb { " --relocate-cdb" } else { "" };
    format!("{} relocate --to \"$PWD\"{cdb_flag}", shell_quote(binary))
}

/// `text` as one single-quoted POSIX shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn read_existing_hook(hook_path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(hook_path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", hook_path.display())),
    }
}

impl fmt::Display for HookInstallReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let action = match self.decision {
            HookInstallDecision::Create => "installed",
            HookInstallDecision::ReplaceOwn => "updated",
            HookInstallDecision::ReplaceForeign => "replaced the existing hook with",
            HookInstallDecision::Refuse => "refused to install",
        };
        writeln!(formatter, "{action} {}", self.hook_path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BINARY: &str = "/opt/my tools/jw-clangd-index";

    #[test]
    fn script_runs_relocation_only_for_null_previous_head() {
        let script = hook_script(BINARY, false);

        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains(HOOK_MARKER));
        assert!(script.contains(&format!("{}|{}) ;;", "0".repeat(40), "0".repeat(64))));
        assert!(script.contains("*) exit 0 ;;"));
        assert!(script.contains(
            "[ \"$(git rev-parse --absolute-git-dir)\" = \"$(git rev-parse --path-format=absolute --git-common-dir)\" ] && exit 0\n"
        ));
        assert!(script.contains("'/opt/my tools/jw-clangd-index' relocate --to \"$PWD\" || true\n"));
    }

    #[test]
    fn script_passes_relocate_cdb_when_asked() {
        assert!(hook_script(BINARY, true).contains("relocate --to \"$PWD\" --relocate-cdb || true"));
    }

    #[test]
    fn quotes_single_quotes_in_binary_path() {
        assert_eq!(shell_quote("/a/it's/bin"), r"'/a/it'\''s/bin'");
    }

    #[test]
    fn creates_missing_hook() {
        assert_eq!(decide_hook_install(None, false), HookInstallDecision::Create);
    }

    #[test]
    fn replaces_own_hook_without_force() {
        let own_hook = hook_script(BINARY, false);

        assert_eq!(decide_hook_install(Some(own_hook.as_bytes()), false), HookInstallDecision::ReplaceOwn);
    }

    #[test]
    fn refuses_foreign_hook_unless_forced() {
        let foreign_hook = b"#!/bin/sh\nexec git lfs post-checkout \"$@\"\n";

        assert_eq!(decide_hook_install(Some(foreign_hook), false), HookInstallDecision::Refuse);
        assert_eq!(decide_hook_install(Some(foreign_hook), true), HookInstallDecision::ReplaceForeign);
    }
}
