import { execFile, type ExecFileException } from "node:child_process";

const NOT_FOUND_CODE = "ENOENT";
const MAX_BUFFER_CODE = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER";

export interface ProcessRequest {
  readonly file: string;
  readonly args: readonly string[];
  readonly cwd: string;
  readonly timeoutMs: number;
  readonly maxBufferBytes: number;
  /** Added to the extension host's environment. */
  readonly env?: Readonly<Record<string, string>>;
}

/** `exited` for any exit code; `failed` when the process could not start or was killed (timeout, signal, output too large). */
export type ProcessResult =
  | { readonly kind: "exited"; readonly exitCode: number; readonly stdout: string; readonly stderr: string }
  | { readonly kind: "failed"; readonly reason: string; readonly stdout: string; readonly stderr: string };

/** Runs `request.file` without a shell; never rejects. */
export function runProcess(request: ProcessRequest): Promise<ProcessResult> {
  const env = { ...process.env, ...request.env };
  const options = { cwd: request.cwd, env, timeout: request.timeoutMs, maxBuffer: request.maxBufferBytes, encoding: "utf8" as const };
  return new Promise((resolve) => {
    execFile(request.file, request.args, options, (error, stdout, stderr) => {
      resolve(toProcessResult(request, error, stdout, stderr));
    });
  });
}

function toProcessResult(request: ProcessRequest, error: ExecFileException | null, stdout: string, stderr: string): ProcessResult {
  if (error === null) {
    return { kind: "exited", exitCode: 0, stdout, stderr };
  }
  const isExitCode = typeof error.code === "number" && error.signal == null;
  if (isExitCode) {
    return { kind: "exited", exitCode: Number(error.code), stdout, stderr };
  }
  return { kind: "failed", reason: describeFailure(request, error), stdout, stderr };
}

/** Never `error.message`, which repeats the whole command line and stderr. */
function describeFailure(request: ProcessRequest, error: ExecFileException): string {
  if (error.code === NOT_FOUND_CODE) {
    return `${request.file} not found`;
  }
  if (error.code === MAX_BUFFER_CODE) {
    return `${request.file} printed more than ${request.maxBufferBytes} bytes and was stopped`;
  }
  if (error.signal != null) {
    const timeoutNote = error.killed === true ? ` after the timeout of ${request.timeoutMs} ms` : "";
    return `${request.file} was stopped by ${error.signal}${timeoutNote}`;
  }
  return `${request.file} could not run (${String(error.code ?? error.name)})`;
}
