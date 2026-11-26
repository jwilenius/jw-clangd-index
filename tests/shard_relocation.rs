mod common;

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use common::{ALL_SHARDS, HEADER_SHARD, SAMPLE_ROOT, SYSTEM_SHARD, TU_SHARD, os, path_map, read_fixture, with_version};
use jw_clangd_index::format::bytes::{ByteReader, write_var};
use jw_clangd_index::format::digest::{digest, digest_hex};
use jw_clangd_index::format::riff::{parse_riff, write_riff};
use jw_clangd_index::format::shard::{SYMBOLS_CHUNK, ShardFile, TAGS_VERSION};
use jw_clangd_index::format::shard_contents::{ShardContents, decode_shard_contents};
use jw_clangd_index::format::string_table::TableCompression;
use jw_clangd_index::relocate::path_map::PathMap;
use jw_clangd_index::relocate::shard_relocation::{HashSelfCheckError, UnsupportedVersionError, relocate_shard};

const MOVED_ROOT: &str = "/work/my proj+wt";
const MOVED_ROOT_URI: &str = "file:///work/my%20proj%2Bwt/";
const CLANGD_23_VERSION: u32 = 20;
const UNSUPPORTED_VERSION: u32 = 19;
/// Needs two varint bytes, so a misplaced insert shifts every later field.
const SYNTHETIC_TAGS: u32 = 0x1234;

// `symb` record layout, as `read_symbol` in `src/format/shard_contents.rs` reads it.
const SYMBOL_ID_AND_KIND_SIZE: usize = 8 + 2;
const STRINGS_BEFORE_LOCATIONS: usize = 3;
const VARS_PER_LOCATION: usize = 5;
const LOCATIONS: usize = 2;
const STRINGS_AFTER_FLAGS: usize = 5;
const VARS_PER_INCLUDE_HEADER: usize = 2;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn fixtures_are_clangd_23_shards_that_decode() {
    for file_name in ALL_SHARDS {
        let bytes = read_fixture(file_name);

        let shard = ShardFile::parse(&bytes).unwrap();
        let contents = decode_shard_contents(&shard).unwrap();

        assert_eq!(contents.version, CLANGD_23_VERSION, "{file_name}");
    }
}

#[test]
fn finds_self_node_so_path_hash_matches_clangd_file_name() {
    let identity_map = PathMap::new(&[]).unwrap();
    for file_name in ALL_SHARDS {
        let bytes = read_fixture(file_name);

        let relocated = relocate_shard(&os(file_name), &bytes, &identity_map, TableCompression::Fast).unwrap();

        assert_eq!(relocated.file_name, os(file_name));
    }
}

#[test]
fn round_trip_restores_contents_and_file_name() {
    let forward = path_map(SAMPLE_ROOT, MOVED_ROOT);
    let backward = path_map(MOVED_ROOT, SAMPLE_ROOT);
    for file_name in ALL_SHARDS {
        let original = read_fixture(file_name);

        let moved = relocate_shard(&os(file_name), &original, &forward, TableCompression::Fast).unwrap();
        let restored = relocate_shard(&moved.file_name, &moved.bytes, &backward, TableCompression::Default).unwrap();

        let original_contents = decode_shard_contents(&ShardFile::parse(&original).unwrap()).unwrap();
        let restored_contents = decode_shard_contents(&ShardFile::parse(&restored.bytes).unwrap()).unwrap();
        assert_eq!(restored_contents, original_contents, "{file_name}");
        assert_eq!(restored.file_name, os(file_name));
    }
}

#[test]
fn relocation_leaves_no_old_root_in_any_string() {
    let forward = path_map(SAMPLE_ROOT, MOVED_ROOT);
    for file_name in [TU_SHARD, HEADER_SHARD] {
        let original = read_fixture(file_name);

        let moved = relocate_shard(&os(file_name), &original, &forward, TableCompression::None).unwrap();

        let strings = ShardFile::parse(&moved.bytes).unwrap().strings.strings;
        let leftover = strings.iter().find(|string| contains(string, SAMPLE_ROOT.as_bytes()));
        assert_eq!(leftover.map(|string| String::from_utf8_lossy(string).into_owned()), None, "{file_name}");
        assert!(strings.iter().any(|string| contains(string, MOVED_ROOT_URI.as_bytes())), "{file_name}");
    }
}

#[test]
fn names_relocated_shard_after_mapped_source_path() {
    let forward = path_map(SAMPLE_ROOT, MOVED_ROOT);
    let bytes = read_fixture(TU_SHARD);

    let moved = relocate_shard(&os(TU_SHARD), &bytes, &forward, TableCompression::Fast).unwrap();

    let expected_path = format!("{MOVED_ROOT}/src/tu0.cpp");
    assert_eq!(moved.source_path, Path::new(&expected_path));
    assert_eq!(moved.file_name, os(&format!("tu0.cpp.{}.idx", digest_hex(&digest(expected_path.as_bytes())))));
}

#[test]
fn rewrites_uris_and_raw_compile_arguments() {
    let forward = path_map(SAMPLE_ROOT, MOVED_ROOT);
    let bytes = read_fixture(TU_SHARD);

    let moved = relocate_shard(&os(TU_SHARD), &bytes, &forward, TableCompression::Fast).unwrap();

    let contents = decode_shard_contents(&ShardFile::parse(&moved.bytes).unwrap()).unwrap();
    let command = contents.command.unwrap();
    assert_eq!(command.directory, format!("{MOVED_ROOT}/build"));
    assert!(command.arguments.contains(&format!("-I{MOVED_ROOT}/include")));
    assert!(contents.sources.iter().any(|source| source.uri == format!("{MOVED_ROOT_URI}src/tu0.cpp")));
}

#[test]
fn keeps_paths_outside_mapped_roots() {
    let forward = path_map(SAMPLE_ROOT, MOVED_ROOT);
    let original = read_fixture(SYSTEM_SHARD);

    let moved = relocate_shard(&os(SYSTEM_SHARD), &original, &forward, TableCompression::Fast).unwrap();

    let original_strings = ShardFile::parse(&original).unwrap().strings;
    assert_eq!(ShardFile::parse(&moved.bytes).unwrap().strings, original_strings);
    assert_eq!(moved.file_name, os(SYSTEM_SHARD));
}

#[test]
fn reports_hash_self_check_failure_when_no_source_hashes_to_file_name() {
    let identity_map = PathMap::new(&[]).unwrap();
    let bytes = read_fixture(TU_SHARD);

    let result = relocate_shard(&os("tu0.cpp.0000000000000000.idx"), &bytes, &identity_map, TableCompression::Fast);

    assert!(result.unwrap_err().downcast_ref::<HashSelfCheckError>().is_some());
}

#[test]
fn rejects_malformed_file_name_and_corrupt_bytes() {
    let identity_map = PathMap::new(&[]).unwrap();
    let bytes = read_fixture(TU_SHARD);

    let misnamed = relocate_shard(&os("tu0.cpp.idx"), &bytes, &identity_map, TableCompression::Fast);
    let corrupt = relocate_shard(&os(TU_SHARD), &bytes[..bytes.len() / 2], &identity_map, TableCompression::Fast);

    for error in [misnamed.unwrap_err(), corrupt.unwrap_err()] {
        assert!(error.downcast_ref::<HashSelfCheckError>().is_none(), "{error:#}");
    }
}

#[test]
fn self_path_bytes_are_the_mapped_uri_path() {
    let forward = path_map(SAMPLE_ROOT, MOVED_ROOT);
    let bytes = read_fixture(HEADER_SHARD);

    let moved = relocate_shard(&os(HEADER_SHARD), &bytes, &forward, TableCompression::Fast).unwrap();

    assert_eq!(moved.source_path.as_os_str().as_bytes(), format!("{MOVED_ROOT}/include/lib/h0.h").as_bytes());
}

fn decode(bytes: &[u8]) -> ShardContents {
    decode_shard_contents(&ShardFile::parse(bytes).unwrap()).unwrap()
}

/// Rewrites a v20 shard as v21: patches the version and inserts `tags` after `Type` in every `symb` record.
fn as_v21_with_tags(v20_bytes: &[u8], tags: u32) -> Vec<u8> {
    let v21_bytes = with_version(v20_bytes, TAGS_VERSION);
    let mut riff = parse_riff(&v21_bytes).unwrap();
    let symbols = insert_symbol_tags(riff.chunk(SYMBOLS_CHUNK).unwrap(), tags);
    riff.replace_chunk(SYMBOLS_CHUNK, symbols);
    write_riff(&riff)
}

fn insert_symbol_tags(symbols: &[u8], tags: u32) -> Vec<u8> {
    let mut reader = ByteReader::new(symbols);
    let mut out = Vec::with_capacity(symbols.len());
    let mut copied_until = 0;
    while !reader.is_empty() {
        skip_symbol_fields_before_tags(&mut reader);
        let tags_offset = symbols.len() - reader.remaining();
        out.extend_from_slice(&symbols[copied_until..tags_offset]);
        write_var(&mut out, tags);
        copied_until = tags_offset;
        let header_count = reader.read_count().unwrap();
        skip_vars(&mut reader, header_count * VARS_PER_INCLUDE_HEADER);
    }
    out.extend_from_slice(&symbols[copied_until..]);
    out
}

/// ID, kind, lang, three strings, two locations, references, flags, then five strings.
fn skip_symbol_fields_before_tags(reader: &mut ByteReader) {
    reader.read_bytes(SYMBOL_ID_AND_KIND_SIZE).unwrap();
    skip_vars(reader, STRINGS_BEFORE_LOCATIONS + LOCATIONS * VARS_PER_LOCATION + 1);
    reader.read_u8().unwrap();
    skip_vars(reader, STRINGS_AFTER_FLAGS);
}

fn skip_vars(reader: &mut ByteReader, count: usize) {
    for _ in 0..count {
        reader.read_var().unwrap();
    }
}

#[test]
fn decodes_synthetic_v21_shard_with_tags() {
    // The system header shard has no symbols, so it would not exercise the insert.
    for file_name in [TU_SHARD, HEADER_SHARD] {
        let v20_bytes = read_fixture(file_name);
        let v20 = decode(&v20_bytes);

        let mut v21 = decode(&as_v21_with_tags(&v20_bytes, SYNTHETIC_TAGS));

        assert!(!v20.symbols.is_empty(), "{file_name}");
        assert_eq!(v21.version, TAGS_VERSION, "{file_name}");
        assert!(v21.symbols.iter().all(|symbol| symbol.tags == SYNTHETIC_TAGS), "{file_name}");
        v21.version = v20.version;
        v21.symbols.iter_mut().for_each(|symbol| symbol.tags = 0);
        assert_eq!(v21, v20, "{file_name}");
    }
}

#[test]
fn relocates_synthetic_v21_shard_keeping_tags() {
    let v21_bytes = as_v21_with_tags(&read_fixture(TU_SHARD), SYNTHETIC_TAGS);

    let relocated = relocate_shard(&os(TU_SHARD), &v21_bytes, &path_map(SAMPLE_ROOT, MOVED_ROOT), TableCompression::Fast).unwrap();

    let contents = decode(&relocated.bytes);
    assert_eq!(contents.version, TAGS_VERSION);
    assert!(contents.symbols.iter().all(|symbol| symbol.tags == SYNTHETIC_TAGS));
}

#[test]
fn rejects_unsupported_version_before_self_check() {
    let old_bytes = with_version(&read_fixture(TU_SHARD), UNSUPPORTED_VERSION);

    let result =
        relocate_shard(&os("tu0.cpp.0000000000000000.idx"), &old_bytes, &path_map(SAMPLE_ROOT, MOVED_ROOT), TableCompression::Fast);

    let error = result.unwrap_err();
    assert_eq!(error.downcast_ref::<UnsupportedVersionError>().map(|unsupported| unsupported.version), Some(UNSUPPORTED_VERSION));
}
