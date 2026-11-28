//! Predicts which translation units clangd reindexes when it loads a relocated index.
//!
//! Mirrors `BackgroundIndexLoader.cpp` and `BackgroundIndex::loadProject` in `clangd/index/Background.cpp`:
//! - clangd loads the main files of its compilation database, each walking the include graph
//!   breadth-first across shards; a file belongs to the first main file that reaches it, and later
//!   walks stop at it;
//! - a file is stale when it is readable and the digest of its contents differs from the one in its
//!   own shard (a missing shard always differs); unreadable (deleted) files are never stale;
//! - for each stale file clangd reindexes only the main file it belongs to, not every includer;
//! - the shards of files outside the project, such as system headers, live in the user's index (see
//!   [`load_external_nodes`]), and those of files under a nested directory with its own compilation
//!   database, such as an in-tree `build/`, in that directory's index (see [`load_nested_project_nodes`]).
//!
//! The walk order decides which main file owns a file shared by several. clangd walks the database's
//! files in the iteration order of `JSONCompilationDatabase`'s `StringMap`, which
//! [`load_cdb_main_files`] reproduces; without a database the TUs of the index are walked in path order.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::ops::RangeInclusive;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::Deserialize;

use super::index_relocation::{INDEX_RELATIVE_DIR, list_shards};
use super::path_map::PathMap;
use super::shard_relocation::{find_shard_self_node, shard_file_name};
use super::string_map_order::{StringMapProbing, string_map_order};
use crate::format::digest::{FileDigest, digest};
use crate::format::shard::{SOURCE_FLAG_IS_TU, SUPPORTED_VERSIONS, ShardFile, read_shard_version};
use crate::format::uri::file_uri_to_path;

/// One shard's own source file in the cross-shard include graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceNode {
    pub is_tu: bool,
    /// Digest of the file contents when the shard was written.
    pub digest: FileDigest,
    /// The other sources the shard lists: the files this one includes directly.
    pub includes: Vec<PathBuf>,
    /// Index format version of the shard, which narrows down which clangd wrote it.
    pub format_version: u32,
    /// Major version of the clangd that wrote the shard, from the resource directory in its compile
    /// command; `None` for a shard without one, such as a header's.
    pub clangd_major: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IncludeGraph {
    /// Keyed by absolute source path; a file without a shard has no node.
    pub nodes: HashMap<PathBuf, SourceNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReindexPrediction {
    /// Main files that own a stale file, sorted by path.
    pub reindexed_tus: Vec<PathBuf>,
    /// Stale files that some main file's include graph reaches, except those in `main_files_without_shard`.
    pub stale_files: usize,
    /// Readable main files without a shard, sorted by path; clangd indexes them from scratch.
    pub main_files_without_shard: Vec<PathBuf>,
}

/// The newest index format version that only clangd versions with quadratic probing write: 19 is written
/// by clangd 18 and 19.
const LAST_QUADRATIC_PROBING_FORMAT_VERSION: u32 = 19;
/// The index format version that clangd 20 to 23 write, so it does not tell their probings apart.
const AMBIGUOUS_PROBING_FORMAT_VERSION: u32 = 20;
/// The clangd majors that write [`AMBIGUOUS_PROBING_FORMAT_VERSION`] and probe quadratically; llvm-project
/// a296bdef76 ("Replace tombstone deletion with TAOCP 6.4 Algorithm R") switched
/// `llvm/lib/Support/StringMap.cpp` to linear probing for LLVM 23.
const QUADRATIC_PROBING_FORMAT_20_CLANGD_MAJORS: RangeInclusive<u32> = 20..=22;
/// The newest clangd major that writes [`AMBIGUOUS_PROBING_FORMAT_VERSION`].
const LAST_FORMAT_20_CLANGD_MAJOR: u32 = 23;
/// The compile command flag that clangd's `CommandMangler` adds (`clangd/CompileCommands.cpp`).
const RESOURCE_DIR_FLAG_PREFIX: &[u8] = b"-resource-dir=";

/// The fields of a compilation database entry that locate its main file.
#[derive(Deserialize)]
struct CdbEntry {
    directory: PathBuf,
    file: PathBuf,
}

/// Graph of the shards in `shard_dir` with every path mapped through `path_map`; shards that cannot be
/// read are left out, as clangd treats them as missing.
pub fn load_include_graph(shard_dir: &Path, path_map: &PathMap) -> Result<IncludeGraph> {
    let shards = list_shards(shard_dir)?;
    let nodes = shards.par_iter().filter_map(|shard_path| read_graph_entry(shard_path, path_map)).collect();
    Ok(IncludeGraph { nodes })
}

/// The nodes of the files outside `project_root` that the walks from `graph` reach, read from the user's
/// index `user_index_dir`; a file without a shard there has no node.
///
/// clangd's `BackgroundIndexStorage` factory picks each file's index by the file's own path: a file of
/// the project goes to `<project root>/.cache/clangd/index`, any other file, such as a system header, to
/// `<cache directory>/clangd/index`. A file outside `project_root` counts as outside every project, even
/// one under another compilation database, such as an out-of-tree build directory.
pub fn load_external_nodes(graph: &IncludeGraph, project_root: &Path, user_index_dir: &Path) -> Result<HashMap<PathBuf, SourceNode>> {
    load_index_nodes(graph, user_index_dir, |path| !path.starts_with(project_root))
}

/// Replaces the nodes of the files under `nested_dir`, a directory of the project with a compilation
/// database of its own, by those in `<nested_dir>/.cache/clangd/index` that the walks from `graph` reach.
///
/// clangd assigns each file to the nearest directory with a compilation database and keeps its shard
/// in that directory's index, so it never reads such a file's shard from the project's index. A file
/// without a shard there has no node, so it is stale when readable.
pub fn load_nested_project_nodes(graph: &mut IncludeGraph, nested_dir: &Path) -> Result<()> {
    graph.nodes.retain(|path, _| !path.starts_with(nested_dir));
    let nested_nodes = load_index_nodes(graph, &nested_dir.join(INDEX_RELATIVE_DIR), |path| path.starts_with(nested_dir))?;
    graph.nodes.extend(nested_nodes);
    Ok(())
}

/// The nodes, read from `index_dir`, of the files that `is_in_index` accepts and the walks from `graph`
/// reach; a file without a shard there has no node.
fn load_index_nodes(graph: &IncludeGraph, index_dir: &Path, is_in_index: impl Fn(&Path) -> bool) -> Result<HashMap<PathBuf, SourceNode>> {
    if !index_dir.is_dir() {
        return Ok(HashMap::new());
    }
    let identity_map = PathMap::new(&[])?;
    let mut index_nodes: HashMap<PathBuf, SourceNode> = HashMap::new();
    let mut tried: HashSet<PathBuf> = HashSet::new();
    let mut frontier = unknown_includes(graph.nodes.values(), graph, &is_in_index, &tried);
    while !frontier.is_empty() {
        let loaded: Vec<(PathBuf, SourceNode)> =
            frontier.par_iter().filter_map(|path| read_index_shard(path, index_dir, &identity_map)).collect();
        tried.extend(frontier);
        frontier = unknown_includes(loaded.iter().map(|(_, node)| node), graph, &is_in_index, &tried);
        index_nodes.extend(loaded);
    }
    Ok(index_nodes)
}

/// The includes of `nodes` that `is_in_index` accepts, have no node in `graph` and were not `tried`.
fn unknown_includes<'a>(
    nodes: impl Iterator<Item = &'a SourceNode>,
    graph: &IncludeGraph,
    is_in_index: &impl Fn(&Path) -> bool,
    tried: &HashSet<PathBuf>,
) -> HashSet<PathBuf> {
    let includes = nodes.flat_map(|node| node.includes.iter());
    let is_unknown = |include: &&PathBuf| is_in_index(include) && !graph.nodes.contains_key(*include) && !tried.contains(*include);
    includes.filter(is_unknown).cloned().collect()
}

fn read_index_shard(path: &Path, index_dir: &Path, identity_map: &PathMap) -> Option<(PathBuf, SourceNode)> {
    let shard_path = index_dir.join(shard_file_name(path.as_os_str().as_bytes()));
    read_graph_entry(&shard_path, identity_map)
}

/// The absolute main file of every entry in the compilation database at `cdb_path`, with `directory`
/// and `file` mapped through `path_map` (unchanged when `None`) and `.`/`..` removed lexically, as clangd
/// does; each file once, in the order a clangd whose `StringMap` probes by `probing` loads them (see
/// [`string_map_order`]).
pub fn load_cdb_main_files(cdb_path: &Path, path_map: Option<&PathMap>, probing: StringMapProbing) -> Result<Vec<PathBuf>> {
    let json = fs::read(cdb_path).with_context(|| format!("reading {}", cdb_path.display()))?;
    let entries: Vec<CdbEntry> = serde_json::from_slice(&json).with_context(|| format!("parsing {}", cdb_path.display()))?;
    let main_files =
        entries.iter().map(|entry| normalize_lexically(&mapped(&entry.directory, path_map).join(mapped(&entry.file, path_map)))).collect();
    Ok(string_map_order(main_files, probing))
}

/// The `StringMap` probing of the clangd that wrote the newest TU shards of `graph`: by their format
/// version, and for format 20 by the newest clangd major that their compile commands name; LLVM 23's when
/// it has no TU shards. Header shards are left out: those from the user index may come from another clangd.
pub fn graph_string_map_probing(graph: &IncludeGraph) -> StringMapProbing {
    let tus: Vec<&SourceNode> = graph.nodes.values().filter(|node| node.is_tu).collect();
    let Some(newest_format_version) = tus.iter().map(|node| node.format_version).max() else {
        return StringMapProbing::Linear;
    };
    let newest_tus = tus.iter().filter(|node| node.format_version == newest_format_version);
    let newest_clangd_major = newest_tus.filter_map(|node| node.clangd_major).max();
    string_map_probing(newest_format_version, newest_clangd_major)
}

/// The probing of the clangd that writes index format `format_version` and, for format 20, has major
/// version `clangd_major`. A major that format 20 rules out, which only a compile command's own resource
/// directory can name, counts as unknown; with an unknown major format 20 counts as clangd 23's.
fn string_map_probing(format_version: u32, clangd_major: Option<u32>) -> StringMapProbing {
    if format_version <= LAST_QUADRATIC_PROBING_FORMAT_VERSION {
        return StringMapProbing::Quadratic;
    }
    if format_version > AMBIGUOUS_PROBING_FORMAT_VERSION {
        return StringMapProbing::Linear;
    }
    match clangd_major {
        Some(major) if QUADRATIC_PROBING_FORMAT_20_CLANGD_MAJORS.contains(&major) => StringMapProbing::Quadratic,
        Some(LAST_FORMAT_20_CLANGD_MAJOR) => StringMapProbing::Linear,
        _ => {
            log::warn!(
                "the format {AMBIGUOUS_PROBING_FORMAT_VERSION} TU shards name no resource directory of clangd 20 to 23; \
                 predicting with clangd 23's StringMap probing, which can attribute shared headers to other TUs than clangd 20 to 22 do"
            );
            StringMapProbing::Linear
        }
    }
}

/// `main_files` without those under `nested_dirs`: clangd loads only the main files that the database's
/// own directory governs, not those under a nearer one (the broadcast `Filter` in
/// `GlobalCompilationDatabase.cpp`).
pub fn without_nested_main_files(main_files: Vec<PathBuf>, nested_dirs: &[PathBuf]) -> Vec<PathBuf> {
    main_files.into_iter().filter(|file| !nested_dirs.iter().any(|dir| file.starts_with(dir))).collect()
}

/// The TUs with the `IsTU` flag, sorted by path, for when no compilation database lists the main files.
pub fn graph_tus(graph: &IncludeGraph) -> Vec<PathBuf> {
    let tus: BTreeSet<&PathBuf> = graph.nodes.iter().filter(|(_, node)| node.is_tu).map(|(path, _)| path).collect();
    tus.into_iter().cloned().collect()
}

/// Reads every file the walks from `main_files`, in their order, reach to compare its digest; see the
/// module docs.
pub fn predict_reindex(graph: &IncludeGraph, main_files: &[PathBuf]) -> ReindexPrediction {
    let roots: HashSet<&Path> = main_files.iter().map(PathBuf::as_path).collect();
    let dependent_tus = assign_dependent_tus(graph, main_files);
    let stale_files: Vec<&Path> =
        dependent_tus.par_iter().map(|(file, _)| *file).filter(|file| is_stale(file, graph.nodes.get(*file))).collect();
    for file in &stale_files {
        log::debug!("stale: {} (loaded by {})", file.display(), dependent_tus[file].display());
    }

    let (shardless_main_files, other_stale_files): (Vec<&Path>, Vec<&Path>) =
        stale_files.into_iter().partition(|file| roots.contains(file) && !graph.nodes.contains_key(*file));
    let reindexed_tus: BTreeSet<&Path> = other_stale_files.iter().map(|file| dependent_tus[file]).collect();
    let main_files_without_shard: BTreeSet<&Path> = shardless_main_files.into_iter().collect();
    ReindexPrediction {
        reindexed_tus: reindexed_tus.into_iter().map(Path::to_path_buf).collect(),
        stale_files: other_stale_files.len(),
        main_files_without_shard: main_files_without_shard.into_iter().map(Path::to_path_buf).collect(),
    }
}

/// Maps every reachable file, with or without a shard, to the main file that loads it first.
fn assign_dependent_tus<'a>(graph: &'a IncludeGraph, main_files: &'a [PathBuf]) -> HashMap<&'a Path, &'a Path> {
    let mut dependent_tus: HashMap<&Path, &Path> = HashMap::new();
    for tu in main_files.iter().map(PathBuf::as_path) {
        let mut queued: HashSet<&Path> = HashSet::from([tu]);
        let mut to_visit: VecDeque<&Path> = VecDeque::from([tu]);
        while let Some(file) = to_visit.pop_front() {
            // clangd caches loaded shards and returns no edges for them a second time.
            let is_loaded = dependent_tus.contains_key(file);
            if is_loaded {
                continue;
            }
            dependent_tus.insert(file, tu);
            let includes = graph.nodes.get(file).map(|node| node.includes.as_slice()).unwrap_or_default();
            for include in includes {
                if queued.insert(include) {
                    to_visit.push_back(include);
                }
            }
        }
    }
    dependent_tus
}

/// clangd's `shardIsStale`: an unreadable file is never stale, and a file without a shard has no
/// digest to match.
fn is_stale(file: &Path, node: Option<&SourceNode>) -> bool {
    let Ok(contents) = fs::read(file) else {
        return false;
    };
    node.is_none_or(|node| digest(&contents) != node.digest)
}

fn read_graph_entry(shard_path: &Path, path_map: &PathMap) -> Option<(PathBuf, SourceNode)> {
    match read_source_node(shard_path, path_map) {
        Ok(entry) => entry,
        Err(error) => {
            log::debug!("{}: left out of the include graph: {error:#}", shard_path.display());
            None
        }
    }
}

fn read_source_node(shard_path: &Path, path_map: &PathMap) -> Result<Option<(PathBuf, SourceNode)>> {
    let bytes = fs::read(shard_path).context("reading shard")?;
    let is_supported_version = SUPPORTED_VERSIONS.contains(&read_shard_version(&bytes)?);
    if !is_supported_version {
        return Ok(None);
    }
    let shard = ShardFile::parse(&bytes)?;
    let file_name = shard_path.file_name().context("shard path has no file name")?;
    let Some(self_node) = find_shard_self_node(file_name, &shard)? else {
        return Ok(None);
    };
    let Some(path) = mapped_uri_path(&shard, self_node.uri, path_map)? else {
        return Ok(None);
    };

    let mut includes = Vec::new();
    for node in shard.sources()?.iter().filter(|node| node.uri != self_node.uri) {
        includes.extend(mapped_uri_path(&shard, node.uri, path_map)?);
    }
    let is_tu = self_node.flags & SOURCE_FLAG_IS_TU != 0;
    // clangd's `readRIFF` rejects a shard whose compile command does not decode, so it counts as missing.
    let clangd_major = read_clangd_major(&shard)?;
    Ok(Some((path, SourceNode { is_tu, digest: self_node.digest, includes, format_version: shard.version, clangd_major })))
}

/// The clangd major of the shard's compile command, if it has one; see [`clangd_major_of_command`].
fn read_clangd_major(shard: &ShardFile) -> Result<Option<u32>> {
    let Some(command) = shard.command()? else {
        return Ok(None);
    };
    let arguments = command.arguments.iter().map(|&index| shard.strings.get(index)).collect::<Result<Vec<&[u8]>>>()?;
    Ok(clangd_major_of_command(&arguments))
}

/// The major version in the last `-resource-dir=<install>/lib/clang/<major>`, which clangd's
/// `CommandMangler` adds to a compile command that sets no resource directory itself. clangd never adds
/// the separate `-resource-dir <dir>` form, so a command with only that one names no clangd.
fn clangd_major_of_command(arguments: &[&[u8]]) -> Option<u32> {
    let resource_dir = arguments.iter().rev().find_map(|argument| argument.strip_prefix(RESOURCE_DIR_FLAG_PREFIX))?;
    clangd_major_of_resource_dir(resource_dir)
}

/// The number before the first `.` of the last component of `resource_dir`, such as 21 of
/// `/usr/lib/llvm-21/lib/clang/21` or of `/opt/llvm/lib/clang/21.1.0`.
fn clangd_major_of_resource_dir(resource_dir: &[u8]) -> Option<u32> {
    let version = Path::new(OsStr::from_bytes(resource_dir)).file_name()?;
    let major = version.as_bytes().split(|&byte| byte == b'.').next()?;
    std::str::from_utf8(major).ok()?.parse().ok()
}

/// The path of the `file:` URI at string index `uri` after mapping; `None` for other URI schemes.
fn mapped_uri_path(shard: &ShardFile, uri: u32, path_map: &PathMap) -> Result<Option<PathBuf>> {
    let original = shard.strings.get(uri)?;
    let mapped = path_map.rewrite(original);
    let path = file_uri_to_path(mapped.as_deref().unwrap_or(original));
    Ok(path.map(|bytes| PathBuf::from(OsString::from_vec(bytes))))
}

fn mapped(path: &Path, path_map: Option<&PathMap>) -> PathBuf {
    match path_map.and_then(|path_map| path_map.rewrite(path.as_os_str().as_bytes())) {
        Some(rewritten) => PathBuf::from(OsString::from_vec(rewritten)),
        None => path.to_path_buf(),
    }
}

/// `llvm::sys::path::remove_dots` with `..` removal: drops `.`, and `..` with the component before it.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => normalized.push(component),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::format::bytes::write_var;
    use crate::format::riff::{Chunk, RiffFile, write_riff};
    use crate::format::shard::{COMMAND_CHUNK, META_CHUNK, SOURCES_CHUNK, STRINGS_CHUNK};
    use crate::format::string_table::{StringTable, TableCompression, encode_string_table};
    use crate::format::uri::path_to_file_uri;
    use crate::relocate::path_map::RootMapping;
    use crate::relocate::string_map_order::golden;

    const INDEX_FILE_TYPE: [u8; 4] = *b"CdIx";

    /// A temp directory with source files whose shard digests match their contents.
    struct SourceTree {
        temp: tempfile::TempDir,
        graph: IncludeGraph,
    }

    impl SourceTree {
        /// `sources` are `(relative path, is TU, relative includes)`; each file's contents are its path.
        fn new(sources: &[(&str, bool, &[&str])]) -> SourceTree {
            let temp = tempfile::tempdir().unwrap();
            let mut graph = IncludeGraph::default();
            for (relative_path, is_tu, includes) in sources {
                let path = temp.path().join(relative_path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, relative_path).unwrap();
                let includes = includes.iter().map(|include| temp.path().join(include)).collect();
                let node = SourceNode {
                    is_tu: *is_tu,
                    digest: digest(relative_path.as_bytes()),
                    includes,
                    format_version: SUPPORTED_VERSIONS[0],
                    clangd_major: None,
                };
                graph.nodes.insert(path, node);
            }
            SourceTree { temp, graph }
        }

        fn path(&self, relative_path: &str) -> PathBuf {
            self.temp.path().join(relative_path)
        }

        fn paths(&self, relative_paths: &[&str]) -> Vec<PathBuf> {
            relative_paths.iter().map(|relative_path| self.path(relative_path)).collect()
        }

        fn edit(&self, relative_path: &str) {
            fs::write(self.path(relative_path), "edited").unwrap();
        }

        fn predict(&self) -> ReindexPrediction {
            predict_reindex(&self.graph, &graph_tus(&self.graph))
        }

        /// Writes the file at `relative_path` with its path as contents, and its shard into the user
        /// index `cache/`; the other sources of the shard are `includes`.
        fn add_user_shard(&self, relative_path: &str, includes: &[&str]) {
            self.add_shard(&self.user_index_dir(), relative_path, includes);
        }

        /// [`Self::add_user_shard`] into `index_dir`.
        fn add_shard(&self, index_dir: &Path, relative_path: &str, includes: &[&str]) {
            let path = self.path(relative_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, relative_path).unwrap();
            fs::create_dir_all(index_dir).unwrap();
            let shard = encode_header_shard(&path, digest(relative_path.as_bytes()), &self.paths(includes));
            fs::write(index_dir.join(shard_file_name(path.as_os_str().as_bytes())), shard).unwrap();
        }

        fn user_index_dir(&self) -> PathBuf {
            self.path("cache")
        }

        /// [`Self::predict`] with the nodes that [`load_external_nodes`] adds for project root `proj/`.
        fn predict_with_external_nodes(&mut self) -> ReindexPrediction {
            let external_nodes = load_external_nodes(&self.graph, &self.path("proj"), &self.user_index_dir()).unwrap();
            self.graph.nodes.extend(external_nodes);
            self.predict()
        }
    }

    /// A shard whose only nodes are `path`'s (string 0) and its direct includes'.
    fn encode_header_shard(path: &Path, contents_digest: FileDigest, includes: &[PathBuf]) -> Vec<u8> {
        let paths = std::iter::once(path).chain(includes.iter().map(PathBuf::as_path));
        let uris = paths.map(|source| path_to_file_uri(source.as_os_str().as_bytes())).collect();
        let include_uris: Vec<u32> = (1..=includes.len() as u32).collect();
        let mut sources = encode_source_node(0, 0, contents_digest, &include_uris);
        for &uri in &include_uris {
            sources.extend(encode_source_node(0, uri, FileDigest::default(), &[]));
        }
        let strings = encode_string_table(&StringTable { strings: uris }, TableCompression::None).unwrap();
        let chunks = vec![
            Chunk { id: *META_CHUNK, data: Cow::Owned(SUPPORTED_VERSIONS[0].to_le_bytes().to_vec()) },
            Chunk { id: *STRINGS_CHUNK, data: Cow::Owned(strings) },
            Chunk { id: *SOURCES_CHUNK, data: Cow::Owned(sources) },
        ];
        write_riff(&RiffFile { file_type: INDEX_FILE_TYPE, chunks })
    }

    /// A TU shard of format `version` whose only node is `path`'s (string 0), with compile command
    /// `arguments`.
    fn encode_tu_shard(path: &Path, version: u32, arguments: &[&str]) -> Vec<u8> {
        let uri = path_to_file_uri(path.as_os_str().as_bytes());
        let directory = b"/work".to_vec();
        let argument_strings = arguments.iter().map(|argument| argument.as_bytes().to_vec());
        let strings: Vec<Vec<u8>> = [uri, directory].into_iter().chain(argument_strings).collect();
        let mut command = Vec::new();
        write_var(&mut command, 1);
        write_var(&mut command, arguments.len() as u32);
        for index in 2..strings.len() as u32 {
            write_var(&mut command, index);
        }
        let strings = encode_string_table(&StringTable { strings }, TableCompression::None).unwrap();
        let chunks = vec![
            Chunk { id: *META_CHUNK, data: Cow::Owned(version.to_le_bytes().to_vec()) },
            Chunk { id: *STRINGS_CHUNK, data: Cow::Owned(strings) },
            Chunk { id: *SOURCES_CHUNK, data: Cow::Owned(encode_source_node(SOURCE_FLAG_IS_TU, 0, FileDigest::default(), &[])) },
            Chunk { id: *COMMAND_CHUNK, data: Cow::Owned(command) },
        ];
        write_riff(&RiffFile { file_type: INDEX_FILE_TYPE, chunks })
    }

    /// A `srcs` entry with `flags`.
    fn encode_source_node(flags: u8, uri: u32, contents_digest: FileDigest, direct_includes: &[u32]) -> Vec<u8> {
        let mut node = vec![flags];
        write_var(&mut node, uri);
        node.extend(contents_digest);
        write_var(&mut node, direct_includes.len() as u32);
        for &include in direct_includes {
            write_var(&mut node, include);
        }
        node
    }

    /// `proj/a.cpp → sys/a.h → sys/b.h`, with the `sys/` shards only in the user index.
    fn tree_with_system_headers() -> SourceTree {
        let tree = SourceTree::new(&[("proj/a.cpp", true, &["sys/a.h"])]);
        tree.add_user_shard("sys/a.h", &["sys/b.h"]);
        tree.add_user_shard("sys/b.h", &[]);
        tree
    }

    /// `a.cpp → a.h → common.h`, `b.cpp → b.h`.
    fn two_tu_tree() -> SourceTree {
        SourceTree::new(&[
            ("a.cpp", true, &["a.h"]),
            ("a.h", false, &["common.h"]),
            ("common.h", false, &[]),
            ("b.cpp", true, &["b.h"]),
            ("b.h", false, &[]),
        ])
    }

    #[test]
    fn predicts_tu_of_transitively_included_edited_header_only() {
        let tree = two_tu_tree();
        tree.edit("common.h");

        let prediction = tree.predict();

        assert_eq!(prediction.reindexed_tus, tree.paths(&["a.cpp"]));
        assert_eq!(prediction.stale_files, 1);
    }

    #[test]
    fn predicts_nothing_when_digests_match() {
        let prediction = two_tu_tree().predict();

        assert_eq!(prediction, ReindexPrediction { reindexed_tus: Vec::new(), stale_files: 0, main_files_without_shard: Vec::new() });
    }

    #[test]
    fn never_counts_unreadable_file_as_stale() {
        let tree = two_tu_tree();
        fs::remove_file(tree.path("b.h")).unwrap();

        assert!(tree.predict().reindexed_tus.is_empty());
    }

    #[test]
    fn reindexes_only_first_tu_for_header_shared_by_several() {
        let tree = SourceTree::new(&[("b.cpp", true, &["shared.h"]), ("a.cpp", true, &["shared.h"]), ("shared.h", false, &[])]);
        tree.edit("shared.h");

        assert_eq!(tree.predict().reindexed_tus, tree.paths(&["a.cpp"]));
        assert_eq!(predict_reindex(&tree.graph, &tree.paths(&["b.cpp", "a.cpp"])).reindexed_tus, tree.paths(&["b.cpp"]));
    }

    #[test]
    fn walks_include_cycles_once() {
        let tree = SourceTree::new(&[("a.cpp", true, &["x.h"]), ("x.h", false, &["y.h"]), ("y.h", false, &["x.h", "a.cpp"])]);
        tree.edit("y.h");

        assert_eq!(tree.predict().reindexed_tus, tree.paths(&["a.cpp"]));
    }

    #[test]
    fn counts_readable_include_without_shard_as_stale() {
        let tree = SourceTree::new(&[("a.cpp", true, &["new.h", "gone.h"])]);
        fs::write(tree.path("new.h"), "new").unwrap();

        let prediction = tree.predict();

        assert_eq!(prediction.reindexed_tus, tree.paths(&["a.cpp"]));
        assert_eq!(prediction.stale_files, 1);
    }

    #[test]
    fn reports_readable_main_files_without_shard_separately() {
        let tree = two_tu_tree();
        fs::write(tree.path("new.cpp"), "new").unwrap();
        let main_files = tree.paths(&["a.cpp", "b.cpp", "new.cpp", "missing.cpp"]);

        let prediction = predict_reindex(&tree.graph, &main_files);

        assert_eq!(prediction.main_files_without_shard, tree.paths(&["new.cpp"]));
        assert!(prediction.reindexed_tus.is_empty());
    }

    #[test]
    fn reads_shards_of_files_outside_project_from_user_index() {
        let mut tree = tree_with_system_headers();

        let prediction = tree.predict_with_external_nodes();

        let mut external_files: Vec<&PathBuf> = tree.graph.nodes.keys().filter(|path| !path.starts_with(tree.path("proj"))).collect();
        external_files.sort();
        assert_eq!(external_files, tree.paths(&["sys/a.h", "sys/b.h"]).iter().collect::<Vec<_>>());
        assert_eq!(prediction, ReindexPrediction { reindexed_tus: Vec::new(), stale_files: 0, main_files_without_shard: Vec::new() });
    }

    #[test]
    fn predicts_tu_of_edited_file_outside_project() {
        let mut tree = tree_with_system_headers();
        tree.edit("sys/b.h");

        let prediction = tree.predict_with_external_nodes();

        assert_eq!(prediction.reindexed_tus, tree.paths(&["proj/a.cpp"]));
        assert_eq!(prediction.stale_files, 1);
    }

    #[test]
    fn never_reads_shards_of_project_files_from_user_index() {
        let mut tree = SourceTree::new(&[("proj/a.cpp", true, &["proj/a.h"])]);
        tree.add_user_shard("proj/a.h", &[]);

        let prediction = tree.predict_with_external_nodes();

        assert!(!tree.graph.nodes.contains_key(&tree.path("proj/a.h")));
        assert_eq!(prediction.reindexed_tus, tree.paths(&["proj/a.cpp"]));
    }

    /// `proj/a.cpp → proj/build/gen.h`, with `proj/build/` holding a compilation database of its own.
    fn tree_with_nested_project() -> SourceTree {
        SourceTree::new(&[("proj/a.cpp", true, &["proj/build/gen.h"]), ("proj/build/gen.h", false, &[])])
    }

    #[test]
    fn reads_shards_of_files_under_nested_project_from_its_index() {
        let mut tree = tree_with_nested_project();
        let nested_dir = tree.path("proj/build");
        tree.add_shard(&nested_dir.join(INDEX_RELATIVE_DIR), "proj/build/gen.h", &["proj/build/gen2.h"]);
        fs::write(tree.path("proj/build/gen2.h"), "new").unwrap();

        load_nested_project_nodes(&mut tree.graph, &nested_dir).unwrap();
        let prediction = tree.predict();

        assert_eq!(tree.graph.nodes[&tree.path("proj/build/gen.h")].includes, tree.paths(&["proj/build/gen2.h"]));
        assert_eq!(prediction.reindexed_tus, tree.paths(&["proj/a.cpp"]));
        assert_eq!(prediction.stale_files, 1);
    }

    #[test]
    fn ignores_project_index_shards_of_files_under_nested_project() {
        let mut tree = tree_with_nested_project();
        let nested_dir = tree.path("proj/build");

        load_nested_project_nodes(&mut tree.graph, &nested_dir).unwrap();
        let prediction = tree.predict();

        assert!(!tree.graph.nodes.contains_key(&tree.path("proj/build/gen.h")));
        assert_eq!(prediction.reindexed_tus, tree.paths(&["proj/a.cpp"]));
    }

    #[test]
    fn drops_main_files_under_nested_project() {
        let main_files = vec![PathBuf::from("/p/src/a.cpp"), PathBuf::from("/p/build/gen.cpp"), PathBuf::from("/p/build-x/b.cpp")];

        let governed = without_nested_main_files(main_files, &[PathBuf::from("/p/build")]);

        assert_eq!(governed, [PathBuf::from("/p/src/a.cpp"), PathBuf::from("/p/build-x/b.cpp")]);
    }

    /// Writes a compilation database of [`golden::entries`] into `dir`.
    fn write_golden_cdb(dir: &Path) -> PathBuf {
        let cdb_path = dir.join("compile_commands.json");
        let entries: Vec<serde_json::Value> = golden::entries()
            .into_iter()
            .map(|(directory, file)| serde_json::json!({"directory": directory, "file": file, "command": "c++ -c x.cpp"}))
            .collect();
        fs::write(&cdb_path, serde_json::to_vec(&entries).unwrap()).unwrap();
        cdb_path
    }

    #[test]
    fn loads_cdb_main_files_in_order_of_real_llvm() {
        let temp = tempfile::tempdir().unwrap();
        let cdb_path = write_golden_cdb(temp.path());

        for probing in [StringMapProbing::Quadratic, StringMapProbing::Linear] {
            let main_files = load_cdb_main_files(&cdb_path, None, probing).unwrap();

            assert_eq!(main_files, golden::expected_order(probing));
        }
    }

    #[test]
    fn takes_string_map_probing_of_newest_tu_shards() {
        let mut graph = two_tu_tree().graph;
        graph.nodes.values_mut().for_each(|node| node.format_version = if node.is_tu { 19 } else { 20 });

        assert_eq!(graph_string_map_probing(&IncludeGraph::default()), StringMapProbing::Linear);
        assert_eq!(graph_string_map_probing(&graph), StringMapProbing::Quadratic);
        graph.nodes.values_mut().find(|node| node.is_tu).unwrap().format_version = 20;
        assert_eq!(graph_string_map_probing(&graph), StringMapProbing::Linear);
        graph.nodes.values_mut().for_each(|node| node.clangd_major = Some(if node.is_tu { 21 } else { 23 }));
        assert_eq!(graph_string_map_probing(&graph), StringMapProbing::Quadratic);
        graph.nodes.values_mut().filter(|node| node.is_tu).for_each(|node| node.format_version = 20);
        graph.nodes.values_mut().find(|node| node.is_tu).unwrap().clangd_major = Some(23);
        assert_eq!(graph_string_map_probing(&graph), StringMapProbing::Linear);
        graph.nodes.values_mut().filter(|node| node.is_tu).for_each(|node| node.format_version = 21);
        graph.nodes.values_mut().find(|node| node.is_tu && node.clangd_major == Some(21)).unwrap().format_version = 20;
        assert_eq!(graph_string_map_probing(&graph), StringMapProbing::Linear);
    }

    #[test]
    fn takes_string_map_probing_of_format_version_and_for_format_20_of_clangd_major() {
        let cases = [
            (19, None, StringMapProbing::Quadratic),
            (19, Some(23), StringMapProbing::Quadratic),
            (20, Some(20), StringMapProbing::Quadratic),
            (20, Some(21), StringMapProbing::Quadratic),
            (20, Some(22), StringMapProbing::Quadratic),
            (20, Some(23), StringMapProbing::Linear),
            (20, Some(17), StringMapProbing::Linear),
            (20, None, StringMapProbing::Linear),
            (21, None, StringMapProbing::Linear),
            (21, Some(22), StringMapProbing::Linear),
        ];

        for (format_version, clangd_major, expected) in cases {
            assert_eq!(string_map_probing(format_version, clangd_major), expected, "format {format_version}, clangd {clangd_major:?}");
        }
    }

    #[test]
    fn reads_clangd_major_from_last_joined_resource_dir() {
        let cases: [(&[&str], Option<u32>); 7] = [
            (&["c++", "-resource-dir=/opt/clangd_21.1.8/lib/clang/21", "--", "a.cpp"], Some(21)),
            (&["c++", "-resource-dir=/usr/lib/llvm-21/lib/clang/21/"], Some(21)),
            (&["c++", "-resource-dir=/opt/llvm/lib/clang/22.1.6"], Some(22)),
            (&["c++", "-resource-dir=/a/lib/clang/19", "-resource-dir=/b/lib/clang/23"], Some(23)),
            (&["c++", "-resource-dir", "/opt/clangd_21.1.8/lib/clang/21"], None),
            (&["c++", "-resource-dir=/opt/clangd/lib/clang/"], None),
            (&["c++", "-c", "a.cpp"], None),
        ];

        for (arguments, expected) in cases {
            let arguments: Vec<&[u8]> = arguments.iter().map(|argument| argument.as_bytes()).collect();
            assert_eq!(clangd_major_of_command(&arguments), expected, "{arguments:?}");
        }
    }

    #[test]
    fn loads_cdb_main_files_in_quadratic_order_for_format_20_shards_of_clangd_21() {
        let temp = tempfile::tempdir().unwrap();
        let shard_dir = temp.path().join("index");
        fs::create_dir(&shard_dir).unwrap();
        let tu_path = golden::key(0);
        let arguments = ["/usr/bin/c++", "-c", "-resource-dir=/opt/clangd_21.1.8/lib/clang/21", "--", "/work/proj/src/tu0.cpp"];
        fs::write(shard_dir.join(shard_file_name(tu_path.as_os_str().as_bytes())), encode_tu_shard(&tu_path, 20, &arguments)).unwrap();

        let graph = load_include_graph(&shard_dir, &PathMap::new(&[]).unwrap()).unwrap();
        let probing = graph_string_map_probing(&graph);
        let main_files = load_cdb_main_files(&write_golden_cdb(temp.path()), None, probing).unwrap();

        assert_eq!(graph.nodes[&tu_path].clangd_major, Some(21));
        assert_eq!(probing, StringMapProbing::Quadratic);
        assert_eq!(main_files, golden::expected_order(StringMapProbing::Quadratic));
    }

    #[test]
    fn loads_cdb_main_files_mapped_and_normalized() {
        let temp = tempfile::tempdir().unwrap();
        let cdb_path = temp.path().join("compile_commands.json");
        let cdb = r#"[
            {"directory": "/x/proj/build", "file": "../src/./a.cpp", "command": "c++ -c ../src/a.cpp"},
            {"directory": "/x/proj/build", "file": "/x/proj/src/b.cpp", "arguments": ["c++", "-c", "b.cpp"]}
        ]"#;
        fs::write(&cdb_path, cdb).unwrap();
        let path_map = PathMap::new(&[RootMapping { old_root: b"/x/proj".to_vec(), new_root: b"/y/wt".to_vec() }]).unwrap();

        let main_files = load_cdb_main_files(&cdb_path, Some(&path_map), StringMapProbing::Linear).unwrap();

        let expected = string_map_order(vec![PathBuf::from("/y/wt/src/a.cpp"), PathBuf::from("/y/wt/src/b.cpp")], StringMapProbing::Linear);
        assert_eq!(main_files, expected);
    }
}
