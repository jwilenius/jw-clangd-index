import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import { buildInstallHookArguments, buildRelocateArguments } from "../src/cliArguments";

describe("buildRelocateArguments", () => {
  it("lets the CLI pick the donor and relocates the compilation database", () => {
    const args = buildRelocateArguments({ targetRoot: "/work/feature", donorRoot: undefined, isRelocateCdb: true });

    assert.deepEqual(args, ["relocate", "--json", "--to", "/work/feature", "--relocate-cdb"]);
  });

  it("passes an explicit donor and never overwrites", () => {
    const args = buildRelocateArguments({ targetRoot: "/work/feature", donorRoot: "/work/main", isRelocateCdb: false });

    assert.deepEqual(args, ["relocate", "--json", "--to", "/work/feature", "--from", "/work/main"]);
  });
});

describe("buildInstallHookArguments", () => {
  it("installs the hook for the repository, with --relocate-cdb when enabled", () => {
    assert.deepEqual(buildInstallHookArguments({ repoRoot: "/work/main", isRelocateCdb: true }), [
      "install-hook",
      "--repo",
      "/work/main",
      "--relocate-cdb",
    ]);
    assert.deepEqual(buildInstallHookArguments({ repoRoot: "/work/main", isRelocateCdb: false }), ["install-hook", "--repo", "/work/main"]);
  });
});
