import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import { findCliFailure, interpretRelocationRun } from "../src/cliResult";
import type { ProcessResult } from "../src/processRunner";

const DRY_RUN_SUMMARY = JSON.stringify({
  is_dry_run: true,
  donor_root: "/work/main",
  donor_changed_files: null,
  target_root: "/work/feature",
  shards: { relocated: 3, kept_existing: 0, skipped_deleted: 0, skipped_unsupported_version: 0, failed: 0 },
  cdb: { action: "donor_has_none", path: null, reason: null, target_has_cdb: false },
  reindex: { has_cdb_main_files: false, reindexed_tus: [], stale_files: 0, main_files_without_shard: [] },
  reindex_failure: null,
  elapsed_seconds: 0.01,
});
/** What a binary without `--json` prints, after clap's ANSI styling is stripped by the pipe. */
const UNKNOWN_JSON_FLAG_STDERR =
  "error: unexpected argument '--json' found\n\n" +
  "  tip: to pass '--json' as a value, use '-- --json'\n\n" +
  "Usage: jw-clangd-index relocate [OPTIONS]\n\n" +
  "For more information, try '--help'.\n";
const NO_DONOR_STDERR =
  "[WARN ] cannot read the donor's HEAD\n" +
  "error: no other worktree of /work/feature has a clangd index in .cache/clangd/index; pass --from DONOR_ROOT\n";

function exited(exitCode: number, stdout: string, stderr = ""): ProcessResult {
  return { kind: "exited", exitCode, stdout, stderr };
}

describe("findCliFailure", () => {
  it("is undefined after exit code 0", () => {
    assert.equal(findCliFailure(exited(0, "installed\n")), undefined);
  });

  it("takes the message of the CLI's error line", () => {
    const failure = findCliFailure(exited(2, "", NO_DONOR_STDERR));

    assert.equal(failure, "no other worktree of /work/feature has a clangd index in .cache/clangd/index; pass --from DONOR_ROOT");
  });

  it("names the exit code without an error line", () => {
    assert.equal(findCliFailure(exited(101, "", "thread 'main' panicked")), "exited with 101");
  });

  it("passes on why the process did not run", () => {
    assert.equal(
      findCliFailure({ kind: "failed", reason: "jw-clangd-index not found", stdout: "", stderr: "" }),
      "jw-clangd-index not found",
    );
  });
});

describe("interpretRelocationRun", () => {
  it("parses the summary of a successful run", () => {
    const result = interpretRelocationRun(exited(0, DRY_RUN_SUMMARY));

    assert.equal(result.kind, "relocated");
    assert.equal(result.kind === "relocated" && result.summary.shards.relocated, 3);
  });

  it("fails with the CLI's error", () => {
    const result = interpretRelocationRun(exited(2, "", NO_DONOR_STDERR));

    assert.equal(result.kind, "failed");
    assert.match(result.kind === "failed" ? result.reason : "", /^no other worktree/);
  });

  it("fails with clap's error when the binary has no --json", () => {
    const result = interpretRelocationRun(exited(2, "", UNKNOWN_JSON_FLAG_STDERR));

    assert.equal(result.kind, "failed");
    assert.equal(result.kind === "failed" && result.reason, "unexpected argument '--json' found");
  });

  it("fails on output that is not a summary", () => {
    const result = interpretRelocationRun(exited(0, "donor:    /work/main\n"));

    assert.equal(result.kind, "failed");
    assert.match(result.kind === "failed" ? result.reason : "", /^unexpected relocate output \(/);
  });
});
