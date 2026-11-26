//! Command-line interface: argument parsing and the dispatch to each command.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::format::shard::ShardFile;
use crate::format::shard_contents::decode_shard_contents;
use crate::format::string_table::TableCompression;
use crate::relocate::path_map::{RootMapping, canonical_root_mapping};
use crate::relocate::worktree_relocation::{WorktreeRelocationRequest, relocate_worktree};

const MAPPING_SEPARATOR: char = '=';
/// clangd's cache directory is `llvm::sys::path::cache_directory`: `$XDG_CACHE_HOME`, else `$HOME/.cache`.
const CACHE_HOME_ENV: &str = "XDG_CACHE_HOME";
const HOME_ENV: &str = "HOME";
const HOME_CACHE_DIR: &str = ".cache";
/// Where clangd keeps the shards of files outside every project, relative to the cache directory.
const USER_INDEX_RELATIVE_DIR: &str = "clangd/index";

/// Relocates a clangd background index between git worktrees by rewriting absolute paths.
#[derive(Debug, Parser)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Copy a donor worktree's index into a target worktree with paths rewritten.
    Relocate(RelocateArgs),
    /// Print a shard's decoded contents as JSON.
    Dump(DumpArgs),
}

#[derive(Debug, Args)]
struct RelocateArgs {
    /// Donor worktree root [default: the indexed worktree closest to the target's HEAD].
    #[arg(long = "from", value_name = "DONOR_ROOT")]
    donor_root: Option<PathBuf>,
    /// Target worktree root [default: the worktree of the current directory].
    #[arg(long = "to", value_name = "TARGET_ROOT")]
    target_root: Option<PathBuf>,
    /// Also rewrite paths under OLD to NEW, such as an out-of-tree build directory (repeatable).
    #[arg(long = "map", value_name = "OLD=NEW", value_parser = parse_root_mapping)]
    mappings: Vec<RootMapping>,
    /// String-table compression of the written shards.
    #[arg(long, value_enum, default_value_t = CompressionArg::Fast)]
    compression: CompressionArg,
    /// Report what would happen without writing anything.
    #[arg(long = "dry-run")]
    is_dry_run: bool,
    /// Replace target shards and compilation database that already exist.
    #[arg(long = "overwrite")]
    is_overwrite: bool,
    /// Also copy the donor's compile_commands.json with paths rewritten.
    #[arg(long = "relocate-cdb")]
    is_relocate_cdb: bool,
}

#[derive(Debug, Args)]
struct DumpArgs {
    /// Shard file (`.idx`).
    shard: PathBuf,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CompressionArg {
    /// zlib level 1.
    Fast,
    /// zlib default level, as clangd writes.
    Default,
    /// No compression.
    None,
}

impl From<CompressionArg> for TableCompression {
    fn from(compression: CompressionArg) -> Self {
        match compression {
            CompressionArg::Fast => TableCompression::Fast,
            CompressionArg::Default => TableCompression::Default,
            CompressionArg::None => TableCompression::None,
        }
    }
}

/// `OLD=NEW`, split at the first `=`, with each root canonicalized when it exists.
fn parse_root_mapping(text: &str) -> Result<RootMapping> {
    let Some((old_root, new_root)) = text.split_once(MAPPING_SEPARATOR) else {
        bail!("expected OLD=NEW, got {text:?}");
    };
    let is_absolute = Path::new(old_root).is_absolute() && Path::new(new_root).is_absolute();
    if !is_absolute {
        bail!("both roots of {text:?} must be absolute paths");
    }
    Ok(canonical_root_mapping(Path::new(old_root), Path::new(new_root)))
}

pub fn run(cli: Cli) -> Result<ExitCode> {
    let mut stdout = io::stdout().lock();
    match cli.command {
        Command::Relocate(args) => run_relocate(args, &mut stdout),
        Command::Dump(args) => run_dump(&args, &mut stdout),
    }
}

fn run_relocate(args: RelocateArgs, out: &mut impl Write) -> Result<ExitCode> {
    let report = relocate_worktree(&WorktreeRelocationRequest {
        donor_root: args.donor_root,
        target_root: args.target_root,
        extra_mappings: args.mappings,
        compression: args.compression.into(),
        is_dry_run: args.is_dry_run,
        is_overwrite: args.is_overwrite,
        is_relocate_cdb: args.is_relocate_cdb,
        user_index_dir: user_index_dir(env::var_os(CACHE_HOME_ENV), env::var_os(HOME_ENV)),
    })?;
    write!(out, "{report}")?;
    Ok(ExitCode::SUCCESS)
}

/// clangd's user index. Unlike LLVM, an empty variable counts as unset and an unset `HOME` gives
/// no index instead of falling back to the passwd entry.
fn user_index_dir(cache_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let is_set = |value: &OsString| !value.is_empty();
    let cache_dir = match (cache_home.filter(is_set), home.filter(is_set)) {
        (Some(cache_home), _) => PathBuf::from(cache_home),
        (None, Some(home)) => Path::new(&home).join(HOME_CACHE_DIR),
        (None, None) => return None,
    };
    Some(cache_dir.join(USER_INDEX_RELATIVE_DIR))
}

fn run_dump(args: &DumpArgs, out: &mut impl Write) -> Result<ExitCode> {
    let bytes = fs::read(&args.shard).with_context(|| format!("reading {}", args.shard.display()))?;
    let contents = decode_shard_contents(&ShardFile::parse(&bytes)?).with_context(|| format!("decoding {}", args.shard.display()))?;
    serde_json::to_writer_pretty(&mut *out, &contents)?;
    writeln!(out)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_mapping_at_first_separator() {
        let mapping = parse_root_mapping("/nonexistent/old=/nonexistent/new=x").unwrap();

        assert_eq!(mapping, RootMapping { old_root: b"/nonexistent/old".to_vec(), new_root: b"/nonexistent/new=x".to_vec() });
    }

    #[test]
    fn finds_user_index_in_xdg_cache_home_before_home() {
        let cache_home = Some(OsString::from("/xdg"));
        let home = Some(OsString::from("/home/u"));

        assert_eq!(user_index_dir(cache_home, home.clone()), Some(PathBuf::from("/xdg/clangd/index")));
        assert_eq!(user_index_dir(Some(OsString::new()), home), Some(PathBuf::from("/home/u/.cache/clangd/index")));
        assert_eq!(user_index_dir(None, None), None);
    }

    #[test]
    fn rejects_mapping_without_separator_or_with_relative_root() {
        assert!(parse_root_mapping("/a").is_err());
        assert!(parse_root_mapping("a=/b").is_err());
    }
}
