//! Real clangd 23.1.0 shards (format v20) and git repositories shared by the integration tests.
//!
//! Each test binary uses only some of these helpers.
#![allow(dead_code)]

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use jw_clangd_index::format::riff::{parse_riff, write_riff};
use jw_clangd_index::format::shard::META_CHUNK;
use jw_clangd_index::format::string_table::TableCompression;
use jw_clangd_index::git::REPOSITORY_ENV_VARS;
use jw_clangd_index::relocate::index_relocation::{INDEX_RELATIVE_DIR, IndexRelocationRequest, relocate_index};
use jw_clangd_index::relocate::path_map::{PathMap, RootMapping};

/// Project root of the checkout that clangd 23.1.0 indexed when it wrote the project fixtures.
///
/// Only a string baked into the fixture bytes; the directory need not exist. The fixtures were made with
/// the shared scripts `make-sample-cpp-project.py` and `clangd-background-index-run.py`.
pub const SAMPLE_ROOT: &str = "/tmp/claude-1000/-home-jim-projects-jw-clangd-index/bf56e891-af29-4210-8506-f55cbd7454da/scratchpad/sample";
pub const TU_SHARD: &str = "tu0.cpp.F62AFE0C00FAC064.idx";
pub const HEADER_SHARD: &str = "h0.h.AE9F8A9495C4CC86.idx";
/// System header shard (paths under `/usr`, URI contains `c%2B%2B`).
pub const SYSTEM_SHARD: &str = "uses_allocator_args.h.DD2424DE20F3F1C7.idx";
pub const ALL_SHARDS: [&str; 3] = [TU_SHARD, HEADER_SHARD, SYSTEM_SHARD];

pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn read_fixture(file_name: &str) -> Vec<u8> {
    std::fs::read(fixture_dir().join(file_name)).unwrap()
}

pub fn os(file_name: &str) -> OsString {
    OsString::from(file_name)
}

pub fn path_map(old_root: &str, new_root: &str) -> PathMap {
    let mapping = RootMapping { old_root: old_root.as_bytes().to_vec(), new_root: new_root.as_bytes().to_vec() };
    PathMap::new(&[mapping]).unwrap()
}

/// `bytes` with the `meta` chunk's leading u32le format version replaced by `version`.
pub fn with_version(bytes: &[u8], version: u32) -> Vec<u8> {
    let mut riff = parse_riff(bytes).unwrap();
    let mut meta = riff.chunk(META_CHUNK).unwrap().to_vec();
    meta[..size_of::<u32>()].copy_from_slice(&version.to_le_bytes());
    riff.replace_chunk(META_CHUNK, meta);
    write_riff(&riff)
}

/// Isolates git from the user's and the system's configuration and fixes commit metadata.
pub const GIT_ENV: [(&str, &str); 8] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_AUTHOR_NAME", "Test Author"),
    ("GIT_AUTHOR_EMAIL", "author@example.com"),
    ("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z"),
    ("GIT_COMMITTER_NAME", "Test Committer"),
    ("GIT_COMMITTER_EMAIL", "committer@example.com"),
    ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z"),
];
/// Source files of the fixture TU's include graph, relative to the project root.
pub const SAMPLE_SOURCES: [&str; 5] = ["src/tu0.cpp", "include/lib/h0.h", "include/lib/h1.h", "include/lib/h2.h", "include/lib/h3.h"];
pub const CDB_RELATIVE_PATH: &str = "build/compile_commands.json";

/// `command` in [`GIT_ENV`], without the variables that would point git at another repository, such as
/// those git sets while running a hook of the repository under test.
pub fn isolate_git_env(command: &mut Command) -> &mut Command {
    for name in REPOSITORY_ENV_VARS {
        command.env_remove(name);
    }
    command.envs(GIT_ENV)
}

/// Runs `git -C dir args` in [`GIT_ENV`] and returns its stdout; panics on failure.
pub fn git(dir: &Path, args: &[&str]) -> String {
    String::from_utf8(git_output(dir, args).stdout).unwrap()
}

/// Runs `git -C dir args` in [`GIT_ENV`]; panics on failure.
pub fn git_output(dir: &Path, args: &[&str]) -> Output {
    let output = isolate_git_env(Command::new("git").arg("-C").arg(dir).args(args)).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "git {args:?} in {} failed: {stderr}", dir.display());
    output
}

/// A git repository at `root` with one commit of [`SAMPLE_SOURCES`], an ignored build directory holding a
/// compilation database, and the fixture shards relocated into its clangd index.
pub fn create_indexed_repo(root: &Path) {
    create_committed_repo(root);
    write_cdb(root);
    write_index(root);
}

/// Like [`create_indexed_repo`], without the index and the compilation database.
pub fn create_committed_repo(root: &Path) {
    for relative_path in SAMPLE_SOURCES {
        let path = root.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("// {relative_path}\n")).unwrap();
    }
    fs::write(root.join(".gitignore"), ".cache/\nbuild/\n").unwrap();
    git(root, &["init", "--quiet", "--initial-branch=main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "--message=Initial commit"]);
}

/// Adds a linked worktree of `repo` at `path` on a new branch named after its directory.
pub fn add_worktree(repo: &Path, path: &Path) {
    let branch = path.file_name().unwrap().to_str().unwrap();
    git(repo, &["worktree", "add", "--quiet", "-b", branch, path.to_str().unwrap()]);
}

fn write_cdb(root: &Path) {
    let root = root.to_str().unwrap();
    let cdb = format!(
        "[{{\"directory\": \"{root}/build\", \"file\": \"{root}/src/tu0.cpp\", \
         \"arguments\": [\"c++\", \"-I{root}/include\", \"-c\", \"{root}/src/tu0.cpp\"]}}]\n"
    );
    let path = Path::new(root).join(CDB_RELATIVE_PATH);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, cdb).unwrap();
}

fn write_index(root: &Path) {
    let report = relocate_index(&IndexRelocationRequest {
        donor_dir: fixture_dir(),
        target_dir: root.join(INDEX_RELATIVE_DIR),
        path_map: path_map(SAMPLE_ROOT, root.to_str().unwrap()),
        compression: TableCompression::Fast,
        is_overwrite: false,
        is_dry_run: false,
        deleted_target_paths: HashSet::new(),
    })
    .unwrap();
    assert_eq!(report.relocated, ALL_SHARDS.len());
}
