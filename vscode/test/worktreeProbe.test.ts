import * as assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { after, before, describe, it } from "node:test";

import { countIndexShards, countLargestDonorIndex, readGitLocation, resolveRealPath } from "../src/worktreeProbe";

const INDEX_RELATIVE_DIR = path.join(".cache", "clangd", "index");
/** Keeps the user's git configuration out of the test repository. */
const GIT_ENV = { ...process.env, GIT_CONFIG_GLOBAL: os.devNull, GIT_CONFIG_NOSYSTEM: "1" };

function makeTempDir(): string {
  return fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "jw-clangd-index-probe-")));
}

function writeIndex(worktreeRoot: string, fileNames: readonly string[]): void {
  const indexDir = path.join(worktreeRoot, INDEX_RELATIVE_DIR);
  fs.mkdirSync(indexDir, { recursive: true });
  for (const fileName of fileNames) {
    fs.writeFileSync(path.join(indexDir, fileName), "");
  }
}

function git(cwd: string, args: readonly string[]): void {
  execFileSync("git", ["-c", "user.name=test", "-c", "user.email=test@example.com", ...args], { cwd, env: GIT_ENV, stdio: "ignore" });
}

describe("countIndexShards", () => {
  let root = "";
  before(() => (root = makeTempDir()));
  after(() => fs.rmSync(root, { recursive: true, force: true }));

  it("counts only .idx files", async () => {
    writeIndex(root, ["a.0123.idx", "b.4567.idx", "notes.txt"]);

    assert.equal(await countIndexShards(root), 2);
  });

  it("is 0 without an index directory", async () => {
    assert.equal(await countIndexShards(path.join(root, "missing")), 0);
  });
});

describe("countLargestDonorIndex", () => {
  let base = "";
  let mainRoot = "";
  let worktreeRoot = "";
  before(() => {
    base = makeTempDir();
    mainRoot = path.join(base, "main");
    worktreeRoot = path.join(base, "wt");
    fs.mkdirSync(mainRoot);
    git(mainRoot, ["init", "--quiet"]);
    git(mainRoot, ["commit", "--quiet", "--allow-empty", "-m", "initial"]);
    git(mainRoot, ["worktree", "add", "--quiet", "--detach", worktreeRoot]);
    writeIndex(mainRoot, ["a.idx", "b.idx", "c.idx"]);
    writeIndex(worktreeRoot, ["a.idx"]);
  });
  after(() => fs.rmSync(base, { recursive: true, force: true }));

  it("counts the shards of the other worktree", async () => {
    assert.equal(await countLargestDonorIndex(worktreeRoot), 3);
    assert.equal(await countLargestDonorIndex(mainRoot), 1);
  });

  it("is 0 outside git", async () => {
    assert.equal(await countLargestDonorIndex(base), 0);
  });
});

describe("readGitLocation", () => {
  let base = "";
  before(() => {
    base = makeTempDir();
    git(base, ["init", "--quiet", "repo"]);
  });
  after(() => fs.rmSync(base, { recursive: true, force: true }));

  it("finds the worktree of a folder in git", async () => {
    const repoRoot = path.join(base, "repo");

    const result = await readGitLocation(repoRoot);

    const gitDir = path.join(repoRoot, ".git");
    assert.deepEqual(result, { kind: "found", location: { toplevel: repoRoot, gitDir, commonDir: gitDir } });
  });

  it("finds a folder outside git", async () => {
    assert.deepEqual(await readGitLocation(base), { kind: "notInGit" });
  });
});

describe("resolveRealPath", () => {
  let root = "";
  before(() => (root = makeTempDir()));
  after(() => fs.rmSync(root, { recursive: true, force: true }));

  it("resolves a symlink", async () => {
    const target = path.join(root, "target");
    const link = path.join(root, "link");
    fs.mkdirSync(target);
    fs.symlinkSync(target, link);

    assert.equal(await resolveRealPath(link), target);
  });

  it("returns a missing path as given", async () => {
    const missing = path.join(root, "missing");

    assert.equal(await resolveRealPath(missing), missing);
  });
});
