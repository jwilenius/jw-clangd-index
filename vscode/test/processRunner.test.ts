import * as assert from "node:assert/strict";
import * as os from "node:os";
import { describe, it } from "node:test";

import { type ProcessRequest, runProcess } from "../src/processRunner";

const TIMEOUT_MS = 5_000;
const SHORT_TIMEOUT_MS = 200;
const MAX_BUFFER_BYTES = 1024 * 1024;

function nodeRequest(script: string, timeoutMs: number = TIMEOUT_MS): ProcessRequest {
  return { file: process.execPath, args: ["-e", script], cwd: os.tmpdir(), timeoutMs, maxBufferBytes: MAX_BUFFER_BYTES };
}

describe("runProcess", () => {
  it("captures the exit code and both streams", async () => {
    const result = await runProcess(nodeRequest("process.stdout.write('out'); process.stderr.write('err'); process.exit(2)"));

    assert.deepEqual(result, { kind: "exited", exitCode: 2, stdout: "out", stderr: "err" });
  });

  it("adds the requested environment", async () => {
    const result = await runProcess({ ...nodeRequest("process.stdout.write(process.env.JW_PROBE ?? '')"), env: { JW_PROBE: "set" } });

    assert.deepEqual(result, { kind: "exited", exitCode: 0, stdout: "set", stderr: "" });
  });

  it("reports a missing binary", async () => {
    const result = await runProcess({ ...nodeRequest(""), file: "/nonexistent/jw-clangd-index" });

    assert.equal(result.kind, "failed");
    assert.equal(result.kind === "failed" && result.reason, "/nonexistent/jw-clangd-index not found");
  });

  it("stops a process that exceeds the timeout", async () => {
    const result = await runProcess(nodeRequest("setTimeout(() => {}, 10000)", SHORT_TIMEOUT_MS));

    assert.equal(result.kind, "failed");
    assert.equal(
      result.kind === "failed" && result.reason,
      `${process.execPath} was stopped by SIGTERM after the timeout of ${SHORT_TIMEOUT_MS} ms`,
    );
  });

  it("stops a process that prints too much", async () => {
    const result = await runProcess(nodeRequest(`process.stdout.write("x".repeat(${2 * MAX_BUFFER_BYTES}))`));

    assert.equal(result.kind, "failed");
    assert.equal(
      result.kind === "failed" && result.reason,
      `${process.execPath} printed more than ${MAX_BUFFER_BYTES} bytes and was stopped`,
    );
  });

  it("names the signal of a process that killed itself", async () => {
    const result = await runProcess(nodeRequest("process.kill(process.pid, 'SIGKILL')"));

    assert.equal(result.kind, "failed");
    assert.equal(result.kind === "failed" && result.reason, `${process.execPath} was stopped by SIGKILL`);
  });
});
