import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  describeRelocation,
  hasRelocationProblems,
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
    "reason": null,
    "target_has_cdb": true
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
  cdb: { action: "written", path: "/work/feature/compile_commands.json", reason: undefined, targetHasCdb: true },
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
  cdb: { action: "failed", path: undefined, reason: "parsing compilation database", targetHasCdb: true },
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
    const json = withField(["cdb"], { action: "donor_has_none", path: null, reason: null, target_has_cdb: false });

    const summary = parseRelocationSummary(withField(["donor_changed_files"], null, json));

    assert.equal(summary.donorChangedFiles, undefined);
    assert.deepEqual(summary.cdb, { action: "donor_has_none", path: undefined, reason: undefined, targetHasCdb: false });
  });

  it("reads the reason a tracked compilation database was kept", () => {
    const tracked = { action: "kept_tracked", path: "/work/feature/compile_commands.json", reason: "tracked_by_git", target_has_cdb: true };

    const summary = parseRelocationSummary(withField(["cdb"], tracked));

    assert.deepEqual(summary.cdb, {
      action: "kept_tracked",
      path: "/work/feature/compile_commands.json",
      reason: "tracked_by_git",
      targetHasCdb: true,
    });
  });

  it("reads a failed compilation database and prediction", () => {
    const cdb = { action: "failed", path: null, reason: "parsing compilation database", target_has_cdb: true };
    const json = withField(["reindex_failure"], "reading /work/feature/.cache/clangd/index", withField(["cdb"], cdb));

    const summary = parseRelocationSummary(withField(["reindex"], null, json));

    assert.deepEqual(summary, INCOMPLETE_SUMMARY);
  });

  it("rejects a missing field", () => {
    assert.throws(() => parseRelocationSummary(withField(["shards", "relocated"], undefined)), SummaryFormatError);
  });

  it("reads the summary of a CLI that predates cdb.reason, cdb.target_has_cdb and reindex_failure", () => {
    const withoutCdbFields = withField(["cdb", "target_has_cdb"], undefined, withField(["cdb", "reason"], undefined));
    const json = withField(["reindex_failure"], undefined, withoutCdbFields);

    const summary = parseRelocationSummary(json);

    assert.deepEqual(summary, { ...SUMMARY, cdb: { ...SUMMARY.cdb, targetHasCdb: undefined } });
  });

  it("rejects a cdb.target_has_cdb that is not a boolean", () => {
    assert.throws(() => parseRelocationSummary(withField(["cdb", "target_has_cdb"], "yes")), /target_has_cdb is not a boolean/);
  });

  it("rejects an optional field of the wrong type", () => {
    assert.throws(() => parseRelocationSummary(withField(["reindex_failure"], 1)), /reindex_failure is neither a string nor null/);
  });

  it("reads an unknown cdb action as unknown", () => {
    const summary = parseRelocationSummary(withField(["cdb", "action"], "moved"));

    assert.equal(summary.cdb.action, "unknown");
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
    const cdb = { action: "target_has_none", path: undefined, reason: undefined, targetHasCdb: false } as const;

    assert.match(describeRelocation({ ...SUMMARY, cdb }), /; no compile_commands.json, so clangd ignores the index$/);
  });

  it("does not warn when the donor has no compilation database but the target has one", () => {
    const cdb = { action: "donor_has_none", path: undefined, reason: undefined, targetHasCdb: true } as const;

    assert.equal(describeRelocation({ ...SUMMARY, cdb }), describeRelocation(SUMMARY));
  });

  it("warns about a donor without a compilation database for a CLI that predates target_has_cdb", () => {
    const cdb = { action: "donor_has_none", path: undefined, reason: undefined, targetHasCdb: undefined } as const;

    assert.match(describeRelocation({ ...SUMMARY, cdb }), /; no compile_commands.json, so clangd ignores the index$/);
  });

  it("warns that clangd ignores the index after a failed relocation leaves the target without a database", () => {
    const cdb = { action: "failed", path: undefined, reason: "parsing compilation database", targetHasCdb: false } as const;

    assert.match(
      describeRelocation({ ...SUMMARY, cdb }),
      /; relocating compile_commands.json failed: parsing compilation database, and clangd ignores the index until the worktree has a usable one$/,
    );
  });

  it("warns that nothing was written when the compilation database links outside the worktree", () => {
    const cdb = {
      action: "skipped_link",
      path: "/work/feature/compile_commands.json",
      reason: "leaves_target",
      targetHasCdb: true,
    } as const;
    const summary: RelocationSummary = { ...SUMMARY, cdb };

    assert.match(describeRelocation(summary), /; compile_commands.json links outside the worktree, so nothing was written$/);
  });

  it("warns that clangd ignores the index behind a skipped link without a usable database, also when the prediction failed", () => {
    const cdb = {
      action: "skipped_link",
      path: "/work/feature/compile_commands.json",
      reason: "points_at_donor_cdb",
      targetHasCdb: false,
    } as const;
    const summary: RelocationSummary = { ...SUMMARY, cdb, reindex: undefined, reindexFailure: "x" };

    assert.match(
      describeRelocation(summary),
      /; compile_commands.json links to the donor's database, so nothing was written, and clangd ignores the index until the worktree has a usable one$/,
    );
  });

  it("tells by the prediction whether clangd ignores the index behind a skipped link for a CLI that predates target_has_cdb", () => {
    const cdb = {
      action: "skipped_link",
      path: "/work/feature/compile_commands.json",
      reason: "points_at_donor_cdb",
      targetHasCdb: undefined,
    } as const;
    const reindex = { hasCdbMainFiles: false, reindexedTus: [], staleFiles: 0, mainFilesWithoutShard: [] };
    const summary: RelocationSummary = { ...SUMMARY, cdb, reindex };

    assert.match(
      describeRelocation(summary),
      /; compile_commands.json links to the donor's database, so nothing was written, and clangd ignores the index until the worktree has a usable one$/,
    );
  });

  it("names a link into the donor worktree", () => {
    const cdb = {
      action: "skipped_link",
      path: "/work/feature/compile_commands.json",
      reason: "points_into_donor",
      targetHasCdb: true,
    } as const;
    const summary: RelocationSummary = { ...SUMMARY, cdb };

    assert.match(describeRelocation(summary), /; compile_commands.json links into the donor worktree, so nothing was written$/);
  });

  it("gives each failure of a run that relocated the shards once, with its reason", () => {
    assert.equal(
      describeRelocation(INCOMPLETE_SUMMARY),
      "relocated 9876 shards from main (12 kept, 2 failed); " +
        "predicting the reindex failed: reading /work/feature/.cache/clangd/index; " +
        "relocating compile_commands.json failed: parsing compilation database",
    );
  });

  it("says the reindex was not predicted when the CLI gave no reason", () => {
    const summary: RelocationSummary = { ...SUMMARY, reindex: undefined };

    assert.equal(describeRelocation(summary), "relocated 9876 shards from main (12 kept, 2 failed); reindex not predicted");
  });

  it("says that the CLI reported an unknown compilation database action", () => {
    const summary: RelocationSummary = { ...SUMMARY, cdb: { action: "unknown", path: undefined, reason: undefined, targetHasCdb: true } };

    assert.match(describeRelocation(summary), /; compile_commands.json: unknown result, see the log$/);
  });

  it("warns when git tracks the compilation database", () => {
    const cdb = {
      action: "kept_tracked",
      path: "/work/feature/compile_commands.json",
      reason: "tracked_by_git",
      targetHasCdb: true,
    } as const;

    assert.match(describeRelocation({ ...SUMMARY, cdb }), /; git tracks compile_commands.json, so it was not replaced$/);
  });
});

describe("hasRelocationProblems", () => {
  it("is false for a complete run", () => {
    assert.equal(hasRelocationProblems(SUMMARY), false);
  });

  it("is true when the compilation database or the prediction failed", () => {
    assert.equal(hasRelocationProblems(INCOMPLETE_SUMMARY), true);
    assert.equal(hasRelocationProblems({ ...SUMMARY, cdb: INCOMPLETE_SUMMARY.cdb }), true);
    assert.equal(hasRelocationProblems({ ...SUMMARY, reindex: undefined, reindexFailure: "x" }), true);
  });
});
