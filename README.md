<p align="center"><img src="vscode/media/icon.png" alt="jw-clangd-index icon" width="128"></p>

# jw-clangd-index

Copies a clangd background index from one git worktree to another, so a new worktree starts with a full index instead of a cold reindex.

clangd stores absolute paths in every shard of `.cache/clangd/index` and names each shard after a hash of its file's path.
A fresh worktree therefore gets no use from another checkout's index, and indexing a large project again takes a long time.
`jw-clangd-index` rewrites the paths, renames the shards and leaves clangd only the files that really differ to reindex.

## Install

```sh
cargo install --path .
```

Requires git 2.36 or later (`git worktree list -z`).

## Usage

### relocate

```sh
jw-clangd-index relocate [--from DONOR_ROOT] [--to TARGET_ROOT] [--map OLD=NEW]... \
    [--compression fast|default|none] [--dry-run] [--keep-existing] [--no-relocate-cdb] [--json]
```

- `--to` defaults to the worktree of the current directory.
- `--to` and `--from` must be worktree roots; a directory inside a worktree is an error, one outside any worktree is accepted.
- `--from` defaults to the indexed worktree of the same repository whose `HEAD` differs from the target's in the fewest files; on a tie the main worktree wins, then the smaller path.
- Paths under the donor root become paths under the target root; `--map` adds further roots, such as an out-of-tree build directory.
- When the donor root as given (or as `git worktree list` prints it) goes through a symlink, paths under both forms are mapped.
- A warning reports a relocation in which no shard path, or no compilation database string, was under a mapped root.
- Shards of files deleted in the target (relative to the donor's `HEAD`) are not copied; a file removed only from the git index (`git rm --cached`) still exists and keeps its shard.
- Shards that already exist in the target are replaced; `--keep-existing` keeps them.
- It also copies the donor's `compile_commands.json` (or `build/compile_commands.json`) with paths rewritten, unless `--no-relocate-cdb`; an existing readable target database is replaced, or kept with `--keep-existing`.
- When the target's database path is a symlink, such as a tracked one into the build directory, the database is written where it points (creating the directory), so the symlink stays.
- A database path that leads to the donor's database, or outside the target, is not written through; the summary reports it as skipped with the reason.
- With `--no-relocate-cdb` it warns when the target has no compilation database, because clangd then ignores the index.
- `--dry-run` does everything in memory and writes nothing.
- The summary on stdout lists donor, target, shard counts, failures, the compilation database action and the translation units clangd is predicted to reindex.
- `--json` prints the summary as one JSON object instead: `is_dry_run`, `donor_root`, `donor_changed_files` (`null` with `--from`), `target_root`, `shards` (`relocated`, `kept_existing`, `skipped_deleted`, `skipped_unsupported_version`, `failed`), `cdb` (`action`: `target_has_one`, `target_has_none`, `donor_has_none`, `kept_existing`, `written` or `skipped_link`; `path` or `null`; `reason`, `points_at_donor_cdb` or `leaves_target` for `skipped_link`, else `null`), `reindex` (`has_cdb_main_files`, all `reindexed_tus`, `stale_files`, all `main_files_without_shard`) and `elapsed_seconds`.
- A per-shard failure is a warning; a fatal error exits with 2.

### dump

```sh
jw-clangd-index dump SHARD.idx
```

Prints the decoded shard (symbols, refs, relations, include graph, compile command) as JSON.

### diff

```sh
jw-clangd-index diff DIR_A DIR_B [--map OLD=NEW]... [--ignore-command]
```

Relocates the shards of `DIR_A` in memory through the `--map` roots, then compares them with `DIR_B` shard by shard.
It lists shards only in A, only in B and differing ones with the differing parts (version, symbols, refs, relations, sources, command).
Shards of an unsupported format version on either side are listed as skipped and left out of the comparison.
Exits with 0 when equal, 1 when different, 2 on error.
Use it to check a relocated index against one clangd built in the target itself.

### install-hook

```sh
jw-clangd-index install-hook [--repo PATH] [--no-relocate-cdb] [--keep-existing] [--force]
```

Writes a `post-checkout` hook into `core.hooksPath`, or else the `hooks` directory of the repository's common git directory, which all worktrees share.
Works in a bare repository too, unless `core.hooksPath` is relative.
A hook that this tool did not write is never replaced unless `--force`; the error shows the line to add to it instead.
Installing again replaces this tool's own hook.
`--no-relocate-cdb` and `--keep-existing` make the hook pass the same flags to `relocate`.

## Hook workflow

1. Index the main worktree once with clangd.
2. Run `jw-clangd-index install-hook` in the repository.
3. Every `git worktree add` now runs `jw-clangd-index relocate --to <new worktree>` before it returns.

The hook acts only when git reports a null previous `HEAD` in a linked worktree, which happens only for `git worktree add`.
Branch switches, clones and the first checkout in a main worktree do nothing.
It runs the absolute path of the installing binary in the foreground and never fails the checkout.

## VS Code extension

`vscode/` holds a thin extension that runs `relocate` when a linked git worktree opens without a clangd index, then restarts clangd so it loads the shards.
It also offers commands to relocate on demand, from a chosen worktree, and to install the git hook.
See [vscode/README.md](vscode/README.md) for its settings and how to build the `.vsix`.

## How it works

Each shard is a RIFF file whose chunks refer to a single string table, so rewriting the table's path strings (raw paths and `file:` URIs, matched only at path boundaries) relocates every symbol, reference and include-graph entry at once, and the shard is renamed after the xxh3 hash of its new path.
A self-check verifies that some file in each shard hashes to its original name, which proves this tool hashes paths as the clangd that wrote it.
git supplies the donor choice and the files deleted in the target, whose shards are skipped.
The reindex prediction follows clangd's loader and its `shardIsStale` check and uses no git.
The main files are the target compilation database's entries (`file` resolved against `directory`), or the shards flagged as translation units when the target has no database.
A file belongs to the first main file whose include walk reaches it, in the order clangd loads them: the iteration order of the `llvm::StringMap` that holds the database's entries (LLVM 23), or path order without a database.
A reached file is stale when it is readable and the xxh3 digest of its contents differs from the digest in its own shard, or it has no shard; only the main file it belongs to is reindexed.
A file outside the target root, such as a system header, has its shard in clangd's user index, `$XDG_CACHE_HOME/clangd/index` or else `~/.cache/clangd/index`, where the prediction reads it.
clangd assigns each file to the nearest directory with a compilation database, so with an in-tree `build/compile_commands.json` the directory `build/` is a project of its own: the prediction reads the shards of files under it from `build/.cache/clangd/index`, and leaves its main files out of the root database's, as clangd's broadcast filter does.
A readable database entry without a shard is reported separately, as clangd indexes it from scratch.

## Limitations

- Generated headers are compared like any other file, so a missing generated header counts as not stale until the build creates it, and clangd only notices it changed when it reindexes later.
- Only clangd index format versions 20 and 21 (clangd 23 and llvm main as of 2026-11) are relocated; shards of other versions are skipped.
- Only the target root's index is relocated, not the donor's `build/.cache/clangd/index`, so files under an in-tree `build/`, such as generated headers, have no shard in the target and their main files are reindexed.
- Without a usable compilation database clangd reindexes nothing; the prediction then walks the index's translation units and is labelled as holding once a database exists.
- The predicted owner of a file shared by several translation units assumes LLVM 23's `StringMap` hashing and probing; a clangd built against a different implementation can pick another owner.
- A file outside the target root is looked up in the user index even when clangd would store it with another project, such as an out-of-tree build directory with its own compilation database.
- Relocated compilation databases are pretty-printed with object keys sorted.

## Development / E2E

Scripts in the shared script library (`~/projects/ai-config/tmp/script-library/`) check the tool against real clangd:

- `make-sample-cpp-project.py` builds a synthetic C++ git repo with a compilation database.
- `clangd-background-index-run.py` drives clangd over LSP until its background index is idle and lists the indexed files.
- `clangd-index-relocate-e2e.py` runs the end-to-end checks (unchanged worktree, edited header plus new TU, semantic `diff` against a fresh index, `install-hook` with `git worktree add`) and prints a JSON summary.
- `clangd-index-relocate-timing.py` times `relocate` into fresh worktrees of an indexed repo.
- `llvm-stringmap-order/run.py` builds a driver against LLVM's own `StringMap.cpp` and `xxhash.cpp` from an llvm-project clone and prints the iteration order of a fixed compilation database; its output is the expected order in the `string_map_order` golden test.

Run clangd with `XDG_CACHE_HOME` in a scratch directory, so the user index in `~/.cache` stays untouched.

## License

MIT, see `LICENSE`.
