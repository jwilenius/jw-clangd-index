//! Real clangd 23.1.0 shards (format v20) shared by the integration tests.
//!
//! Each test binary uses only some of these helpers.
#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use jw_clangd_index::format::riff::{parse_riff, write_riff};
use jw_clangd_index::format::shard::META_CHUNK;
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
