//! Runs the built `jw-clangd-index` binary against real git repositories and worktrees.

mod common;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{CDB_RELATIVE_PATH, add_worktree, create_committed_repo, create_indexed_repo, git, isolate_git_env};
use jw_clangd_index::relocate::index_relocation::INDEX_RELATIVE_DIR;
use tempfile::TempDir;

const SUCCESS_EXIT_CODE: i32 = 0;
const ERROR_EXIT_CODE: i32 = 2;
/// Start of the fixture header's shard name, which relocation changes after it.
const HEADER_SHARD_PREFIX: &str = "h0.h.";

struct ToolRun {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Runs the binary in `dir` with git isolated like the fixtures' git.
fn run_tool(dir: &Path, args: &[&str]) -> ToolRun {
    let output = isolate_git_env(Command::new(env!("CARGO_BIN_EXE_jw-clangd-index")).args(args).current_dir(dir))
        .env("RUST_LOG", "info")
        .output()
        .unwrap();
    ToolRun {
        exit_code: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

struct Repos {
    _temp: TempDir,
    /// Canonical temp root holding both checkouts.
    base: PathBuf,
    main: PathBuf,
    worktree: PathBuf,
}

/// An indexed main worktree and a fresh linked worktree of it at the same commit.
fn indexed_repo_with_worktree() -> Repos {
    let repos = empty_repos();
    create_indexed_repo(&repos.main);
    add_worktree(&repos.main, &repos.worktree);
    repos
}

fn empty_repos() -> Repos {
    let temp = TempDir::new().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    Repos { main: base.join("main"), worktree: base.join("wt"), base, _temp: temp }
}

fn index_shard_names(root: &Path) -> HashSet<String> {
    shard_names(&root.join(INDEX_RELATIVE_DIR))
}

fn shard_names(index_dir: &Path) -> HashSet<String> {
    let names = fs::read_dir(index_dir).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap());
    names.filter(|name| name.ends_with(".idx")).collect()
}

fn assert_contains(text: &str, expected: &str) {
    assert!(text.contains(expected), "expected {expected:?} in:\n{text}");
}

#[test]
fn relocate_chooses_donor_skips_deleted_file_and_relocates_cdb() {
    let repos = indexed_repo_with_worktree();
    git(&repos.worktree, &["rm", "--quiet", "include/lib/h0.h"]);
    fs::write(repos.worktree.join("include/lib/h1.h"), "// changed\n").unwrap();

    let run = run_tool(&repos.worktree, &["relocate", "--relocate-cdb"]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    assert_contains(&run.stdout, &format!("donor:    {} (closest worktree, 0 files differ)", repos.main.display()));
    assert_contains(&run.stdout, "2 relocated, 0 kept existing, 1 of deleted files skipped");
    let shards = index_shard_names(&repos.worktree);
    assert_eq!(shards.len(), 2);
    assert!(shards.iter().all(|name| !name.starts_with(HEADER_SHARD_PREFIX)), "{shards:?}");
    let cdb = fs::read_to_string(repos.worktree.join(CDB_RELATIVE_PATH)).unwrap();
    assert_contains(&cdb, &format!("{}/src/tu0.cpp", repos.worktree.display()));
    assert!(!cdb.contains(repos.main.to_str().unwrap()), "{cdb}");
}

#[test]
fn relocate_dry_run_writes_nothing_and_warns_about_missing_cdb() {
    let repos = indexed_repo_with_worktree();

    let run = run_tool(&repos.base, &["relocate", "--dry-run", "--to", repos.worktree.to_str().unwrap()]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    assert_contains(&run.stdout, "dry run: nothing was written\n");
    assert_contains(&run.stdout, "3 relocated");
    assert_contains(&run.stdout, "cdb:      not relocated; target has none\n");
    assert_contains(&run.stderr, "--relocate-cdb");
    assert!(!repos.worktree.join(".cache").exists());
}

#[test]
fn relocate_maps_donor_root_given_through_symlink() {
    let repos = empty_repos();
    fs::create_dir(&repos.main).unwrap();
    let main_link = repos.base.join("main-link");
    std::os::unix::fs::symlink(&repos.main, &main_link).unwrap();
    create_indexed_repo(&main_link);
    add_worktree(&repos.main, &repos.worktree);

    let run = run_tool(&repos.base, &["relocate", "--relocate-cdb", "--from", "main-link/", "--to", repos.worktree.to_str().unwrap()]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    assert!(!run.stderr.contains("unchanged"), "{}", run.stderr);
    let cdb = fs::read_to_string(repos.worktree.join(CDB_RELATIVE_PATH)).unwrap();
    assert_contains(&cdb, &format!("{}/src/tu0.cpp", repos.worktree.display()));
    assert!(!cdb.contains(main_link.to_str().unwrap()), "{cdb}");
}

#[test]
fn relocate_refuses_directory_inside_worktree() {
    let repos = indexed_repo_with_worktree();

    let run = run_tool(&repos.base, &["relocate", "--to", repos.worktree.join("src").to_str().unwrap()]);

    assert_eq!(run.exit_code, ERROR_EXIT_CODE);
    assert_contains(&run.stderr, "pass the worktree root");
}

#[test]
fn relocate_fails_without_indexed_donor() {
    let repos = empty_repos();
    create_committed_repo(&repos.main);
    add_worktree(&repos.main, &repos.worktree);

    let run = run_tool(&repos.worktree, &["relocate"]);

    assert_eq!(run.exit_code, ERROR_EXIT_CODE);
    assert_contains(&run.stderr, "pass --from DONOR_ROOT");
}
