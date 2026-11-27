import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import { classifyFolder, decideAutoRelocation, isClangdRestartNeeded } from "../src/relocationPolicy";
import type { RelocationSummary } from "../src/relocationSummary";

const LINKED_LOCATION = { toplevel: "/work/feature", gitDir: "/work/main/.git/worktrees/feature", commonDir: "/work/main/.git" };
const MAIN_LOCATION = { toplevel: "/work/main", gitDir: "/work/main/.git", commonDir: "/work/main/.git" };

function summary(relocated: number, isDryRun: boolean): RelocationSummary {
  return {
    isDryRun,
    donorRoot: "/work/main",
    donorChangedFiles: 0,
    targetRoot: "/work/feature",
    shards: { relocated, keptExisting: 0, skippedDeleted: 0, skippedUnsupportedVersion: 0, failed: 0 },
    cdb: { action: "written", path: "/work/feature/compile_commands.json", reason: undefined },
    reindex: { hasCdbMainFiles: true, reindexedTus: [], staleFiles: 0, mainFilesWithoutShard: [] },
    reindexFailure: undefined,
    elapsedSeconds: 1,
  };
}

describe("classifyFolder", () => {
  it("finds a linked worktree root", () => {
    assert.equal(classifyFolder("/work/feature", LINKED_LOCATION), "linkedWorktree");
  });

  it("finds the main worktree root", () => {
    assert.equal(classifyFolder("/work/main", MAIN_LOCATION), "mainWorktree");
  });

  it("finds a folder inside a worktree", () => {
    assert.equal(classifyFolder("/work/feature/src", LINKED_LOCATION), "insideWorktree");
  });

  it("finds a folder outside git", () => {
    assert.equal(classifyFolder("/tmp", undefined), "notInGit");
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
