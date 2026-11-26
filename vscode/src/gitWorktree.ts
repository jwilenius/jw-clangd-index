/** Arguments that print the worktree root, its git directory and the common git directory, one per line. */
export const REV_PARSE_LOCATION_ARGS: readonly string[] = [
  "rev-parse",
  "--show-toplevel",
  "--absolute-git-dir",
  "--path-format=absolute",
  "--git-common-dir",
];
export const WORKTREE_LIST_ARGS: readonly string[] = ["worktree", "list", "--porcelain", "-z"];

const LINE_SEPARATOR = /\r?\n/;
const FIELD_SEPARATOR = "\0";
const ATTRIBUTE_SEPARATOR = " ";
const BRANCH_REF_PREFIX = "refs/heads/";
const SHORT_HEAD_LENGTH = 7;

/** Where git places a worktree; a linked worktree's git directory differs from the common one. */
export interface GitLocation {
  readonly toplevel: string;
  readonly gitDir: string;
  readonly commonDir: string;
}

/** One record of `git worktree list --porcelain -z`. */
export interface WorktreeEntry {
  readonly path: string;
  readonly head: string | undefined;
  /** Short branch name; `undefined` when detached or bare. */
  readonly branch: string | undefined;
  readonly isBare: boolean;
  /** The directory is gone; `git worktree prune` would remove the entry. */
  readonly isPrunable: boolean;
}

/** `undefined` unless the output of {@link REV_PARSE_LOCATION_ARGS} has its three lines. */
export function parseGitLocation(output: string): GitLocation | undefined {
  const lines = output.split(LINE_SEPARATOR).filter((line) => line !== "");
  const [toplevel, gitDir, commonDir] = lines;
  if (lines.length !== 3 || toplevel === undefined || gitDir === undefined || commonDir === undefined) {
    return undefined;
  }
  return { toplevel, gitDir, commonDir };
}

export function isLinkedWorktree(location: GitLocation): boolean {
  return location.gitDir !== location.commonDir;
}

/** Records are NUL-terminated attribute lines, each record ended by an empty field. */
export function parseWorktreeList(output: string): WorktreeEntry[] {
  const records: string[][] = [[]];
  for (const field of output.split(FIELD_SEPARATOR)) {
    if (field === "") {
      records.push([]);
      continue;
    }
    records[records.length - 1]?.push(field);
  }
  return records.filter((fields) => fields.length > 0).flatMap(parseWorktreeRecord);
}

/** Empty unless the record starts with its `worktree` line; unknown attributes are ignored. */
function parseWorktreeRecord(fields: readonly string[]): WorktreeEntry[] {
  const attributes = new Map(fields.map(splitAttribute));
  const worktreePath = attributes.get("worktree");
  if (worktreePath === undefined) {
    return [];
  }
  const branchRef = attributes.get("branch");
  return [
    {
      path: worktreePath,
      head: attributes.get("HEAD"),
      branch: branchRef === undefined ? undefined : shortBranchName(branchRef),
      isBare: attributes.has("bare"),
      isPrunable: attributes.has("prunable"),
    },
  ];
}

function shortBranchName(branchRef: string): string {
  return branchRef.startsWith(BRANCH_REF_PREFIX) ? branchRef.slice(BRANCH_REF_PREFIX.length) : branchRef;
}

function splitAttribute(field: string): [string, string] {
  const separatorIndex = field.indexOf(ATTRIBUTE_SEPARATOR);
  if (separatorIndex < 0) {
    return [field, ""];
  }
  return [field.slice(0, separatorIndex), field.slice(separatorIndex + 1)];
}

/** The live checkouts other than `targetRoot`, in `git worktree list` order. */
export function selectDonorWorktrees(entries: readonly WorktreeEntry[], targetRoot: string): WorktreeEntry[] {
  return entries.filter((entry) => !entry.isBare && !entry.isPrunable && entry.path !== targetRoot);
}

/** The branch name, or the short `HEAD` of a detached worktree. */
export function describeCheckout(entry: WorktreeEntry): string {
  if (entry.branch !== undefined) {
    return entry.branch;
  }
  const shortHead = entry.head?.slice(0, SHORT_HEAD_LENGTH) ?? "unknown";
  return `detached at ${shortHead}`;
}
