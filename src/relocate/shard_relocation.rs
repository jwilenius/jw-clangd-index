//! Relocates one shard: rewrites every string-table entry through a [`PathMap`] and names the result
//! after the shard's mapped source file.
//!
//! Only `stri` and the RIFF container change; every other chunk refers to strings by position, so
//! it is copied byte for byte.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::path_map::PathMap;
use crate::format::digest::{DIGEST_SIZE, digest, digest_hex};
use crate::format::shard::{IncludeGraphNode, SUPPORTED_VERSIONS, ShardFile};
use crate::format::string_table::{StringTable, TableCompression};
use crate::format::uri::file_uri_to_path;

const SHARD_EXTENSION: &[u8] = b".idx";
const NAME_SEPARATOR: u8 = b'.';
const PATH_SEPARATOR: u8 = b'/';
const DIGEST_HEX_LENGTH: usize = DIGEST_SIZE * 2;

#[derive(Clone, Debug)]
pub struct RelocatedShard {
    /// `<basename>.<HEX>.idx` of the mapped source path, as clangd names shards.
    pub file_name: OsString,
    /// Absolute path of the shard's own source file after mapping.
    pub source_path: PathBuf,
    pub bytes: Vec<u8>,
    /// String-table entries the path map changed; 0 when no path is under a mapped root.
    pub rewritten_strings: usize,
}

/// No include-graph node hashes to the shard's file name, so this tool's path hash differs from the
/// clangd that wrote the shard and every relocated name would be wrong.
#[derive(Debug)]
pub struct HashSelfCheckError {
    pub file_name: OsString,
}

impl fmt::Display for HashSelfCheckError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "no source in {} hashes to its file name; clangd's path hash differs from ours", self.file_name.to_string_lossy())
    }
}

impl std::error::Error for HashSelfCheckError {}

/// The shard's format version is not in [`SUPPORTED_VERSIONS`], so it is not relocated: its path hash
/// may differ from ours, and an old orphaned shard must not fail the self-check for the whole run.
#[derive(Debug)]
pub struct UnsupportedVersionError {
    pub file_name: OsString,
    pub version: u32,
}

impl fmt::Display for UnsupportedVersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} has unsupported format version {}", self.file_name.to_string_lossy(), self.version)
    }
}

impl std::error::Error for UnsupportedVersionError {}

/// `file_name` is the donor shard's name. The version is checked before the self-check: an unsupported
/// version fails with [`UnsupportedVersionError`], a missing self node with [`HashSelfCheckError`].
pub fn relocate_shard(file_name: &OsStr, bytes: &[u8], path_map: &PathMap, compression: TableCompression) -> Result<RelocatedShard> {
    let shard_name = ShardName::parse(file_name.as_bytes())?;
    let mut shard = ShardFile::parse(bytes)?;
    let is_supported_version = SUPPORTED_VERSIONS.contains(&shard.version);
    if !is_supported_version {
        return Err(UnsupportedVersionError { file_name: file_name.to_owned(), version: shard.version }.into());
    }
    let self_node = find_self_node(&shard, &shard_name)?.ok_or_else(|| HashSelfCheckError { file_name: file_name.to_owned() })?;

    let rewritten_strings = rewrite_strings(&mut shard.strings, path_map);
    let source_path = file_uri_to_path(shard.strings.get(self_node.uri)?).context("mapped self URI is no longer a file URI")?;
    Ok(RelocatedShard {
        file_name: shard_file_name(&source_path),
        bytes: shard.encode(compression)?,
        source_path: PathBuf::from(OsString::from_vec(source_path)),
        rewritten_strings,
    })
}

/// The parts of `<basename>.<HEX>.idx`.
struct ShardName<'a> {
    basename: &'a [u8],
    path_hex: &'a [u8],
}

impl<'a> ShardName<'a> {
    fn parse(file_name: &'a [u8]) -> Result<Self> {
        let invalid = || format!("not a shard file name: {}", String::from_utf8_lossy(file_name));
        let stem = file_name.strip_suffix(SHARD_EXTENSION).with_context(invalid)?;
        let separator = stem.iter().rposition(|&byte| byte == NAME_SEPARATOR).with_context(invalid)?;
        let (basename, path_hex) = (&stem[..separator], &stem[separator + 1..]);
        if path_hex.len() != DIGEST_HEX_LENGTH {
            bail!(invalid());
        }
        Ok(Self { basename, path_hex })
    }
}

/// The include-graph node of the shard's own source file, as found by its donor file name `file_name`;
/// `None` when no node matches (see [`HashSelfCheckError`]).
pub fn find_shard_self_node(file_name: &OsStr, shard: &ShardFile) -> Result<Option<IncludeGraphNode>> {
    let shard_name = ShardName::parse(file_name.as_bytes())?;
    find_self_node(shard, &shard_name)
}

/// The node whose path has the file name's basename and hashes to its hex.
fn find_self_node(shard: &ShardFile, shard_name: &ShardName) -> Result<Option<IncludeGraphNode>> {
    for node in shard.sources()? {
        let Some(path) = file_uri_to_path(shard.strings.get(node.uri)?) else {
            continue;
        };
        let is_self = basename(&path) == shard_name.basename && digest_hex(&digest(&path)).as_bytes() == shard_name.path_hex;
        if is_self {
            return Ok(Some(node));
        }
    }
    Ok(None)
}

/// Returns how many strings changed.
fn rewrite_strings(strings: &mut StringTable, path_map: &PathMap) -> usize {
    let mut rewritten_strings = 0;
    for string in &mut strings.strings {
        if let Some(rewritten) = path_map.rewrite(string) {
            *string = rewritten;
            rewritten_strings += 1;
        }
    }
    rewritten_strings
}

fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&byte| byte == PATH_SEPARATOR).next().unwrap_or(path)
}

/// `<basename>.<HEX>.idx`, the name clangd gives the shard of the file at absolute `source_path`.
pub fn shard_file_name(source_path: &[u8]) -> OsString {
    let path_hex = digest_hex(&digest(source_path));
    let name = [basename(source_path), &[NAME_SEPARATOR], path_hex.as_bytes(), SHARD_EXTENSION].concat();
    OsString::from_vec(name)
}
