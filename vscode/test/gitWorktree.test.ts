import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  describeCheckout,
  interpretGitLocation,
  isLinkedWorktree,
  parseGitLocation,
  parseWorktreeList,
  selectDonorWorktrees,
  type WorktreeEntry,
} from "../src/gitWorktree";

const MAIN_HEAD = "1111111111111111111111111111111111111111";
const FEATURE_HEAD = "2222222222222222222222222222222222222222";
const DETACHED_HEAD = "3333333333333333333333333333333333333333";

/** `git worktree list --porcelain -z` of a main worktree, a locked branch worktree, a detached and a prunable one. */
const WORKTREE_LIST =
  `worktree /work/main\0HEAD ${MAIN_HEAD}\0branch refs/heads/main\0\0` +
  `worktree /work/feature\0HEAD ${FEATURE_HEAD}\0branch refs/heads/feature/x\0locked on a usb stick\0\0` +
  `worktree /work/detached\0HEAD ${DETACHED_HEAD}\0detached\0\0` +
  `worktree /work/gone\0HEAD ${MAIN_HEAD}\0detached\0prunable gitdir file points to non-existent location\0\0`;

const MAIN: WorktreeEntry = { path: "/work/main", head: MAIN_HEAD, branch: "main", isBare: false, isPrunable: false };
const FEATURE: WorktreeEntry = { path: "/work/feature", head: FEATURE_HEAD, branch: "feature/x", isBare: false, isPrunable: false };
const DETACHED: WorktreeEntry = { path: "/work/detached", head: DETACHED_HEAD, branch: undefined, isBare: false, isPrunable: false };
const GONE: WorktreeEntry = { path: "/work/gone", head: MAIN_HEAD, branch: undefined, isBare: false, isPrunable: true };

describe("interpretGitLocation", () => {
  const LOCATION_OUTPUT = "/work/feature\n/work/main/.git/worktrees/feature\n/work/main/.git\n";

  it("reads the location git printed", () => {
    const result = interpretGitLocation({ kind: "exited", exitCode: 0, stdout: LOCATION_OUTPUT, stderr: "" });

    const location = { toplevel: "/work/feature", gitDir: "/work/main/.git/worktrees/feature", commonDir: "/work/main/.git" };
    assert.deepEqual(result, { kind: "found", location });
  });

  it("tells a folder outside git from a git failure", () => {
    const stderr = "fatal: not a git repository (or any of the parent directories): .git\n";

    const result = interpretGitLocation({ kind: "exited", exitCode: 128, stdout: "", stderr });

    assert.deepEqual(result, { kind: "notInGit" });
  });

  it("tells a folder outside git when discovery stops at a mount point", () => {
    const stderr =
      "fatal: not a git repository (or any parent up to mount point /mnt)\nStopping at filesystem boundary (GIT_DISCOVERY_ACROSS_FILESYSTEM not set).\n";

    const result = interpretGitLocation({ kind: "exited", exitCode: 128, stdout: "", stderr });

    assert.deepEqual(result, { kind: "notInGit" });
  });

  it("gives git's error for any other failure", () => {
    const stderr = "fatal: detected dubious ownership in repository at '/work/feature'\n";

    const result = interpretGitLocation({ kind: "exited", exitCode: 128, stdout: "", stderr });

    const reason = "git rev-parse failed (exit code 128): fatal: detected dubious ownership in repository at '/work/feature'";
    assert.deepEqual(result, { kind: "failed", reason });
  });

  it("gives git's error for a linked worktree whose git directory is gone", () => {
    const stderr = "fatal: not a git repository: /x/.git/worktrees/y\n";

    const result = interpretGitLocation({ kind: "exited", exitCode: 128, stdout: "", stderr });

    const reason = "git rev-parse failed (exit code 128): fatal: not a git repository: /x/.git/worktrees/y";
    assert.deepEqual(result, { kind: "failed", reason });
  });

  it("reports a git that could not run", () => {
    const result = interpretGitLocation({ kind: "failed", reason: "git not found", stdout: "", stderr: "" });

    assert.deepEqual(result, { kind: "failed", reason: "git not found" });
  });

  it("reports output it cannot read", () => {
    const result = interpretGitLocation({ kind: "exited", exitCode: 0, stdout: "/work/main/.git\n", stderr: "" });

    assert.deepEqual(result, { kind: "failed", reason: "unexpected git rev-parse output: /work/main/.git" });
  });
});

describe("parseGitLocation", () => {
  it("reads the top level, git directory and common directory", () => {
    const location = parseGitLocation("/work/feature\n/work/main/.git/worktrees/feature\n/work/main/.git\n");

    assert.deepEqual(location, { toplevel: "/work/feature", gitDir: "/work/main/.git/worktrees/feature", commonDir: "/work/main/.git" });
  });

  it("rejects output without three lines", () => {
    assert.equal(parseGitLocation("/work/main/.git\n"), undefined);
  });
});

describe("isLinkedWorktree", () => {
  it("is true only when the git directory differs from the common one", () => {
    assert.equal(isLinkedWorktree({ toplevel: "/w", gitDir: "/m/.git/worktrees/w", commonDir: "/m/.git" }), true);
    assert.equal(isLinkedWorktree({ toplevel: "/m", gitDir: "/m/.git", commonDir: "/m/.git" }), false);
  });
});

describe("parseWorktreeList", () => {
  it("parses every record in order", () => {
    assert.deepEqual(parseWorktreeList(WORKTREE_LIST), [MAIN, FEATURE, DETACHED, GONE]);
  });

  it("parses a bare main repository", () => {
    const entries = parseWorktreeList("worktree /work/repo.git\0bare\0\0");

    assert.deepEqual(entries, [{ path: "/work/repo.git", head: undefined, branch: undefined, isBare: true, isPrunable: false }]);
  });

  it("returns nothing for empty output", () => {
    assert.deepEqual(parseWorktreeList(""), []);
  });
});

describe("selectDonorWorktrees", () => {
  it("leaves out the target, bare and prunable worktrees", () => {
    const bare: WorktreeEntry = { path: "/work/repo.git", head: undefined, branch: undefined, isBare: true, isPrunable: false };

    const donors = selectDonorWorktrees([bare, MAIN, FEATURE, DETACHED, GONE], "/work/feature");

    assert.deepEqual(donors, [MAIN, DETACHED]);
  });
});

describe("describeCheckout", () => {
  it("names the branch, or the short HEAD when detached", () => {
    assert.equal(describeCheckout(FEATURE), "feature/x");
    assert.equal(describeCheckout(DETACHED), "detached at 3333333");
  });
});
