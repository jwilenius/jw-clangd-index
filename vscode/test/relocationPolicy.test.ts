import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import { classifyFolder, decideAutoRelocation, decideCommandTarget, isClangdRestartNeeded } from "../src/relocationPolicy";
import type { GitLocationResult } from "../src/gitWorktree";
import type { RelocationSummary } from "../src/relocationSummary";

const LINKED_LOCATION = { toplevel: "/work/feature", gitDir: "/work/main/.git/worktrees/feature", commonDir: "/work/main/.git" };
const MAIN_LOCATION = { toplevel: "/work/main", gitDir: "/work/main/.git", commonDir: "/work/main/.git" };
const IN_LINKED: GitLocationResult = { kind: "found", location: LINKED_LOCATION };
const IN_MAIN: GitLocationResult = { kind: "found", location: MAIN_LOCATION };
const NOT_IN_GIT: GitLocationResult = { kind: "notInGit" };

function summary(relocated: number, isDryRun: boolean): RelocationSummary {
  return {
    isDryRun,
    donorRoot: "/work/main",
    donorChangedFiles: 0,
    targetRoot: "/work/feature",
    shards: { relocated, keptExisting: 0, skippedDeleted: 0, skippedUnsupportedVersion: 0, failed: 0 },
    cdb: { action: "written", path: "/work/feature/compile_commands.json", reason: undefined, targetHasCdb: true },
    reindex: { hasCdbMainFiles: true, reindexedTus: [], staleFiles: 0, mainFilesWithoutShard: [] },
    reindexFailure: undefined,
    elapsedSeconds: 1,
  };
}

describe("classifyFolder", () => {
  it("finds a linked worktree root", () => {
    assert.equal(classifyFolder("/work/feature", IN_LINKED), "linkedWorktree");
  });

  it("finds the main worktree root", () => {
    assert.equal(classifyFolder("/work/main", IN_MAIN), "mainWorktree");
  });

  it("finds a folder inside a worktree", () => {
    assert.equal(classifyFolder("/work/feature/src", IN_LINKED), "insideWorktree");
  });

  it("finds a folder outside git", () => {
    assert.equal(classifyFolder("/tmp", NOT_IN_GIT), "notInGit");
  });
});

describe("decideCommandTarget", () => {
  it("relocates into a worktree root as picked", () => {
    assert.deepEqual(decideCommandTarget("/work/feature", IN_LINKED), { kind: "relocate", root: "/work/feature", note: undefined });
    assert.deepEqual(decideCommandTarget("/work/main", IN_MAIN), { kind: "relocate", root: "/work/main", note: undefined });
  });

  it("relocates into the root of the worktree a folder is inside, and says so", () => {
    assert.deepEqual(decideCommandTarget("/work/feature/src", IN_LINKED), {
      kind: "relocate",
      root: "/work/feature",
      note: "/work/feature/src is inside the worktree /work/feature, so the index is relocated into /work/feature",
    });
  });

  it("refuses a folder outside git", () => {
    assert.deepEqual(decideCommandTarget("/tmp", NOT_IN_GIT), {
      kind: "refused",
      message: "/tmp is not in a git worktree, so it has no index to relocate into",
    });
  });

  it("refuses a folder git failed on, with git's error", () => {
    const failure: GitLocationResult = { kind: "failed", reason: "git rev-parse failed (exit code 128): fatal: bad config" };

    assert.deepEqual(decideCommandTarget("/work/feature", failure), {
      kind: "refused",
      message: "cannot tell whether /work/feature is in a git worktree: git rev-parse failed (exit code 128): fatal: bad config",
    });
  });
});

describe("decideAutoRelocation", () => {
  it("relocates into a worktree with no or few shards of its own", () => {
    assert.equal(decideAutoRelocation(0, 1), "relocate");
    assert.equal(decideAutoRelocation(49, 100), "relocate");
  });

  it("skips a worktree holding at least half the donor's shards", () => {
    assert.equal(decideAutoRelocation(50, 100), "alreadyIndexed");
    assert.equal(decideAutoRelocation(120, 100), "alreadyIndexed");
  });

  it("skips when no other worktree is indexed", () => {
    assert.equal(decideAutoRelocation(0, 0), "noIndexedDonor");
  });
});

describe("isClangdRestartNeeded", () => {
  it("restarts an active clangd after shards were relocated", () => {
    assert.equal(isClangdRestartNeeded(summary(5, false), true), true);
  });

  it("does not restart an inactive clangd, after a dry run, or when nothing was relocated", () => {
    assert.equal(isClangdRestartNeeded(summary(5, false), false), false);
    assert.equal(isClangdRestartNeeded(summary(5, true), true), false);
    assert.equal(isClangdRestartNeeded(summary(0, false), true), false);
  });
});
