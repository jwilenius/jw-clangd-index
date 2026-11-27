//! Runs the built `jw-clangd-index` binary against real git repositories and worktrees.

mod common;

use std::collections::HashSet;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{
    ALL_SHARDS, CDB_RELATIVE_PATH, HEADER_SHARD, NO_CACHE_HOME, SAMPLE_ROOT, TU_SHARD, add_worktree, create_committed_repo,
    create_indexed_repo, fixture_dir, git, git_output, isolate_git_env, read_fixture, v19_fixture_dir, with_version,
};
use jw_clangd_index::format::string_table::TableCompression;
use jw_clangd_index::hook::HOOK_MARKER;
use jw_clangd_index::relocate::index_relocation::{INDEX_RELATIVE_DIR, IndexRelocationRequest, relocate_index};
use jw_clangd_index::relocate::path_map::{PathMap, RootMapping};
use tempfile::TempDir;

const SUCCESS_EXIT_CODE: i32 = 0;
const DIFFERENT_EXIT_CODE: i32 = 1;
const ERROR_EXIT_CODE: i32 = 2;
const DIFF_TARGET_ROOT: &str = "/work/wt";
const FOREIGN_HOOK: &str = "#!/bin/sh\necho foreign hook\n";
const HOOK_MODE: u32 = 0o755;
const PERMISSION_BITS: u32 = 0o777;
/// Start of the fixture header's shard name, which relocation changes after it.
const HEADER_SHARD_PREFIX: &str = "h0.h.";
/// A format version this tool cannot decode.
const UNSUPPORTED_VERSION: u32 = 18;
/// An external header that the fixture TU includes directly and the fixture index has no shard of.
const EXTERNAL_HEADER: &str = "/usr/include/c++/13/map";
/// The directory of the fixture header's own includes (`string`, `vector`).
const EXTERNAL_HEADER_DIR: &str = "/usr/include/c++/13";
const USER_INDEX_RELATIVE_DIR: &str = "clangd/index";

struct ToolRun {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Runs the binary in `dir` with git isolated like the fixtures' git.
fn run_tool(dir: &Path, args: &[&str]) -> ToolRun {
    run_tool_with_cache_home(dir, args, Path::new(NO_CACHE_HOME))
}

/// [`run_tool`] with clangd's user index in `cache_home/clangd/index`.
fn run_tool_with_cache_home(dir: &Path, args: &[&str], cache_home: &Path) -> ToolRun {
    let output = isolate_git_env(Command::new(env!("CARGO_BIN_EXE_jw-clangd-index")).args(args).current_dir(dir))
        .env("RUST_LOG", "info")
        .env("XDG_CACHE_HOME", cache_home)
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

fn hook_path(repo: &Path) -> PathBuf {
    repo.join(".git/hooks/post-checkout")
}

fn assert_contains(text: &str, expected: &str) {
    assert!(text.contains(expected), "expected {expected:?} in:\n{text}");
}

#[test]
fn relocate_chooses_donor_skips_deleted_file_and_relocates_cdb() {
    let repos = indexed_repo_with_worktree();
    git(&repos.worktree, &["rm", "--quiet", "include/lib/h0.h"]);
    fs::write(repos.worktree.join("include/lib/h1.h"), "// changed\n").unwrap();

    let run = run_tool(&repos.worktree, &["relocate"]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    assert_contains(&run.stdout, &format!("donor:    {} (closest worktree, 0 files differ)", repos.main.display()));
    assert_contains(&run.stdout, "2 relocated, 0 kept existing, 1 of deleted files skipped");
    // The stale count includes the system headers the fixture lacks shards for, which vary by machine.
    assert_contains(&run.stdout, "reindex:  1 TUs predicted (");
    assert_contains(&run.stdout, "), 0 CDB entries without a shard\n          src/tu0.cpp\n");
    let shards = index_shard_names(&repos.worktree);
    assert_eq!(shards.len(), 2);
    assert!(shards.iter().all(|name| !name.starts_with(HEADER_SHARD_PREFIX)), "{shards:?}");
    let cdb = fs::read_to_string(repos.worktree.join(CDB_RELATIVE_PATH)).unwrap();
    assert_contains(&cdb, &format!("{}/src/tu0.cpp", repos.worktree.display()));
    assert!(!cdb.contains(repos.main.to_str().unwrap()), "{cdb}");
}

#[test]
fn relocate_keeps_shard_of_ignored_file_removed_only_from_index() {
    let repos = indexed_repo_with_worktree();
    let gitignore = repos.worktree.join(".gitignore");
    let ignored = fs::read_to_string(&gitignore).unwrap() + "include/lib/h0.h\n";
    fs::write(&gitignore, ignored).unwrap();
    git(&repos.worktree, &["rm", "--cached", "--quiet", "include/lib/h0.h"]);

    let run = run_tool(&repos.worktree, &["relocate", "--json"]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    let summary: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(summary["shards"]["relocated"], 3);
    assert_eq!(summary["shards"]["skipped_deleted"], 0);
}

#[test]
fn relocate_prints_json_summary() {
    let repos = indexed_repo_with_worktree();
    git(&repos.worktree, &["rm", "--quiet", "include/lib/h0.h"]);

    let run = run_tool(&repos.worktree, &["relocate", "--json"]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    let summary: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(summary["is_dry_run"], false);
    assert_eq!(summary["donor_root"], repos.main.to_str().unwrap());
    assert_eq!(summary["donor_changed_files"], 0);
    assert_eq!(summary["target_root"], repos.worktree.to_str().unwrap());
    let expected_shards = serde_json::json!({
        "relocated": 2, "kept_existing": 0, "skipped_deleted": 1, "skipped_unsupported_version": 0, "failed": 0
    });
    assert_eq!(summary["shards"], expected_shards);
    assert_eq!(summary["cdb"]["action"], "written");
    assert_eq!(summary["cdb"]["path"], repos.worktree.join(CDB_RELATIVE_PATH).to_str().unwrap());
    assert_eq!(summary["reindex"]["has_cdb_main_files"], true);
    assert_eq!(summary["reindex"]["reindexed_tus"], serde_json::json!([repos.worktree.join("src/tu0.cpp")]));
    assert!(summary["reindex"]["stale_files"].is_u64(), "{summary}");
    assert_eq!(summary["reindex"]["main_files_without_shard"], serde_json::json!([]));
    assert!(summary["elapsed_seconds"].is_f64(), "{summary}");
    let expected_keys = ["cdb", "donor_changed_files", "donor_root", "elapsed_seconds", "is_dry_run", "reindex", "shards", "target_root"];
    assert_eq!(summary.as_object().unwrap().keys().collect::<Vec<_>>(), expected_keys);
}

#[test]
fn relocate_replaces_existing_shards_and_cdb_unless_keep_existing() {
    let repos = indexed_repo_with_worktree();
    let target_cdb = repos.worktree.join(CDB_RELATIVE_PATH);
    assert_eq!(run_tool(&repos.worktree, &["relocate"]).exit_code, SUCCESS_EXIT_CODE);
    fs::write(&target_cdb, "[]\n").unwrap();

    let kept_run = run_tool(&repos.worktree, &["relocate", "--keep-existing", "--json"]);
    let kept_cdb = fs::read_to_string(&target_cdb).unwrap();
    let replaced_run = run_tool(&repos.worktree, &["relocate", "--json"]);

    let kept: serde_json::Value = serde_json::from_str(&kept_run.stdout).unwrap();
    assert_eq!((&kept["shards"]["relocated"], &kept["shards"]["kept_existing"]), (&0.into(), &3.into()), "{kept}");
    assert_eq!(kept["cdb"]["action"], "kept_existing");
    assert_eq!(kept_cdb, "[]\n");
    let replaced: serde_json::Value = serde_json::from_str(&replaced_run.stdout).unwrap();
    assert_eq!((&replaced["shards"]["relocated"], &replaced["shards"]["kept_existing"]), (&3.into(), &0.into()), "{replaced}");
    assert_eq!(replaced["cdb"]["action"], "written");
    assert_contains(&fs::read_to_string(&target_cdb).unwrap(), &format!("{}/src/tu0.cpp", repos.worktree.display()));
}

#[test]
fn relocate_keeps_cdb_that_git_tracks() {
    let repos = indexed_repo_with_worktree();
    let target_cdb = repos.worktree.join(CDB_RELATIVE_PATH);
    fs::create_dir_all(target_cdb.parent().unwrap()).unwrap();
    fs::write(&target_cdb, "[]\n").unwrap();
    git(&repos.worktree, &["add", "--force", CDB_RELATIVE_PATH]);
    git(&repos.worktree, &["commit", "--quiet", "-m", "track the cdb"]);

    let run = run_tool(&repos.worktree, &["relocate", "--json"]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    let summary: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    let expected_cdb = serde_json::json!({ "action": "kept_tracked", "path": target_cdb, "reason": "tracked_by_git" });
    assert_eq!(summary["cdb"], expected_cdb);
    assert_eq!(fs::read_to_string(&target_cdb).unwrap(), "[]\n");
    assert_contains(&run.stderr, "git tracks");
}

#[test]
fn relocate_never_writes_through_link_to_donor_cdb() {
    let repos = indexed_repo_with_worktree();
    let donor_cdb = repos.main.join(CDB_RELATIVE_PATH);
    let donor_json = fs::read(&donor_cdb).unwrap();
    let target_link = repos.worktree.join(CDB_RELATIVE_PATH);
    fs::create_dir_all(target_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&donor_cdb, &target_link).unwrap();

    let run = run_tool(&repos.worktree, &["relocate", "--json"]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    let summary: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    let expected_cdb = serde_json::json!({ "action": "skipped_link", "path": target_link, "reason": "points_at_donor_cdb" });
    assert_eq!(summary["cdb"], expected_cdb);
    assert_eq!(fs::read(&donor_cdb).unwrap(), donor_json);
    assert_contains(&run.stderr, "links to the donor's database");
}

#[test]
fn relocate_warns_that_clangd_ignores_index_behind_skipped_dangling_link() {
    let repos = indexed_repo_with_worktree();
    let target_link = repos.worktree.join(CDB_RELATIVE_PATH);
    fs::create_dir_all(target_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("../../outside/compile_commands.json", &target_link).unwrap();

    let run = run_tool(&repos.worktree, &["relocate", "--json"]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    let summary: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(summary["cdb"]["reason"], "leaves_target");
    assert_contains(
        &run.stderr,
        "links outside the target, so the relocated compile_commands.json is not written through it; clangd will not use the index until the target has one",
    );
}

#[test]
fn relocate_json_has_nulls_for_a_given_donor_without_cdb_relocation() {
    let repos = indexed_repo_with_worktree();

    let run = run_tool(&repos.worktree, &["relocate", "--json", "--no-relocate-cdb", "--from", repos.main.to_str().unwrap()]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    let summary: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(summary["donor_root"], repos.main.to_str().unwrap());
    assert!(summary["donor_changed_files"].is_null(), "{summary}");
    assert!(summary["cdb"]["path"].is_null(), "{summary}");
    assert_eq!(summary["shards"]["relocated"], 3);
}

#[test]
fn relocate_dry_run_writes_nothing_and_warns_about_missing_cdb() {
    let repos = indexed_repo_with_worktree();
    fs::write(repos.worktree.join("src/tu0.cpp"), "// changed\n").unwrap();

    let run = run_tool(&repos.base, &["relocate", "--dry-run", "--no-relocate-cdb", "--to", repos.worktree.to_str().unwrap()]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    assert_contains(&run.stdout, "dry run: nothing was written\n");
    assert_contains(&run.stdout, "3 relocated");
    assert_contains(&run.stdout, "cdb:      not relocated; target has none\n");
    assert_contains(&run.stdout, "reindex:  1 TUs predicted once a compile database exists (");
    assert_contains(&run.stderr, "drop --no-relocate-cdb");
    assert!(!repos.worktree.join(".cache").exists());
}

/// The `N` of `N stale files` in a relocation summary.
fn stale_file_count(stdout: &str) -> usize {
    let (_, after_count_start) = stdout.split_once("TUs predicted (").expect(stdout);
    let (count, _) = after_count_start.split_once(" stale files").expect(stdout);
    count.parse().unwrap()
}

/// Writes into the user index under `cache_home` a shard of [`EXTERNAL_HEADER`]: the fixture header's,
/// with its self path mapped to the external header and its includes into `include_dir`.
fn write_external_header_shard(cache_home: &Path, include_dir: &Path) {
    let header_path = format!("{SAMPLE_ROOT}/include/lib/h0.h");
    let header_mapping = RootMapping { old_root: header_path.into_bytes(), new_root: EXTERNAL_HEADER.as_bytes().to_vec() };
    let include_mapping =
        RootMapping { old_root: EXTERNAL_HEADER_DIR.as_bytes().to_vec(), new_root: include_dir.as_os_str().as_bytes().to_vec() };
    let donor_dir = cache_home.join("donor");
    fs::create_dir_all(&donor_dir).unwrap();
    fs::copy(fixture_dir().join(HEADER_SHARD), donor_dir.join(HEADER_SHARD)).unwrap();
    let report = relocate_index(&IndexRelocationRequest {
        donor_dir,
        target_dir: cache_home.join(USER_INDEX_RELATIVE_DIR),
        path_map: PathMap::new(&[header_mapping, include_mapping]).unwrap(),
        compression: TableCompression::Default,
        is_overwrite: false,
        is_dry_run: false,
        deleted_target_paths: HashSet::new(),
    })
    .unwrap();
    assert_eq!(report.relocated, 1);
}

#[test]
fn relocate_reads_shards_of_external_headers_from_user_index() {
    let repos = indexed_repo_with_worktree();
    let cache_home = repos.base.join("cache");
    let include_dir = repos.base.join("external");
    write_external_header_shard(&cache_home, &include_dir);
    fs::create_dir_all(&include_dir).unwrap();
    for header in ["string", "vector"] {
        fs::write(include_dir.join(header), header).unwrap();
    }
    let args = ["relocate", "--dry-run"];

    let run = run_tool(&repos.worktree, &args);
    let user_index_run = run_tool_with_cache_home(&repos.worktree, &args, &cache_home);

    assert_eq!(user_index_run.exit_code, SUCCESS_EXIT_CODE, "{}", user_index_run.stderr);
    // The external header's shard leads the walk to its two readable includes, which have no shard.
    assert_eq!(stale_file_count(&user_index_run.stdout), stale_file_count(&run.stdout) + 2, "{}", user_index_run.stdout);
}

#[test]
fn relocate_maps_donor_root_given_through_symlink() {
    let repos = empty_repos();
    fs::create_dir(&repos.main).unwrap();
    let main_link = repos.base.join("main-link");
    std::os::unix::fs::symlink(&repos.main, &main_link).unwrap();
    create_indexed_repo(&main_link);
    add_worktree(&repos.main, &repos.worktree);

    let run = run_tool(&repos.base, &["relocate", "--from", "main-link/", "--to", repos.worktree.to_str().unwrap()]);

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

/// The fixture index in `fixtures` relocated to [`DIFF_TARGET_ROOT`] plus `extra_mappings`, in `temp/b`.
fn relocated_fixture_index(temp: &TempDir, fixtures: &Path, extra_mappings: &[RootMapping]) -> PathBuf {
    let root_mapping = RootMapping { old_root: SAMPLE_ROOT.as_bytes().to_vec(), new_root: DIFF_TARGET_ROOT.as_bytes().to_vec() };
    let mappings: Vec<RootMapping> = std::iter::once(root_mapping).chain(extra_mappings.iter().cloned()).collect();
    let target_dir = temp.path().join("b");
    relocate_index(&IndexRelocationRequest {
        donor_dir: fixtures.to_path_buf(),
        target_dir: target_dir.clone(),
        path_map: PathMap::new(&mappings).unwrap(),
        compression: TableCompression::Default,
        is_overwrite: false,
        is_dry_run: false,
        deleted_target_paths: HashSet::new(),
    })
    .unwrap();
    target_dir
}

fn run_diff(temp: &TempDir, dir_a: &Path, dir_b: &Path, extra_args: &[&str]) -> ToolRun {
    let map = format!("{SAMPLE_ROOT}={DIFF_TARGET_ROOT}");
    let mut args = vec!["diff", dir_a.to_str().unwrap(), dir_b.to_str().unwrap(), "--map", &map];
    args.extend_from_slice(extra_args);
    run_tool(temp.path(), &args)
}

#[test]
fn diff_finds_relocated_index_equal() {
    let temp = TempDir::new().unwrap();
    let dir_b = relocated_fixture_index(&temp, &fixture_dir(), &[]);

    let run = run_diff(&temp, &fixture_dir(), &dir_b, &[]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "indexes are equal: 3 shards compared, 0 differ, 0 only in A, 0 only in B\n");
}

#[test]
fn diff_finds_relocated_v19_index_equal() {
    let temp = TempDir::new().unwrap();
    let dir_b = relocated_fixture_index(&temp, &v19_fixture_dir(), &[]);

    let run = run_diff(&temp, &v19_fixture_dir(), &dir_b, &[]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "indexes are equal: 3 shards compared, 0 differ, 0 only in A, 0 only in B\n");
}

#[test]
fn diff_tells_v19_and_v20_shards_of_same_files_apart() {
    let temp = TempDir::new().unwrap();

    let run = run_tool(temp.path(), &["diff", fixture_dir().to_str().unwrap(), v19_fixture_dir().to_str().unwrap(), "--ignore-command"]);

    assert_eq!(run.exit_code, DIFFERENT_EXIT_CODE, "{}{}", run.stdout, run.stderr);
    assert_contains(&run.stdout, &format!("differs:   {TU_SHARD} ("));
}

#[test]
fn dump_prints_v19_shard_as_json() {
    let temp = TempDir::new().unwrap();
    let shard = v19_fixture_dir().join(TU_SHARD);

    let run = run_tool(temp.path(), &["dump", shard.to_str().unwrap()]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    let dump: serde_json::Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(dump["version"], 19);
    assert_eq!(dump["command"]["directory"], format!("{SAMPLE_ROOT}/build"));
    assert!(dump["symbols"].as_array().unwrap().iter().any(|symbol| symbol["name"] == "local_helper0"));
}

#[test]
fn diff_names_differing_command_unless_ignored() {
    let temp = TempDir::new().unwrap();
    let gcc_mapping = RootMapping { old_root: b"/usr/lib/gcc".to_vec(), new_root: b"/opt/gcc".to_vec() };
    let dir_b = relocated_fixture_index(&temp, &fixture_dir(), &[gcc_mapping]);

    let run = run_diff(&temp, &fixture_dir(), &dir_b, &[]);
    let ignoring_run = run_diff(&temp, &fixture_dir(), &dir_b, &["--ignore-command"]);

    assert_eq!(run.exit_code, DIFFERENT_EXIT_CODE, "{}{}", run.stdout, run.stderr);
    assert_contains(&run.stdout, "differs:   tu0.cpp.");
    assert_contains(&run.stdout, ".idx (command)\n");
    assert_eq!(ignoring_run.exit_code, SUCCESS_EXIT_CODE, "{}", ignoring_run.stdout);
}

#[test]
fn diff_skips_shards_of_unsupported_version() {
    let temp = TempDir::new().unwrap();
    let dir_b = relocated_fixture_index(&temp, &fixture_dir(), &[]);
    let dir_a = temp.path().join("a");
    fs::create_dir(&dir_a).unwrap();
    for file_name in ALL_SHARDS {
        fs::copy(fixture_dir().join(file_name), dir_a.join(file_name)).unwrap();
    }
    fs::write(dir_a.join(HEADER_SHARD), with_version(&read_fixture(HEADER_SHARD), UNSUPPORTED_VERSION)).unwrap();
    let header_shard_b = dir_b.join(shard_names(&dir_b).into_iter().find(|name| name.starts_with(HEADER_SHARD_PREFIX)).unwrap());
    fs::write(&header_shard_b, with_version(&fs::read(&header_shard_b).unwrap(), UNSUPPORTED_VERSION)).unwrap();
    let map = format!("{SAMPLE_ROOT}={DIFF_TARGET_ROOT}");

    let run = run_tool(temp.path(), &["diff", dir_a.to_str().unwrap(), dir_b.to_str().unwrap(), "--map", &map]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}{}", run.stdout, run.stderr);
    assert_contains(
        &run.stdout,
        &format!("skipped:   {} (unsupported format version {UNSUPPORTED_VERSION})\n", dir_a.join(HEADER_SHARD).display()),
    );
    assert_contains(&run.stdout, &format!("skipped:   {} (unsupported format version {UNSUPPORTED_VERSION})\n", header_shard_b.display()));
    assert_contains(&run.stdout, "indexes are equal: 2 shards compared");
}

#[test]
fn diff_reports_shard_missing_from_b() {
    let temp = TempDir::new().unwrap();
    let dir_b = relocated_fixture_index(&temp, &fixture_dir(), &[]);
    let header_shard = shard_names(&dir_b).into_iter().find(|name| name.starts_with(HEADER_SHARD_PREFIX)).unwrap();
    fs::remove_file(dir_b.join(header_shard)).unwrap();

    let run = run_diff(&temp, &fixture_dir(), &dir_b, &[]);

    assert_eq!(run.exit_code, DIFFERENT_EXIT_CODE);
    assert_contains(&run.stdout, &format!("only in A: {HEADER_SHARD_PREFIX}"));
}

#[test]
fn installed_hook_relocates_into_new_worktree() {
    let repos = empty_repos();
    create_indexed_repo(&repos.main);

    let install_run = run_tool(&repos.main, &["install-hook"]);
    add_worktree(&repos.main, &repos.worktree);
    let reinstall_run = run_tool(&repos.main, &["install-hook"]);

    assert_eq!(install_run.exit_code, SUCCESS_EXIT_CODE, "{}", install_run.stderr);
    assert_eq!(install_run.stdout, format!("installed {}\n", hook_path(&repos.main).display()));
    let hook = fs::read_to_string(hook_path(&repos.main)).unwrap();
    assert_contains(&hook, HOOK_MARKER);
    assert_contains(&hook, "relocate --to \"$PWD\" || true");
    assert_eq!(fs::metadata(hook_path(&repos.main)).unwrap().permissions().mode() & PERMISSION_BITS, HOOK_MODE);
    assert_eq!(index_shard_names(&repos.worktree).len(), 3);
    assert!(repos.worktree.join(CDB_RELATIVE_PATH).is_file());
    assert_eq!(reinstall_run.stdout, format!("updated {}\n", hook_path(&repos.main).display()));
}

#[test]
fn install_hook_passes_opt_out_flags_to_relocate() {
    let repos = empty_repos();
    create_indexed_repo(&repos.main);

    let run = run_tool(&repos.main, &["install-hook", "--no-relocate-cdb", "--keep-existing"]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    let hook = fs::read_to_string(hook_path(&repos.main)).unwrap();
    assert_contains(&hook, "relocate --to \"$PWD\" --no-relocate-cdb --keep-existing || true");
}

#[test]
fn installed_hook_ignores_clone() {
    let repos = empty_repos();
    create_indexed_repo(&repos.main);
    run_tool(&repos.main, &["install-hook"]);
    let hooks_dir = format!("core.hooksPath={}", repos.main.join(".git/hooks").display());
    let clone = repos.base.join("clone");

    let output = git_output(&repos.base, &["-c", &hooks_dir, "clone", "--quiet", "main", "clone"]);

    // Without the linked-worktree check the hook would run and fail to find a donor for the clone.
    assert!(String::from_utf8_lossy(&output.stderr).is_empty(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!clone.join(".cache").exists());
}

#[test]
fn installed_hook_ignores_branch_switch() {
    let repos = indexed_repo_with_worktree();
    run_tool(&repos.main, &["install-hook"]);

    git(&repos.worktree, &["switch", "--quiet", "-c", "other"]);

    assert!(!repos.worktree.join(".cache").exists());
}

#[test]
fn install_hook_supports_bare_repository() {
    let temp = TempDir::new().unwrap();
    let bare = fs::canonicalize(temp.path()).unwrap().join("bare.git");
    git(temp.path(), &["init", "--quiet", "--bare", bare.to_str().unwrap()]);

    let run = run_tool(&bare, &["install-hook"]);

    assert_eq!(run.exit_code, SUCCESS_EXIT_CODE, "{}", run.stderr);
    assert_eq!(run.stdout, format!("installed {}\n", bare.join("hooks/post-checkout").display()));
}

#[test]
fn install_hook_keeps_foreign_hook_unless_forced() {
    let repos = empty_repos();
    create_committed_repo(&repos.main);
    fs::write(hook_path(&repos.main), FOREIGN_HOOK).unwrap();

    let refused_run = run_tool(&repos.main, &["install-hook"]);
    let hook_after_refusal = fs::read_to_string(hook_path(&repos.main)).unwrap();
    let forced_run = run_tool(&repos.main, &["install-hook", "--force"]);

    assert_eq!(refused_run.exit_code, ERROR_EXIT_CODE);
    assert_contains(&refused_run.stderr, "--force");
    assert_eq!(hook_after_refusal, FOREIGN_HOOK);
    assert_eq!(forced_run.exit_code, SUCCESS_EXIT_CODE, "{}", forced_run.stderr);
    assert_contains(&fs::read_to_string(hook_path(&repos.main)).unwrap(), HOOK_MARKER);
}
