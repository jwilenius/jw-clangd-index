import * as assert from "node:assert/strict";
import { describe, it } from "node:test";

import { BINARY_NAME, type BinaryPathRequest, isInsideExtension, resolveBinaryPath } from "../src/binaryPath";

const EXTENSION_ROOT = "/ext";
const BUNDLED_LINUX_BINARY = "/ext/bin/linux-x64/jw-clangd-index";

function request(overrides: Partial<BinaryPathRequest>): BinaryPathRequest {
  return { configuredPath: "", extensionRoot: EXTENSION_ROOT, platform: "linux", arch: "x64", isFile: () => true, ...overrides };
}

describe("resolveBinaryPath", () => {
  it("prefers the configured path", () => {
    assert.equal(resolveBinaryPath(request({ configuredPath: " /opt/jw-clangd-index " })), "/opt/jw-clangd-index");
  });

  it("uses the bundled binary of the platform when it exists", () => {
    assert.equal(resolveBinaryPath(request({ isFile: (filePath) => filePath === BUNDLED_LINUX_BINARY })), BUNDLED_LINUX_BINARY);
  });

  it("falls back to the binary on PATH without a bundled one", () => {
    assert.equal(resolveBinaryPath(request({ isFile: () => false })), BINARY_NAME);
  });
});

describe("isInsideExtension", () => {
  it("is true for the bundled binary", () => {
    assert.equal(isInsideExtension(BUNDLED_LINUX_BINARY, EXTENSION_ROOT), true);
  });

  it("is false for a binary elsewhere, even with a common name prefix", () => {
    assert.equal(isInsideExtension("/opt/jw-clangd-index", EXTENSION_ROOT), false);
    assert.equal(isInsideExtension("/ext-other/jw-clangd-index", EXTENSION_ROOT), false);
  });

  it("is false for the bare name looked up on PATH", () => {
    assert.equal(isInsideExtension(BINARY_NAME, EXTENSION_ROOT), false);
  });
});
