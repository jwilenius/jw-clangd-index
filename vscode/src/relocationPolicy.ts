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
