//! A clangd background-index shard file (`clangd/index/Serialization.cpp`, RIFF type `CdIx`).

use anyhow::{Context, Result, bail};

use super::bytes::ByteReader;
use super::digest::FileDigest;
use super::riff::{FourCc, RiffFile, parse_riff, write_riff};
use super::string_table::{StringTable, TableCompression, decode_string_table, encode_string_table};

/// Format versions this tool was verified against: 20 (clangd 23.1 release) and 21 (adds `Symbol::Tags`).
pub const SUPPORTED_VERSIONS: [u32; 2] = [20, 21];
/// First version that serializes `Symbol::Tags`.
pub const TAGS_VERSION: u32 = 21;

const INDEX_FILE_TYPE: &FourCc = b"CdIx";
pub const META_CHUNK: &FourCc = b"meta";
pub const STRINGS_CHUNK: &FourCc = b"stri";
pub const SYMBOLS_CHUNK: &FourCc = b"symb";
pub const REFS_CHUNK: &FourCc = b"refs";
pub const RELATIONS_CHUNK: &FourCc = b"rela";
pub const SOURCES_CHUNK: &FourCc = b"srcs";
pub const COMMAND_CHUNK: &FourCc = b"cmdl";

/// `IncludeGraphNode::SourceFlag` bits.
pub const SOURCE_FLAG_IS_TU: u8 = 1;
pub const SOURCE_FLAG_HAD_ERRORS: u8 = 2;

/// One `srcs` entry; strings are string-table indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncludeGraphNode {
    pub flags: u8,
    pub uri: u32,
    pub digest: FileDigest,
    pub direct_includes: Vec<u32>,
}

/// The `cmdl` chunk; strings are string-table indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileCommandIndices {
    pub directory: u32,
    pub arguments: Vec<u32>,
}

pub struct ShardFile<'a> {
    riff: RiffFile<'a>,
    pub version: u32,
    pub strings: StringTable,
}

impl<'a> ShardFile<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let riff = parse_riff(bytes)?;
        if &riff.file_type != INDEX_FILE_TYPE {
            bail!("not a clangd index file (type {})", String::from_utf8_lossy(&riff.file_type));
        }
        let version = read_version(&riff)?;
        let strings = decode_string_table(riff.chunk(STRINGS_CHUNK).context("missing stri chunk")?)?;
        Ok(Self { riff, version, strings })
    }

    pub fn chunk(&self, id: &FourCc) -> Option<&[u8]> {
        self.riff.chunk(id)
    }

    pub fn sources(&self) -> Result<Vec<IncludeGraphNode>> {
        let Some(chunk) = self.chunk(SOURCES_CHUNK) else {
            return Ok(Vec::new());
        };
        let mut reader = ByteReader::new(chunk);
        let mut nodes = Vec::new();
        while !reader.is_empty() {
            nodes.push(read_include_graph_node(&mut reader)?);
        }
        Ok(nodes)
    }

    pub fn command(&self) -> Result<Option<CompileCommandIndices>> {
        let Some(chunk) = self.chunk(COMMAND_CHUNK) else {
            return Ok(None);
        };
        let mut reader = ByteReader::new(chunk);
        let directory = reader.read_var()?;
        let count = reader.read_count()?;
        let arguments = (0..count).map(|_| reader.read_var()).collect::<Result<Vec<u32>>>()?;
        Ok(Some(CompileCommandIndices { directory, arguments }))
    }

    /// Serializes with the current string table; every other chunk is written back unchanged.
    pub fn encode(&mut self, compression: TableCompression) -> Result<Vec<u8>> {
        let strings = encode_string_table(&self.strings, compression)?;
        self.riff.replace_chunk(STRINGS_CHUNK, strings);
        Ok(write_riff(&self.riff))
    }
}

fn read_version(riff: &RiffFile) -> Result<u32> {
    let meta = riff.chunk(META_CHUNK).context("missing meta chunk")?;
    let version_bytes = meta.first_chunk::<4>().context("meta chunk truncated")?;
    Ok(u32::from_le_bytes(*version_bytes))
}

fn read_include_graph_node(reader: &mut ByteReader) -> Result<IncludeGraphNode> {
    let flags = reader.read_u8()?;
    let uri = reader.read_var()?;
    let digest: FileDigest = reader.read_array()?;
    let count = reader.read_count()?;
    let direct_includes = (0..count).map(|_| reader.read_var()).collect::<Result<Vec<u32>>>()?;
    Ok(IncludeGraphNode { flags, uri, digest, direct_includes })
}
