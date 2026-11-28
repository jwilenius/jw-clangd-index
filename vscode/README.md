# jw-clangd-index for VS Code

Relocates a clangd background index into a git worktree that opens without one, so clangd reindexes only the files that differ instead of the whole project.
It runs the `jw-clangd-index` CLI of the parent directory; see the README of the repository root for how relocation works.
Like the CLI, which uses `std::os::unix`, it works on Linux and macOS only; each `.vsix` bundles the binary of one target: `linux-x64`, `linux-arm64`, `darwin-x64` or `darwin-arm64`.
The macOS packages are built but have not been tested on a real Mac.

## Behaviour

- The extension activates in workspaces that contain `.git` (a directory in the main worktree, a file in a linked one).
- On activation, and for every workspace folder added later, it checks each local folder, one at a time, in a single queue.
- It acts only on the root of a linked git worktree: `git rev-parse --absolute-git-dir` differs from `--path-format=absolute --git-common-dir`.
  The main worktree, folders inside a worktree and folders outside git are left alone after that single `git rev-parse` call.
- It counts the `*.idx` shards in `<folder>/.cache/clangd/index` and in each other worktree's index.
  When no other worktree has shards, it only logs, so repositories that clangd never indexed stay quiet.
  When the folder already holds at least half as many shards as the largest other index, it logs that the folder is indexed and stops.
  Otherwise it runs `jw-clangd-index relocate --json --keep-existing --to <folder>`, plus `--no-relocate-cdb` when `jwClangdIndex.relocateCdb` is off.
- The threshold is half, not "any shard", because clangd may start indexing the new worktree before the check runs: its first shards must not cancel the relocation that saves reindexing everything.
  The race remains for a clangd that writes more than half the shards first; then run `Relocate index` by hand.
  The CLI picks the donor: the indexed worktree whose `HEAD` differs from the folder's in the fewest files.
- It passes `--keep-existing`, so shards and a compilation database that are already in the folder, such as shards clangd has written, are kept.
- When more than 0 shards were relocated and the clangd extension (`llvm-vs-code-extensions.vscode-clangd`) is active, it runs `clangd.restart`, because a running clangd loads shards only at startup.
  In a multi-root workspace this restarts clangd once per folder that received shards.
  A clangd that is not running yet loads them when it starts.
- A status-bar message shows the counts, such as `relocated 9876 shards from main (0 kept, 0 failed); 3 TUs to reindex`.
- A failure of an automatic run shows a warning with a button that opens the log.
- The `jw-clangd-index` output channel logs every command line, exit code, stdout and stderr.
- `relocate` times out after 10 minutes, `install-hook` after 30 seconds, and each git call after 15 seconds.
- The extension is disabled in untrusted workspaces and in virtual workspaces.

## Commands

| Command                                 | Action                                                                                                                 |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `jw-clangd-index: Relocate index`       | Relocates into the chosen workspace folder now, from the automatically chosen donor.                                   |
| `jw-clangd-index: Relocate index from…` | Picks the donor from the folder's other worktrees (`git worktree list --porcelain -z`, without bare or prunable ones). |
| `jw-clangd-index: Install git hook`     | Runs `install-hook --repo <folder>`, plus `--no-relocate-cdb` when `jwClangdIndex.relocateCdb` is off.                 |

The hook stores the absolute path of the binary that installed it.
VS Code deletes the bundled binary's versioned extension directory on update, so after installing with the bundled binary the command warns; set `jwClangdIndex.path` to a lasting binary and install again.

With several workspace folders, each command first asks for the folder.
The relocate commands relocate into the root of the worktree that holds the folder, and say so when that is not the folder itself; they refuse a folder outside git with a warning.
The commands report success in a notification.
The relocate commands do not pass `--keep-existing`, so they replace the folder's shards and compilation database, unless git tracks the database.

## Settings

| Setting                      | Default | Meaning                                                                                                               |
| ---------------------------- | ------- | --------------------------------------------------------------------------------------------------------------------- |
| `jwClangdIndex.path`         | `""`    | Path of the binary; empty uses the bundled `bin/<platform>-<arch>/jw-clangd-index`, else `jw-clangd-index` on `PATH`. |
| `jwClangdIndex.autoRelocate` | `true`  | Relocate automatically when a linked worktree opens without an index.                                                 |
| `jwClangdIndex.relocateCdb`  | `true`  | Relocate the compilation database; off passes `--no-relocate-cdb`. clangd ignores the index without one.              |

## Development

The build tools need Node.js 22 or later (`@vscode/vsce` 4 requires it); the unit tests also run on Node.js 18.

```sh
npm install
npm run build          # esbuild bundle: dist/extension.js
npm run typecheck      # tsc --noEmit, strict
npm test               # tsc into out/, then node --test out/test/*.test.js
npm run format:check   # prettier on src, test, scripts and this README
```

The modules under `src/` other than `extension.ts` and `relocationController.ts` do not import `vscode`, so the tests run without VS Code.

## Packaging

```sh
npm run package                           # the host's target
npm run package -- --target darwin-arm64  # any supported target
```

`package` (`scripts/package.mjs`) maps the target to a Rust triple:

| Target         | Rust triple                  |
| -------------- | ---------------------------- |
| `linux-x64`    | `x86_64-unknown-linux-musl`  |
| `linux-arm64`  | `aarch64-unknown-linux-musl` |
| `darwin-x64`   | `x86_64-apple-darwin`        |
| `darwin-arm64` | `aarch64-apple-darwin`       |

Linux builds are static musl binaries, so one runs on every distribution.
Add the triple first with `rustup target add <triple>`; macOS triples build only on a Mac, which has the SDK.
It runs `cargo build --release --target <triple>` in the parent directory, deletes `bin/` so no other target's binary ships, copies `../target/<triple>/release/jw-clangd-index` into `bin/<target>/` and runs `vsce package --target <target> --no-dependencies` (esbuild bundles all code, so no `node_modules` are shipped), which type-checks and builds first and writes `jw-clangd-index-<target>-0.1.0.vsix`.
`LICENSE` is a symlink to the repository root's `LICENSE`, so the `.vsix` carries it.
`THIRD-PARTY-NOTICES.txt` holds the licenses of the Rust crates in the bundled binary; `npm run notices` regenerates it with `cargo-about`, and CI fails when it is out of date.
Install it with `code --install-extension jw-clangd-index-<target>-0.1.0.vsix`.
CI (`.github/workflows/ci.yml`) packages all four targets, testing the three it can run natively, and uploads each `.vsix` as artifact `jw-clangd-index-<target>-vsix`.

## Publishing (not yet done)

- Nothing is published yet; the CI workflow (`.github/workflows/ci.yml`) only uploads each target's `.vsix` as artifact `jw-clangd-index-<target>-vsix`.
- The extension id is `jwilenius.jw-clangd-index`: publisher `jwilenius` on the VS Code Marketplace, the same publisher as jw-differ.
- Publishing would follow jw-differ: pushing a `vscode-vX.Y.Z` tag runs a workflow that publishes the tested `.vsix` with `vsce publish --oidc --packagePath <vsix>`.
- That uses Marketplace Trusted Publishing, so no token is stored: publisher `jwilenius` needs a policy for repository `jw-clangd-index`, the workflow file and an environment `marketplace` limited to `vscode-v*` tags, and the job needs `id-token: write`.
- Without Trusted Publishing, a repository secret `VSCE_PAT` (an Azure DevOps token with scope Marketplace: Manage) passed to `vsce publish` works instead.
- Publishing would upload all four target `.vsix` files of one version, so the Marketplace offers each platform its own; Windows gets none.

## License

MIT, see `LICENSE`.
