import * as fs from "node:fs/promises";
import * as path from "node:path";

import {
  GIT_MESSAGE_LOCALE_ENV,
  type GitLocationResult,
  interpretGitLocation,
  parseWorktreeList,
  REV_PARSE_LOCATION_ARGS,
  selectDonorWorktrees,
  WORKTREE_LIST_ARGS,
  type WorktreeEntry,
} from "./gitWorktree";
import { type ProcessRequest, runProcess } from "./processRunner";

const GIT_BINARY = "git";
const GIT_TIMEOUT_MS = 15_000;
const GIT_MAX_BUFFER_BYTES = 4 * 1024 * 1024;
/** Where clangd keeps a project's shards, relative to the project root. */
const INDEX_RELATIVE_DIR = path.join(".cache", "clangd", "index");
const SHARD_EXTENSION = ".idx";

export async function readGitLocation(folder: string): Promise<GitLocationResult> {
  const request = { ...gitRequest(folder, REV_PARSE_LOCATION_ARGS), env: GIT_MESSAGE_LOCALE_ENV };
  return interpretGitLocation(await runProcess(request));
}

/** Empty when git fails. */
export async function listWorktrees(folder: string): Promise<WorktreeEntry[]> {
  const stdout = await runGit(folder, WORKTREE_LIST_ARGS);
  return stdout === undefined ? [] : parseWorktreeList(stdout);
}

function gitRequest(folder: string, args: readonly string[]): ProcessRequest {
  return { file: GIT_BINARY, args, cwd: folder, timeoutMs: GIT_TIMEOUT_MS, maxBufferBytes: GIT_MAX_BUFFER_BYTES };
}

async function runGit(folder: string, args: readonly string[]): Promise<string | undefined> {
  const result = await runProcess(gitRequest(folder, args));
  const isSuccess = result.kind === "exited" && result.exitCode === 0;
  return isSuccess ? result.stdout : undefined;
}

/** 0 when the index directory is missing or unreadable. */
export async function countIndexShards(worktreeRoot: string): Promise<number> {
  try {
    const names = await fs.readdir(path.join(worktreeRoot, INDEX_RELATIVE_DIR));
    return names.filter((name) => name.endsWith(SHARD_EXTENSION)).length;
  } catch {
    return 0;
  }
}

/** The most shards any other worktree of `targetRoot` has; 0 when none could be a donor. */
export async function countLargestDonorIndex(targetRoot: string): Promise<number> {
  const donors = selectDonorWorktrees(await listWorktrees(targetRoot), targetRoot);
  const shardCounts = await Promise.all(donors.map((donor) => countIndexShards(donor.path)));
  return Math.max(0, ...shardCounts);
}

/** `folder` with symlinks resolved, or as given when it cannot be resolved. */
export async function resolveRealPath(folder: string): Promise<string> {
  try {
    return await fs.realpath(folder);
  } catch {
    return folder;
  }
}
