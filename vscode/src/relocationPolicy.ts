import { type GitLocation, isLinkedWorktree } from "./gitWorktree";
import type { RelocationSummary } from "./relocationSummary";

/**
 * Share of the donor's shards at which the target counts as indexed already.
 * clangd may write its first shards before the probe runs, so a mere `*.idx` file does not mean indexed; relocate keeps existing shards anyway.
 */
const INDEXED_SHARE_OF_DONOR = 0.5;

export type AutoRelocationDecision = "relocate" | "noIndexedDonor" | "alreadyIndexed";

/** How a workspace folder relates to git; only a `linkedWorktree` is relocated into automatically. */
export type FolderKind = "notInGit" | "insideWorktree" | "mainWorktree" | "linkedWorktree";

/** @param folderRoot the folder with symlinks resolved, as git prints its top level. */
export function classifyFolder(folderRoot: string, location: GitLocation | undefined): FolderKind {
  if (location === undefined) {
    return "notInGit";
  }
  if (location.toplevel !== folderRoot) {
    return "insideWorktree";
  }
  return isLinkedWorktree(location) ? "linkedWorktree" : "mainWorktree";
}

/** Where a relocate command started on a workspace folder relocates into; `note` tells the user when that is not the folder itself. */
export type CommandTarget =
  | { readonly kind: "relocate"; readonly root: string; readonly note: string | undefined }
  | { readonly kind: "refused"; readonly message: string };

/** @param folderRoot the folder with symlinks resolved, as git prints its top level. */
export function decideCommandTarget(folderRoot: string, location: GitLocation | undefined): CommandTarget {
  if (location === undefined) {
    return { kind: "refused", message: `${folderRoot} is not in a git worktree, so it has no index to relocate into` };
  }
  if (classifyFolder(folderRoot, location) === "insideWorktree") {
    const note = `${folderRoot} is inside the worktree ${location.toplevel}, so the index is relocated into ${location.toplevel}`;
    return { kind: "relocate", root: location.toplevel, note };
  }
  return { kind: "relocate", root: folderRoot, note: undefined };
}

/** Whether an automatic run should relocate into a linked worktree, given its shard count and that of the largest donor index. */
export function decideAutoRelocation(targetShardCount: number, donorShardCount: number): AutoRelocationDecision {
  if (donorShardCount === 0) {
    return "noIndexedDonor";
  }
  return targetShardCount >= donorShardCount * INDEXED_SHARE_OF_DONOR ? "alreadyIndexed" : "relocate";
}

/** A clangd that is already running loads relocated shards only after a restart. */
export function isClangdRestartNeeded(summary: RelocationSummary, isClangdActive: boolean): boolean {
  return isClangdActive && !summary.isDryRun && summary.shards.relocated > 0;
}
