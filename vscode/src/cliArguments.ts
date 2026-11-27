export interface RelocateRequest {
  readonly targetRoot: string;
  /** `undefined` lets the CLI pick the closest indexed worktree. */
  readonly donorRoot: string | undefined;
  readonly isRelocateCdb: boolean;
}

export interface InstallHookRequest {
  readonly repoRoot: string;
  readonly isRelocateCdb: boolean;
}

/** `relocate --json --keep-existing`, so shards clangd already wrote in the target are kept. */
export function buildRelocateArguments(request: RelocateRequest): string[] {
  const donorArgs = request.donorRoot === undefined ? [] : ["--from", request.donorRoot];
  const cdbArgs = request.isRelocateCdb ? [] : ["--no-relocate-cdb"];
  return ["relocate", "--json", "--keep-existing", "--to", request.targetRoot, ...donorArgs, ...cdbArgs];
}

/** Without `--keep-existing`: the hook runs only in a newly added worktree, which has nothing to keep. */
export function buildInstallHookArguments(request: InstallHookRequest): string[] {
  const cdbArgs = request.isRelocateCdb ? [] : ["--no-relocate-cdb"];
  return ["install-hook", "--repo", request.repoRoot, ...cdbArgs];
}
