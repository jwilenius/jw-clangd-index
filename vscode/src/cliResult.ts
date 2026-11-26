import type { ProcessResult } from "./processRunner";
import { parseRelocationSummary, type RelocationSummary, SummaryFormatError } from "./relocationSummary";

const SUCCESS_EXIT_CODE = 0;
/** The CLI prints a fatal error as one `error: ...` line on stderr. */
const ERROR_LINE_PREFIX = "error: ";

export type RelocationResult =
  { readonly kind: "relocated"; readonly summary: RelocationSummary } | { readonly kind: "failed"; readonly reason: string };

/** `undefined` when the CLI ran and exited with 0, else why it failed. */
export function findCliFailure(result: ProcessResult): string | undefined {
  if (result.kind === "failed") {
    return result.reason;
  }
  if (result.exitCode === SUCCESS_EXIT_CODE) {
    return undefined;
  }
  return findErrorMessage(result.stderr) ?? `exited with ${result.exitCode}`;
}

function findErrorMessage(stderr: string): string | undefined {
  const errorLines = stderr.split("\n").filter((line) => line.startsWith(ERROR_LINE_PREFIX));
  return errorLines.at(-1)?.slice(ERROR_LINE_PREFIX.length);
}

/** The summary of a `relocate --json` run, or why there is none. */
export function interpretRelocationRun(result: ProcessResult): RelocationResult {
  const failure = findCliFailure(result);
  if (failure !== undefined) {
    return { kind: "failed", reason: failure };
  }
  try {
    return { kind: "relocated", summary: parseRelocationSummary(result.stdout) };
  } catch (error) {
    const isUnexpectedOutput = error instanceof SyntaxError || error instanceof SummaryFormatError;
    if (!isUnexpectedOutput) {
      throw error;
    }
    return { kind: "failed", reason: `unexpected relocate output (${error.message})` };
  }
}
