import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  describeRelocation,
  describeRelocationProblems,
  parseRelocationSummary,
  type RelocationSummary,
  SummaryFormatError,
} from "../src/relocationSummary";

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
  "reindex_failure": null,
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
  reindexFailure: undefined,
  elapsedSeconds: 1.25,
};

/** {@link SUMMARY} of a run whose compilation database and prediction failed. */
const INCOMPLETE_SUMMARY: RelocationSummary = {
  ...SUMMARY,
  cdb: { action: "failed", path: undefined, reason: "parsing compilation database" },
  reindex: undefined,
  reindexFailure: "reading /work/feature/.cache/clangd/index",
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

  it("reads a failed compilation database and prediction", () => {
    const cdb = { action: "failed", path: null, reason: "parsing compilation database" };
    const json = withField(["reindex_failure"], "reading /work/feature/.cache/clangd/index", withField(["cdb"], cdb));

    const summary = parseRelocationSummary(withField(["reindex"], null, json));

    assert.deepEqual(summary, INCOMPLETE_SUMMARY);
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

  it("warns that nothing was written when the compilation database links outside the worktree", () => {
    const cdb = { action: "skipped_link", path: "/work/feature/compile_commands.json", reason: "leaves_target" } as const;
    const summary: RelocationSummary = { ...SUMMARY, cdb };

    assert.match(describeRelocation(summary), /; compile_commands.json links outside the worktree, so nothing was written$/);
  });

  it("warns that clangd ignores the index behind a skipped link without a usable database", () => {
    const cdb = { action: "skipped_link", path: "/work/feature/compile_commands.json", reason: "points_at_donor_cdb" } as const;
    const reindex = { hasCdbMainFiles: false, reindexedTus: [], staleFiles: 0, mainFilesWithoutShard: [] };
    const summary: RelocationSummary = { ...SUMMARY, cdb, reindex };

    assert.match(
      describeRelocation(summary),
      /; compile_commands.json links to the donor's database, so nothing was written, and clangd ignores the index until the worktree has a usable one$/,
    );
  });

  it("names a link into the donor worktree", () => {
    const cdb = { action: "skipped_link", path: "/work/feature/compile_commands.json", reason: "points_into_donor" } as const;
    const summary: RelocationSummary = { ...SUMMARY, cdb };

    assert.match(describeRelocation(summary), /; compile_commands.json links into the donor worktree, so nothing was written$/);
  });

  it("says what a run that relocated the shards could not do", () => {
    assert.equal(
      describeRelocation(INCOMPLETE_SUMMARY),
      "relocated 9876 shards from main (12 kept, 2 failed); reindex not predicted; compile_commands.json was not relocated",
    );
  });

  it("warns when git tracks the compilation database", () => {
    const cdb = { action: "kept_tracked", path: "/work/feature/compile_commands.json", reason: "tracked_by_git" } as const;

    assert.match(describeRelocation({ ...SUMMARY, cdb }), /; git tracks compile_commands.json, so it was not replaced$/);
  });
});

describe("describeRelocationProblems", () => {
  it("is undefined for a complete run", () => {
    assert.equal(describeRelocationProblems(SUMMARY), undefined);
  });

  it("gives the reasons of the compilation database and prediction failures", () => {
    assert.equal(
      describeRelocationProblems(INCOMPLETE_SUMMARY),
      "relocating compile_commands.json failed: parsing compilation database; " +
        "predicting the reindex failed: reading /work/feature/.cache/clangd/index",
    );
  });
});
