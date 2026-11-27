export interface RelocateRequest {
  readonly targetRoot: string;
  /** `undefined` lets the CLI pick the closest indexed worktree. */
  readonly donorRoot: string | undefined;
  readonly isRelocateCdb: boolean;
  /** Keeps shards and a compilation database already in the target, such as shards clangd wrote, instead of replacing them. */
  readonly isKeepExisting: boolean;
}

export interface InstallHookRequest {
  readonly repoRoot: string;
  readonly isRelocateCdb: boolean;
}

export function buildRelocateArguments(request: RelocateRequest): string[] {
  const keepArgs = request.isKeepExisting ? ["--keep-existing"] : [];
  const donorArgs = request.donorRoot === undefined ? [] : ["--from", request.donorRoot];
  const cdbArgs = request.isRelocateCdb ? [] : ["--no-relocate-cdb"];
  return ["relocate", "--json", ...keepArgs, "--to", request.targetRoot, ...donorArgs, ...cdbArgs];
}

/** Without `--keep-existing`: the hook runs only in a newly added worktree, which has nothing to keep. */
export function buildInstallHookArguments(request: InstallHookRequest): string[] {
  const cdbArgs = request.isRelocateCdb ? [] : ["--no-relocate-cdb"];
  return ["install-hook", "--repo", request.repoRoot, ...cdbArgs];
}
