import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import { describeRelocation, parseRelocationSummary, type RelocationSummary, SummaryFormatError } from "../src/relocationSummary";

/** Output of `jw-clangd-index relocate --json` with an automatically chosen donor. */
const SUMMARY_JSON = `{
  "is_dry_run": false,
  "donor_root": "/work/main",
  "donor_changed_files": 4,
  "target_root": "/work/feature",
  "shards": {
    "relocated": 9876,
    "kept_existing": 12,
    "skipped_deleted": 1,
    "skipped_unsupported_version": 0,
    "failed": 2
  },
  "cdb": {
    "action": "written",
    "path": "/work/feature/compile_commands.json",
    "reason": null
  },
  "reindex": {
    "has_cdb_main_files": true,
    "reindexed_tus": ["/work/feature/src/a.cpp", "/work/feature/src/b.cpp"],
    "stale_files": 3,
    "main_files_without_shard": []
  },
  "elapsed_seconds": 1.25
}`;

const SUMMARY: RelocationSummary = {
  isDryRun: false,
  donorRoot: "/work/main",
  donorChangedFiles: 4,
  targetRoot: "/work/feature",
  shards: { relocated: 9876, keptExisting: 12, skippedDeleted: 1, skippedUnsupportedVersion: 0, failed: 2 },
  cdb: { action: "written", path: "/work/feature/compile_commands.json", reason: undefined },
  reindex: {
    hasCdbMainFiles: true,
    reindexedTus: ["/work/feature/src/a.cpp", "/work/feature/src/b.cpp"],
    staleFiles: 3,
    mainFilesWithoutShard: [],
  },
  elapsedSeconds: 1.25,
};

/** `json` with the field at `path` set to `value`; `undefined` removes it. */
function withField(path: readonly string[], value: unknown, json: string = SUMMARY_JSON): string {
  const summary = JSON.parse(json) as Record<string, unknown>;
  const parent = path.slice(0, -1).reduce((object, key) => object[key] as Record<string, unknown>, summary);
  parent[path[path.length - 1] ?? ""] = value;
  return JSON.stringify(summary);
}

describe("parseRelocationSummary", () => {
  it("converts the CLI's JSON summary", () => {
    const summary = parseRelocationSummary(SUMMARY_JSON);

    assert.deepEqual(summary, SUMMARY);
  });

  it("maps null donor_changed_files, cdb.path and cdb.reason to undefined", () => {
    const json = withField(["cdb"], { action: "donor_has_none", path: null, reason: null });

    const summary = parseRelocationSummary(withField(["donor_changed_files"], null, json));

    assert.equal(summary.donorChangedFiles, undefined);
    assert.deepEqual(summary.cdb, { action: "donor_has_none", path: undefined, reason: undefined });
  });

  it("reads the reason a tracked compilation database was kept", () => {
    const tracked = { action: "kept_tracked", path: "/work/feature/compile_commands.json", reason: "tracked_by_git" };

    const summary = parseRelocationSummary(withField(["cdb"], tracked));

    assert.deepEqual(summary.cdb, { action: "kept_tracked", path: "/work/feature/compile_commands.json", reason: "tracked_by_git" });
  });

  it("rejects a missing field", () => {
    assert.throws(() => parseRelocationSummary(withField(["shards", "relocated"], undefined)), SummaryFormatError);
  });

  it("rejects an unknown cdb action", () => {
    assert.throws(() => parseRelocationSummary(withField(["cdb", "action"], "moved")), /unknown cdb.action "moved"/);
  });

  it("rejects a path list with a non-string entry", () => {
    assert.throws(() => parseRelocationSummary(withField(["reindex", "reindexed_tus"], [1])), SummaryFormatError);
  });

  it("rejects text that is not JSON", () => {
    assert.throws(() => parseRelocationSummary("donor:    /work/main"), SyntaxError);
  });
});

describe("describeRelocation", () => {
  it("names the donor, the counts and the TUs to reindex", () => {
    assert.equal(describeRelocation(SUMMARY), "relocated 9876 shards from main (12 kept, 2 failed); 2 TUs to reindex");
  });

  it("marks a dry run", () => {
    assert.match(describeRelocation({ ...SUMMARY, isDryRun: true }), /^would relocate 9876 shards/);
  });

  it("warns when the target has no compilation database", () => {
    const summary: RelocationSummary = { ...SUMMARY, cdb: { action: "target_has_none", path: undefined, reason: undefined } };

    assert.match(describeRelocation(summary), /; no compile_commands.json, so clangd ignores the index$/);
  });

  it("warns when the compilation database links outside the worktree", () => {
    const cdb = { action: "skipped_link", path: "/work/feature/compile_commands.json", reason: "leaves_target" } as const;
    const summary: RelocationSummary = { ...SUMMARY, cdb };

    assert.match(describeRelocation(summary), /; compile_commands.json links outside the worktree, so it was not replaced$/);
  });

  it("warns when git tracks the compilation database", () => {
    const cdb = { action: "kept_tracked", path: "/work/feature/compile_commands.json", reason: "tracked_by_git" } as const;

    assert.match(describeRelocation({ ...SUMMARY, cdb }), /; git tracks compile_commands.json, so it was not replaced$/);
  });
});
