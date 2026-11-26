//! The `diff` command: compares two index directories shard by shard after relocating the first.
//!
//! Used to check a relocated index against one clangd built in the target itself.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;

use crate::format::shard::{SUPPORTED_VERSIONS, ShardFile};
use crate::format::shard_contents::{ShardContents, decode_shard_contents};
use crate::format::string_table::TableCompression;
use crate::relocate::index_relocation::list_shards;
use crate::relocate::path_map::PathMap;
use crate::relocate::shard_relocation::{UnsupportedVersionError, relocate_shard};

pub struct IndexDiffRequest {
    pub dir_a: PathBuf,
    pub dir_b: PathBuf,
    /// Applied to the shards of `dir_a` before comparing.
    pub path_map: PathMap,
    /// Ignore the compile commands, which differ between machines or build directories.
    pub is_ignore_command: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShardPart {
    Version,
    Symbols,
    Refs,
    Relations,
    Sources,
    Command,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShardDifference {
    pub file_name: OsString,
    pub parts: Vec<ShardPart>,
}

/// A shard left out of the comparison because this tool cannot decode its format version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedShard {
    pub path: PathBuf,
    pub version: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexDiffReport {
    /// Relocated shard names of `dir_a` that `dir_b` lacks, sorted.
    pub only_in_a: Vec<OsString>,
    pub only_in_b: Vec<OsString>,
    pub differing: Vec<ShardDifference>,
    /// Shards present on both sides.
    pub compared: usize,
    /// Shards of either side with an unsupported format version, sorted by path.
    pub skipped: Vec<SkippedShard>,
}

impl IndexDiffReport {
    pub fn is_equal(&self) -> bool {
        self.only_in_a.is_empty() && self.only_in_b.is_empty() && self.differing.is_empty()
    }
}

enum ShardDecoding {
    Decoded { file_name: OsString, contents: ShardContents },
    Skipped(SkippedShard),
}

#[derive(Default)]
struct DecodedIndex {
    shards: BTreeMap<OsString, ShardContents>,
    skipped: Vec<SkippedShard>,
}

/// Fails when a shard of either side cannot be read, relocated or decoded; shards of an unsupported
/// format version are skipped and reported instead.
pub fn diff_indexes(request: &IndexDiffRequest) -> Result<IndexDiffReport> {
    let index_a = decode_relocated_index(&request.dir_a, &request.path_map)?;
    let index_b = decode_index(&request.dir_b)?;
    let shards_a = &index_a.shards;
    let shards_b = &index_b.shards;
    let mut skipped: Vec<SkippedShard> = index_a.skipped.into_iter().chain(index_b.skipped).collect();
    skipped.sort_by(|left, right| left.path.cmp(&right.path));
    let mut report = IndexDiffReport {
        only_in_a: shards_a.keys().filter(|name| !shards_b.contains_key(*name)).cloned().collect(),
        only_in_b: shards_b.keys().filter(|name| !shards_a.contains_key(*name)).cloned().collect(),
        skipped,
        ..IndexDiffReport::default()
    };
    for (file_name, contents_a) in shards_a {
        let Some(contents_b) = shards_b.get(file_name) else {
            continue;
        };
        report.compared += 1;
        let parts = compare_contents(contents_a, contents_b, request.is_ignore_command);
        if !parts.is_empty() {
            report.differing.push(ShardDifference { file_name: file_name.clone(), parts });
        }
    }
    Ok(report)
}

/// The parts in which `a` and `b` differ, in [`ShardPart`] order.
pub fn compare_contents(a: &ShardContents, b: &ShardContents, is_ignore_command: bool) -> Vec<ShardPart> {
    let comparisons = [
        (ShardPart::Version, a.version == b.version),
        (ShardPart::Symbols, a.symbols == b.symbols),
        (ShardPart::Refs, a.refs == b.refs),
        (ShardPart::Relations, a.relations == b.relations),
        (ShardPart::Sources, a.sources == b.sources),
        (ShardPart::Command, is_ignore_command || a.command == b.command),
    ];
    comparisons.into_iter().filter(|(_, is_equal)| !is_equal).map(|(part, _)| part).collect()
}

/// Shards of `dir` relocated in memory, keyed by their relocated file name.
fn decode_relocated_index(dir: &Path, path_map: &PathMap) -> Result<DecodedIndex> {
    let decodings = list_shards(dir)?
        .par_iter()
        .map(|shard_path| decode_relocated_shard(shard_path, path_map).with_context(|| shard_path.display().to_string()))
        .collect::<Result<Vec<ShardDecoding>>>()?;
    collect_index(dir, decodings)
}

fn decode_relocated_shard(shard_path: &Path, path_map: &PathMap) -> Result<ShardDecoding> {
    let file_name = shard_path.file_name().context("shard path has no file name")?;
    let bytes = fs::read(shard_path).context("reading shard")?;
    let relocated = match relocate_shard(file_name, &bytes, path_map, TableCompression::None) {
        Ok(relocated) => relocated,
        Err(error) => {
            let Some(unsupported) = error.downcast_ref::<UnsupportedVersionError>() else {
                return Err(error);
            };
            return Ok(ShardDecoding::Skipped(SkippedShard { path: shard_path.to_path_buf(), version: unsupported.version }));
        }
    };
    let contents = decode_shard_contents(&ShardFile::parse(&relocated.bytes)?)?;
    Ok(ShardDecoding::Decoded { file_name: relocated.file_name, contents })
}

fn decode_index(dir: &Path) -> Result<DecodedIndex> {
    let decodings = list_shards(dir)?
        .par_iter()
        .map(|shard_path| decode_shard(shard_path).with_context(|| shard_path.display().to_string()))
        .collect::<Result<Vec<ShardDecoding>>>()?;
    collect_index(dir, decodings)
}

fn decode_shard(shard_path: &Path) -> Result<ShardDecoding> {
    let file_name = shard_path.file_name().context("shard path has no file name")?;
    let bytes = fs::read(shard_path).context("reading shard")?;
    let shard = ShardFile::parse(&bytes)?;
    let is_supported_version = SUPPORTED_VERSIONS.contains(&shard.version);
    if !is_supported_version {
        return Ok(ShardDecoding::Skipped(SkippedShard { path: shard_path.to_path_buf(), version: shard.version }));
    }
    let contents = decode_shard_contents(&shard)?;
    Ok(ShardDecoding::Decoded { file_name: file_name.to_owned(), contents })
}

/// Fails when two shards of `dir` have the same (relocated) name.
fn collect_index(dir: &Path, decodings: Vec<ShardDecoding>) -> Result<DecodedIndex> {
    let mut index = DecodedIndex::default();
    for decoding in decodings {
        match decoding {
            ShardDecoding::Skipped(skipped) => index.skipped.push(skipped),
            ShardDecoding::Decoded { file_name, contents } => {
                if index.shards.insert(file_name.clone(), contents).is_some() {
                    bail!("two shards of {} relocate to the same name {}", dir.display(), file_name.to_string_lossy());
                }
            }
        }
    }
    Ok(index)
}

impl fmt::Display for ShardPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            ShardPart::Version => "version",
            ShardPart::Symbols => "symbols",
            ShardPart::Refs => "refs",
            ShardPart::Relations => "relations",
            ShardPart::Sources => "sources",
            ShardPart::Command => "command",
        };
        formatter.write_str(name)
    }
}

impl fmt::Display for IndexDiffReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for file_name in &self.only_in_a {
            writeln!(formatter, "only in A: {}", file_name.to_string_lossy())?;
        }
        for file_name in &self.only_in_b {
            writeln!(formatter, "only in B: {}", file_name.to_string_lossy())?;
        }
        for difference in &self.differing {
            let parts: Vec<String> = difference.parts.iter().map(ShardPart::to_string).collect();
            writeln!(formatter, "differs:   {} ({})", difference.file_name.to_string_lossy(), parts.join(", "))?;
        }
        for skipped in &self.skipped {
            writeln!(formatter, "skipped:   {} (unsupported format version {})", skipped.path.display(), skipped.version)?;
        }
        let verdict = if self.is_equal() { "indexes are equal" } else { "indexes differ" };
        writeln!(
            formatter,
            "{verdict}: {} shards compared, {} differ, {} only in A, {} only in B",
            self.compared,
            self.differing.len(),
            self.only_in_a.len(),
            self.only_in_b.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::shard_contents::{CompileCommand, Relation};

    fn contents() -> ShardContents {
        ShardContents {
            version: 20,
            symbols: Vec::new(),
            refs: Vec::new(),
            relations: Vec::new(),
            sources: Vec::new(),
            command: Some(CompileCommand { directory: "/w/build".to_string(), arguments: vec!["c++".to_string()] }),
        }
    }

    #[test]
    fn finds_no_difference_in_equal_contents() {
        assert_eq!(compare_contents(&contents(), &contents(), false), Vec::new());
    }

    #[test]
    fn names_each_differing_part() {
        let mut other = contents();
        other.version = 21;
        other.relations.push(Relation { subject: "a".to_string(), predicate: 0, object: "b".to_string() });
        other.command = None;

        assert_eq!(compare_contents(&contents(), &other, false), vec![ShardPart::Version, ShardPart::Relations, ShardPart::Command]);
    }

    #[test]
    fn ignores_command_when_asked() {
        let mut other = contents();
        other.command = None;

        assert_eq!(compare_contents(&contents(), &other, true), Vec::new());
    }

    #[test]
    fn report_is_equal_only_without_any_difference() {
        let only_in_b = IndexDiffReport { only_in_b: vec![OsString::from("a.h.0000000000000000.idx")], ..IndexDiffReport::default() };

        assert!(IndexDiffReport::default().is_equal());
        assert!(!only_in_b.is_equal());
    }
}
