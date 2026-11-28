import * as path from "node:path";

/** What `relocate` did with the compilation database. */
const KNOWN_CDB_ACTIONS = [
  "target_has_one",
  "target_has_none",
  "donor_has_none",
  "kept_existing",
  "written",
  "kept_tracked",
  "skipped_link",
  "failed",
] as const;
/** `unknown` stands for an action of a newer CLI, which the extension cannot describe. */
export type CdbAction = (typeof KNOWN_CDB_ACTIONS)[number] | "unknown";
/** Actions after which the target has no compilation database, so clangd ignores the index. */
const CDB_MISSING_ACTIONS: readonly CdbAction[] = ["target_has_none", "donor_has_none"];

export interface ShardCounts {
  readonly relocated: number;
  readonly keptExisting: number;
  readonly skippedDeleted: number;
  readonly skippedUnsupportedVersion: number;
  readonly failed: number;
}

export interface CdbSummary {
  readonly action: CdbAction;
  readonly path: string | undefined;
  /** Why the database was not written, for `kept_tracked`, `skipped_link` and `failed`. */
  readonly reason: string | undefined;
}

export interface ReindexSummary {
  /** False when the prediction holds only once the target has a compilation database. */
  readonly hasCdbMainFiles: boolean;
  readonly reindexedTus: readonly string[];
  readonly staleFiles: number;
  readonly mainFilesWithoutShard: readonly string[];
}

/** The summary `jw-clangd-index relocate --json` prints. */
export interface RelocationSummary {
  readonly isDryRun: boolean;
  readonly donorRoot: string;
  /** `undefined` when the donor was given with `--from`. */
  readonly donorChangedFiles: number | undefined;
  readonly targetRoot: string;
  readonly shards: ShardCounts;
  readonly cdb: CdbSummary;
  /** `undefined` when the prediction failed. */
  readonly reindex: ReindexSummary | undefined;
  /** Why the prediction failed; `undefined` when `reindex` holds it. */
  readonly reindexFailure: string | undefined;
  readonly elapsedSeconds: number;
}

export class SummaryFormatError extends Error {
  override readonly name = "SummaryFormatError";
}

type JsonObject = Readonly<Record<string, unknown>>;

/**
 * Optional fields may be missing as well as `null`, since a CLI built before they were added omits them.
 * @throws SyntaxError when `text` is not JSON, {@link SummaryFormatError} when a required field is missing or a field is mistyped.
 */
export function parseRelocationSummary(text: string): RelocationSummary {
  const summary = asObject(JSON.parse(text), "summary");
  return {
    isDryRun: readBoolean(summary, "is_dry_run"),
    donorRoot: readString(summary, "donor_root"),
    donorChangedFiles: readOptionalNumber(summary, "donor_changed_files"),
    targetRoot: readString(summary, "target_root"),
    shards: parseShardCounts(asObject(summary["shards"], "shards")),
    cdb: parseCdbSummary(asObject(summary["cdb"], "cdb")),
    reindex: summary["reindex"] === null ? undefined : parseReindexSummary(asObject(summary["reindex"], "reindex")),
    reindexFailure: readOptionalString(summary, "reindex_failure"),
    elapsedSeconds: readNumber(summary, "elapsed_seconds"),
  };
}

function parseShardCounts(shards: JsonObject): ShardCounts {
  return {
    relocated: readNumber(shards, "relocated"),
    keptExisting: readNumber(shards, "kept_existing"),
    skippedDeleted: readNumber(shards, "skipped_deleted"),
    skippedUnsupportedVersion: readNumber(shards, "skipped_unsupported_version"),
    failed: readNumber(shards, "failed"),
  };
}

function parseCdbSummary(cdb: JsonObject): CdbSummary {
  const action = readString(cdb, "action");
  return {
    action: isKnownCdbAction(action) ? action : "unknown",
    path: readOptionalString(cdb, "path"),
    reason: readOptionalString(cdb, "reason"),
  };
}

function isKnownCdbAction(action: string): action is CdbAction {
  return (KNOWN_CDB_ACTIONS as readonly string[]).includes(action);
}

function parseReindexSummary(reindex: JsonObject): ReindexSummary {
  return {
    hasCdbMainFiles: readBoolean(reindex, "has_cdb_main_files"),
    reindexedTus: readStringArray(reindex, "reindexed_tus"),
    staleFiles: readNumber(reindex, "stale_files"),
    mainFilesWithoutShard: readStringArray(reindex, "main_files_without_shard"),
  };
}

function asObject(value: unknown, name: string): JsonObject {
  const isObject = typeof value === "object" && value !== null && !Array.isArray(value);
  if (!isObject) {
    throw new SummaryFormatError(`${name} is not an object`);
  }
  return value as JsonObject;
}

function readString(object: JsonObject, key: string): string {
  const value = object[key];
  if (typeof value !== "string") {
    throw new SummaryFormatError(`${key} is not a string`);
  }
  return value;
}

function readOptionalString(object: JsonObject, key: string): string | undefined {
  const value = object[key];
  if (value === undefined || value === null) {
    return undefined;
  }
  if (typeof value !== "string") {
    throw new SummaryFormatError(`${key} is neither a string nor null`);
  }
  return value;
}

function readNumber(object: JsonObject, key: string): number {
  const value = object[key];
  if (typeof value !== "number") {
    throw new SummaryFormatError(`${key} is not a number`);
  }
  return value;
}

function readOptionalNumber(object: JsonObject, key: string): number | undefined {
  const value = object[key];
  return value === undefined || value === null ? undefined : readNumber(object, key);
}

function readBoolean(object: JsonObject, key: string): boolean {
  const value = object[key];
  if (typeof value !== "boolean") {
    throw new SummaryFormatError(`${key} is not a boolean`);
  }
  return value;
}

function readStringArray(object: JsonObject, key: string): string[] {
  const value = object[key];
  const isStringArray = Array.isArray(value) && value.every((item) => typeof item === "string");
  if (!isStringArray) {
    throw new SummaryFormatError(`${key} is not an array of strings`);
  }
  return value;
}

/** One line for a notification, such as `relocated 9876 shards from main (12 kept); 3 TUs to reindex`. */
export function describeRelocation(summary: RelocationSummary): string {
  const { shards } = summary;
  const verb = summary.isDryRun ? "would relocate" : "relocated";
  const keptAndFailed = `${shards.keptExisting} kept, ${shards.failed} failed`;
  const reindex = describeReindex(summary);
  const cdbWarning = describeCdbWarning(summary);
  return `${verb} ${shards.relocated} shards from ${path.basename(summary.donorRoot)} (${keptAndFailed}); ${reindex}${cdbWarning}`;
}

function describeReindex(summary: RelocationSummary): string {
  if (summary.reindex !== undefined) {
    return `${summary.reindex.reindexedTus.length} TUs to reindex`;
  }
  if (summary.reindexFailure !== undefined) {
    return `predicting the reindex failed: ${summary.reindexFailure}`;
  }
  return "reindex not predicted";
}

function describeCdbWarning(summary: RelocationSummary): string {
  const { action } = summary.cdb;
  if (CDB_MISSING_ACTIONS.includes(action)) {
    return "; no compile_commands.json, so clangd ignores the index";
  }
  if (action === "failed") {
    return `; relocating compile_commands.json failed: ${summary.cdb.reason ?? "unknown reason"}`;
  }
  if (action === "kept_tracked") {
    return "; git tracks compile_commands.json, so it was not replaced";
  }
  if (action === "skipped_link") {
    return describeSkippedLink(summary);
  }
  if (action === "unknown") {
    return "; compile_commands.json: unknown result, see the log";
  }
  return "";
}

/** Where a skipped `compile_commands.json` link points, by its `reason`. */
const LINK_TARGETS: Readonly<Record<string, string>> = {
  points_at_donor_cdb: "to the donor's database",
  points_into_donor: "into the donor worktree",
  leaves_target: "outside the worktree",
};

function describeSkippedLink(summary: RelocationSummary): string {
  const linkTarget = LINK_TARGETS[summary.cdb.reason ?? ""] ?? "outside the worktree";
  const consequence =
    summary.reindex?.hasCdbMainFiles === false ? ", and clangd ignores the index until the worktree has a usable one" : "";
  return `; compile_commands.json links ${linkTarget}, so nothing was written${consequence}`;
}

/** Whether something went wrong in a run that relocated the shards, so it deserves a warning; {@link describeRelocation} gives the reasons. */
export function hasRelocationProblems(summary: RelocationSummary): boolean {
  return summary.cdb.action === "failed" || summary.reindexFailure !== undefined;
}
