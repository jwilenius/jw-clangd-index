//! Full decode of a shard into resolved, comparable values, for `dump` and `diff`.
//!
//! Field order follows `readSymbol`, `readRefs`, `readRelation`, `readIncludeGraphNode` and
//! `readCompileCommand` in `clangd/index/Serialization.cpp` (format versions 20 and 21).

use anyhow::{Context, Result};
use serde::Serialize;

use super::bytes::ByteReader;
use super::digest::digest_hex;
use super::shard::{REFS_CHUNK, RELATIONS_CHUNK, SYMBOLS_CHUNK, ShardFile, TAGS_VERSION};
use super::string_table::StringTable;

const SYMBOL_ID_SIZE: usize = 8;
const INCLUDE_DIRECTIVES_BITS: u32 = 2;
const INCLUDE_DIRECTIVES_MASK: u32 = 0b11;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Location {
    pub file_uri: String,
    pub start: (u32, u32),
    pub end: (u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct IncludeHeader {
    pub header: String,
    pub references: u32,
    pub directives: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Symbol {
    pub id: String,
    pub kind: u8,
    pub lang: u8,
    pub name: String,
    pub scope: String,
    pub template_specialization_args: String,
    pub definition: Location,
    pub canonical_declaration: Location,
    pub references: u32,
    pub flags: u8,
    pub signature: String,
    pub completion_snippet_suffix: String,
    pub documentation: String,
    pub return_type: String,
    pub type_name: String,
    pub tags: u32,
    pub include_headers: Vec<IncludeHeader>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Ref {
    pub symbol: String,
    pub kind: u8,
    pub location: Location,
    pub container: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Relation {
    pub subject: String,
    pub predicate: u8,
    pub object: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Source {
    pub uri: String,
    pub flags: u8,
    pub digest: String,
    pub direct_includes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CompileCommand {
    pub directory: String,
    pub arguments: Vec<String>,
}

/// Shard contents in a canonical order (every list sorted), so equal indexes compare equal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ShardContents {
    pub version: u32,
    pub symbols: Vec<Symbol>,
    pub refs: Vec<Ref>,
    pub relations: Vec<Relation>,
    pub sources: Vec<Source>,
    pub command: Option<CompileCommand>,
}

pub fn decode_shard_contents(shard: &ShardFile) -> Result<ShardContents> {
    let strings = &shard.strings;
    let mut symbols =
        read_records(shard.chunk(SYMBOLS_CHUNK), |reader| read_symbol(reader, strings, shard.version)).context("symb chunk")?;
    let mut refs: Vec<Ref> = read_records(shard.chunk(REFS_CHUNK), |reader| read_ref_group(reader, strings))
        .context("refs chunk")?
        .into_iter()
        .flatten()
        .collect();
    let mut relations = read_records(shard.chunk(RELATIONS_CHUNK), read_relation).context("rela chunk")?;
    let mut sources = resolve_sources(shard).context("srcs chunk")?;
    let command = resolve_command(shard).context("cmdl chunk")?;

    symbols.sort();
    refs.sort();
    relations.sort();
    sources.sort();
    Ok(ShardContents { version: shard.version, symbols, refs, relations, sources, command })
}

fn read_records<T>(chunk: Option<&[u8]>, mut read: impl FnMut(&mut ByteReader) -> Result<T>) -> Result<Vec<T>> {
    let mut reader = ByteReader::new(chunk.unwrap_or_default());
    let mut records = Vec::new();
    while !reader.is_empty() {
        records.push(read(&mut reader)?);
    }
    Ok(records)
}

fn read_string(reader: &mut ByteReader, strings: &StringTable) -> Result<String> {
    let index = reader.read_var()?;
    Ok(String::from_utf8_lossy(strings.get(index)?).into_owned())
}

fn read_symbol_id(reader: &mut ByteReader) -> Result<String> {
    let id: [u8; SYMBOL_ID_SIZE] = reader.read_array()?;
    Ok(digest_hex(&id))
}

fn read_location(reader: &mut ByteReader, strings: &StringTable) -> Result<Location> {
    let file_uri = read_string(reader, strings)?;
    let start = (reader.read_var()?, reader.read_var()?);
    let end = (reader.read_var()?, reader.read_var()?);
    Ok(Location { file_uri, start, end })
}

fn read_symbol(reader: &mut ByteReader, strings: &StringTable, version: u32) -> Result<Symbol> {
    let id = read_symbol_id(reader)?;
    let kind = reader.read_u8()?;
    let lang = reader.read_u8()?;
    let name = read_string(reader, strings)?;
    let scope = read_string(reader, strings)?;
    let template_specialization_args = read_string(reader, strings)?;
    let definition = read_location(reader, strings)?;
    let canonical_declaration = read_location(reader, strings)?;
    let references = reader.read_var()?;
    let flags = reader.read_u8()?;
    let signature = read_string(reader, strings)?;
    let completion_snippet_suffix = read_string(reader, strings)?;
    let documentation = read_string(reader, strings)?;
    let return_type = read_string(reader, strings)?;
    let type_name = read_string(reader, strings)?;
    let tags = if version >= TAGS_VERSION { reader.read_var()? } else { 0 };
    let header_count = reader.read_count()?;
    let mut include_headers = (0..header_count).map(|_| read_include_header(reader, strings)).collect::<Result<Vec<_>>>()?;
    include_headers.sort();
    Ok(Symbol {
        id,
        kind,
        lang,
        name,
        scope,
        template_specialization_args,
        definition,
        canonical_declaration,
        references,
        flags,
        signature,
        completion_snippet_suffix,
        documentation,
        return_type,
        type_name,
        tags,
        include_headers,
    })
}

fn read_include_header(reader: &mut ByteReader, strings: &StringTable) -> Result<IncludeHeader> {
    let header = read_string(reader, strings)?;
    let packed = reader.read_var()?;
    Ok(IncludeHeader { header, references: packed >> INCLUDE_DIRECTIVES_BITS, directives: packed & INCLUDE_DIRECTIVES_MASK })
}

fn read_ref_group(reader: &mut ByteReader, strings: &StringTable) -> Result<Vec<Ref>> {
    let symbol = read_symbol_id(reader)?;
    let count = reader.read_count()?;
    (0..count)
        .map(|_| {
            let kind = reader.read_u8()?;
            let location = read_location(reader, strings)?;
            let container = read_symbol_id(reader)?;
            Ok(Ref { symbol: symbol.clone(), kind, location, container })
        })
        .collect()
}

fn read_relation(reader: &mut ByteReader) -> Result<Relation> {
    let subject = read_symbol_id(reader)?;
    let predicate = reader.read_u8()?;
    let object = read_symbol_id(reader)?;
    Ok(Relation { subject, predicate, object })
}

fn resolve_string(strings: &StringTable, index: u32) -> Result<String> {
    Ok(String::from_utf8_lossy(strings.get(index)?).into_owned())
}

fn resolve_sources(shard: &ShardFile) -> Result<Vec<Source>> {
    let strings = &shard.strings;
    shard
        .sources()?
        .into_iter()
        .map(|node| {
            let mut direct_includes =
                node.direct_includes.iter().map(|&index| resolve_string(strings, index)).collect::<Result<Vec<_>>>()?;
            direct_includes.sort();
            Ok(Source { uri: resolve_string(strings, node.uri)?, flags: node.flags, digest: digest_hex(&node.digest), direct_includes })
        })
        .collect()
}

fn resolve_command(shard: &ShardFile) -> Result<Option<CompileCommand>> {
    let Some(command) = shard.command()? else {
        return Ok(None);
    };
    let strings = &shard.strings;
    let arguments = command.arguments.iter().map(|&index| resolve_string(strings, index)).collect::<Result<Vec<_>>>()?;
    Ok(Some(CompileCommand { directory: resolve_string(strings, command.directory)?, arguments }))
}
