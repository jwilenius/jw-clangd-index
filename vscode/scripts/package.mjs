// Builds the release CLI for one VS Code target, bundles it, and writes that target's .vsix.
// Usage: npm run package -- [--target <target>]; the target defaults to the host's.
import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { parseArgs } from "node:util";

/** VS Code target to Rust target triple; Linux builds use musl so the binary runs on any distribution. */
const RUST_TRIPLES = {
  "linux-x64": "x86_64-unknown-linux-musl",
  "linux-arm64": "aarch64-unknown-linux-musl",
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
};
const BINARY_NAME = "jw-clangd-index";
const CRATE_DIR = "..";
const BIN_DIR = "bin";

const { values } = parseArgs({ options: { target: { type: "string", default: `${process.platform}-${process.arch}` } } });
const target = values.target;
const triple = RUST_TRIPLES[target];
if (triple === undefined) {
  console.error(`package: unsupported target ${target}; supported: ${Object.keys(RUST_TRIPLES).join(", ")}`);
  process.exit(1);
}
assertRustTargetInstalled(triple);
execFileSync("cargo", ["build", "--release", "--target", triple], { cwd: CRATE_DIR, stdio: "inherit" });
// vsce bundles all of bin/, so a binary left from another target would ship in this target's .vsix.
fs.rmSync(BIN_DIR, { recursive: true, force: true });
const targetBinDir = path.join(BIN_DIR, target);
fs.mkdirSync(targetBinDir, { recursive: true });
fs.copyFileSync(path.join(CRATE_DIR, "target", triple, "release", BINARY_NAME), path.join(targetBinDir, BINARY_NAME));
execFileSync("vsce", ["package", "--target", target, "--no-dependencies"], { stdio: "inherit" });

/** Fails with the fix up front, since cargo's error for a missing target does not suggest it. */
function assertRustTargetInstalled(rustTriple) {
  let installedTargets;
  try {
    installedTargets = execFileSync("rustup", ["target", "list", "--installed"], { encoding: "utf8" });
  } catch (error) {
    // Without rustup, cargo reports a missing target itself.
    if (error.code === "ENOENT") {
      return;
    }
    throw error;
  }
  const isInstalled = installedTargets.split(/\r?\n/).includes(rustTriple);
  if (!isInstalled) {
    console.error(`package: the Rust target ${rustTriple} is not installed; run: rustup target add ${rustTriple}`);
    process.exit(1);
  }
}
