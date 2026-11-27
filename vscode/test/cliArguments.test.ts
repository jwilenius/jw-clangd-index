import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import { buildInstallHookArguments, buildRelocateArguments } from "../src/cliArguments";

describe("buildRelocateArguments", () => {
  it("lets the CLI pick the donor, relocates the compilation database and keeps existing files when asked", () => {
    const args = buildRelocateArguments({ targetRoot: "/work/feature", donorRoot: undefined, isRelocateCdb: true, isKeepExisting: true });

    assert.deepEqual(args, ["relocate", "--json", "--keep-existing", "--to", "/work/feature"]);
  });

  it("passes an explicit donor and --no-relocate-cdb when disabled, without --keep-existing when not asked", () => {
    const args = buildRelocateArguments({
      targetRoot: "/work/feature",
      donorRoot: "/work/main",
      isRelocateCdb: false,
      isKeepExisting: false,
    });

    assert.deepEqual(args, ["relocate", "--json", "--to", "/work/feature", "--from", "/work/main", "--no-relocate-cdb"]);
  });
});

describe("buildInstallHookArguments", () => {
  it("installs the hook for the repository, with --no-relocate-cdb when disabled", () => {
    assert.deepEqual(buildInstallHookArguments({ repoRoot: "/work/main", isRelocateCdb: true }), ["install-hook", "--repo", "/work/main"]);
    assert.deepEqual(buildInstallHookArguments({ repoRoot: "/work/main", isRelocateCdb: false }), [
      "install-hook",
      "--repo",
      "/work/main",
      "--no-relocate-cdb",
    ]);
  });
});
