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
use crate::hook::{HookInstallRequest, HookRelocateFlags, install_hook};
use crate::index_diff::{IndexDiffRequest, diff_indexes};
use crate::relocate::path_map::{PathMap, RootMapping, canonical_root_mapping};
use crate::relocate::worktree_relocation::{WorktreeRelocationRequest, relocate_worktree};

/// `diff` exit code when the indexes differ; equal indexes and other successes exit with 0.
const DIFFERENT_EXIT_CODE: u8 = 1;
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
    /// Compare two index directories after relocating the first; exit 1 when they differ.
    Diff(DiffArgs),
    /// Install a post-checkout hook that relocates an index into every new worktree.
    InstallHook(InstallHookArgs),
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
    /// Keep target shards and compilation database that already exist instead of replacing them.
    #[arg(long = "keep-existing")]
    is_keep_existing: bool,
    /// Do not copy the donor's compile_commands.json with paths rewritten.
    #[arg(long = "no-relocate-cdb")]
    is_no_relocate_cdb: bool,
    /// Print the summary as JSON.
    #[arg(long = "json")]
    is_json: bool,
}

#[derive(Debug, Args)]
struct DumpArgs {
    /// Shard file (`.idx`).
    shard: PathBuf,
}

#[derive(Debug, Args)]
struct DiffArgs {
    /// Index directory relocated before comparing (usually the donor's).
    dir_a: PathBuf,
    /// Index directory compared as is.
    dir_b: PathBuf,
    /// Rewrite paths under OLD to NEW in DIR_A (repeatable).
    #[arg(long = "map", value_name = "OLD=NEW", value_parser = parse_root_mapping)]
    mappings: Vec<RootMapping>,
    /// Ignore differences in the compile commands.
    #[arg(long = "ignore-command")]
    is_ignore_command: bool,
}

#[derive(Debug, Args)]
struct InstallHookArgs {
    /// Any directory of the repository.
    #[arg(long = "repo", value_name = "PATH", default_value = ".")]
    repo: PathBuf,
    /// Make the hook pass --no-relocate-cdb.
    #[arg(long = "no-relocate-cdb")]
    is_no_relocate_cdb: bool,
    /// Make the hook pass --keep-existing.
    #[arg(long = "keep-existing")]
    is_keep_existing: bool,
    /// Replace a post-checkout hook that this tool did not install.
    #[arg(long = "force")]
    is_force: bool,
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
        Command::Diff(args) => run_diff(args, &mut stdout),
        Command::InstallHook(args) => run_install_hook(args, &mut stdout),
    }
}

fn run_relocate(args: RelocateArgs, out: &mut impl Write) -> Result<ExitCode> {
    let report = relocate_worktree(&WorktreeRelocationRequest {
        donor_root: args.donor_root,
        target_root: args.target_root,
        extra_mappings: args.mappings,
        compression: args.compression.into(),
        is_dry_run: args.is_dry_run,
        is_overwrite: !args.is_keep_existing,
        is_relocate_cdb: !args.is_no_relocate_cdb,
        user_index_dir: user_index_dir(env::var_os(CACHE_HOME_ENV), env::var_os(HOME_ENV)),
    })?;
    if !args.is_json {
        write!(out, "{report}")?;
        return Ok(ExitCode::SUCCESS);
    }
    // Serialized before writing, so a failure leaves stdout empty instead of half a document.
    let summary = serde_json::to_string_pretty(&report.summary()).context("cannot print the summary as JSON; is a path not UTF-8?")?;
    writeln!(out, "{summary}")?;
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

fn run_diff(args: DiffArgs, out: &mut impl Write) -> Result<ExitCode> {
    let report = diff_indexes(&IndexDiffRequest {
        dir_a: args.dir_a,
        dir_b: args.dir_b,
        path_map: PathMap::new(&args.mappings)?,
        is_ignore_command: args.is_ignore_command,
    })?;
    write!(out, "{report}")?;
    let exit_code = if report.is_equal() { ExitCode::SUCCESS } else { ExitCode::from(DIFFERENT_EXIT_CODE) };
    Ok(exit_code)
}

fn run_install_hook(args: InstallHookArgs, out: &mut impl Write) -> Result<ExitCode> {
    let binary = std::env::current_exe().context("finding this executable's path")?;
    let relocate_flags = HookRelocateFlags { is_no_relocate_cdb: args.is_no_relocate_cdb, is_keep_existing: args.is_keep_existing };
    let report = install_hook(&HookInstallRequest { repo: args.repo, binary, relocate_flags, is_force: args.is_force })?;
    write!(out, "{report}")?;
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
