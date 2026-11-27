// Builds the release CLI, bundles it for the only target it is built for, and writes the .vsix.
import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";

const SUPPORTED_TARGET = "linux-x64";
const BINARY_NAME = "jw-clangd-index";
const CRATE_DIR = "..";

const target = `${process.platform}-${process.arch}`;
if (target !== SUPPORTED_TARGET) {
  console.error(`package: builds only on ${SUPPORTED_TARGET}, not ${target}`);
  process.exit(1);
}
execFileSync("cargo", ["build", "--release"], { cwd: CRATE_DIR, stdio: "inherit" });
const binDir = path.join("bin", target);
fs.mkdirSync(binDir, { recursive: true });
fs.copyFileSync(path.join(CRATE_DIR, "target", "release", BINARY_NAME), path.join(binDir, BINARY_NAME));
execFileSync("vsce", ["package", "--target", target, "--skip-license"], { stdio: "inherit" });
