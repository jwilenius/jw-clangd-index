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

/** `relocate --json`, never `--overwrite`, so shards clangd already wrote in the target are kept. */
export function buildRelocateArguments(request: RelocateRequest): string[] {
  const donorArgs = request.donorRoot === undefined ? [] : ["--from", request.donorRoot];
  const cdbArgs = request.isRelocateCdb ? ["--relocate-cdb"] : [];
  return ["relocate", "--json", "--to", request.targetRoot, ...donorArgs, ...cdbArgs];
}

export function buildInstallHookArguments(request: InstallHookRequest): string[] {
  const cdbArgs = request.isRelocateCdb ? ["--relocate-cdb"] : [];
  return ["install-hook", "--repo", request.repoRoot, ...cdbArgs];
}
