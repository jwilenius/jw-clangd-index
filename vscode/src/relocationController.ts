import * as fs from "node:fs";

import * as vscode from "vscode";

import { isInsideExtension, resolveBinaryPath } from "./binaryPath";
import { buildInstallHookArguments, buildRelocateArguments } from "./cliArguments";
import { findCliFailure, interpretRelocationRun, type RelocationResult } from "./cliResult";
import { describeCheckout, selectDonorWorktrees, type WorktreeEntry } from "./gitWorktree";
import { type ProcessResult, runProcess } from "./processRunner";
import {
  type AutoRelocationDecision,
  classifyFolder,
  decideAutoRelocation,
  decideCommandTarget,
  isClangdRestartNeeded,
} from "./relocationPolicy";
import { describeRelocation, hasRelocationProblems } from "./relocationSummary";
import { countIndexShards, countLargestDonorIndex, listWorktrees, readGitLocation, resolveRealPath } from "./worktreeProbe";

const CONFIGURATION_SECTION = "jwClangdIndex";
const CLANGD_EXTENSION_ID = "llvm-vs-code-extensions.vscode-clangd";
const CLANGD_RESTART_COMMAND = "clangd.restart";
const MESSAGE_PREFIX = "jw-clangd-index";
const SHOW_LOG_ACTION = "Show Log";
const STATUS_MESSAGE_TIMEOUT_MS = 15_000;
const RELOCATE_TIMEOUT_MS = 10 * 60_000;
const INSTALL_HOOK_TIMEOUT_MS = 30_000;
const RELOCATE_TARGET_PLACEHOLDER = "Worktree to relocate the index into";
const HOOK_REPOSITORY_PLACEHOLDER = "Repository to install the post-checkout hook in";
/** `relocate --json` lists every predicted TU, which can be many thousand paths. */
const CLI_MAX_BUFFER_BYTES = 64 * 1024 * 1024;

const SKIP_DESCRIPTIONS: Readonly<Record<Exclude<AutoRelocationDecision, "relocate">, string>> = {
  noIndexedDonor: "no other worktree has a clangd index",
  alreadyIndexed: "already indexed",
};

/** Automatic runs report success in the status bar; commands the user started report in a notification. */
type RunOrigin = "automatic" | "command";

interface Settings {
  readonly binaryPath: string;
  readonly isAutoRelocate: boolean;
  readonly isRelocateCdb: boolean;
}

interface CliRun {
  readonly binaryPath: string;
  readonly args: readonly string[];
  readonly cwd: string;
  readonly timeoutMs: number;
}

/** Runs `jw-clangd-index` for workspace folders and tells clangd about the relocated shards. */
export class RelocationController {
  /** Worktree roots with a relocation in progress. */
  private readonly runningRoots = new Set<string>();

  constructor(
    private readonly log: vscode.LogOutputChannel,
    private readonly extensionRoot: string,
  ) {}

  /** Relocates into `folder` when it is a linked worktree with little of an index and the setting allows it. */
  async autoRelocate(folder: vscode.WorkspaceFolder): Promise<void> {
    const settings = this.readSettings(folder);
    const isLocalFolder = folder.uri.scheme === "file";
    if (!settings.isAutoRelocate || !isLocalFolder) {
      return;
    }
    const root = await resolveRealPath(folder.uri.fsPath);
    const gitLocation = await readGitLocation(root);
    if (gitLocation.kind === "failed") {
      this.log.warn(`${root}: cannot tell whether it is a linked worktree, not relocating: ${gitLocation.reason}`);
      return;
    }
    const folderKind = classifyFolder(root, gitLocation);
    if (folderKind !== "linkedWorktree") {
      this.log.debug(`${root}: ${folderKind}, nothing to relocate`);
      return;
    }
    if (await this.isRelocationWanted(root)) {
      await this.relocate(folder, root, undefined, "automatic");
    }
  }

  private async isRelocationWanted(root: string): Promise<boolean> {
    const targetShardCount = await countIndexShards(root);
    const donorShardCount = await countLargestDonorIndex(root);
    const decision = decideAutoRelocation(targetShardCount, donorShardCount);
    if (decision !== "relocate") {
      const description = SKIP_DESCRIPTIONS[decision];
      this.log.info(`${root}: ${description} (${targetShardCount} shards, largest other index ${donorShardCount}); not relocating`);
    }
    return decision === "relocate";
  }

  async relocateCommand(): Promise<void> {
    const folder = await pickLocalFolder(RELOCATE_TARGET_PLACEHOLDER);
    if (folder === undefined) {
      return;
    }
    const root = await this.resolveCommandTarget(folder);
    if (root !== undefined) {
      await this.relocate(folder, root, undefined, "command");
    }
  }

  async relocateFromCommand(): Promise<void> {
    const folder = await pickLocalFolder(RELOCATE_TARGET_PLACEHOLDER);
    if (folder === undefined) {
      return;
    }
    const root = await this.resolveCommandTarget(folder);
    if (root === undefined) {
      return;
    }
    const donor = await pickDonor(root);
    if (donor === undefined) {
      return;
    }
    await this.relocate(folder, root, donor.path, "command");
  }

  /** The worktree root to relocate `folder` into, after telling the user when it is not `folder`; `undefined` when there is none. */
  private async resolveCommandTarget(folder: vscode.WorkspaceFolder): Promise<string | undefined> {
    const folderRoot = await resolveRealPath(folder.uri.fsPath);
    const target = decideCommandTarget(folderRoot, await readGitLocation(folderRoot));
    if (target.kind === "refused") {
      const message = `${MESSAGE_PREFIX}: ${target.message}`;
      this.log.warn(message);
      void vscode.window.showWarningMessage(message);
      return undefined;
    }
    if (target.note !== undefined) {
      const message = `${MESSAGE_PREFIX}: ${target.note}`;
      this.log.info(message);
      void vscode.window.showInformationMessage(message);
    }
    return target.root;
  }

  async installHookCommand(): Promise<void> {
    const folder = await pickLocalFolder(HOOK_REPOSITORY_PLACEHOLDER);
    if (folder === undefined) {
      return;
    }
    const settings = this.readSettings(folder);
    const args = buildInstallHookArguments({ repoRoot: folder.uri.fsPath, isRelocateCdb: settings.isRelocateCdb });
    const binaryPath = this.resolveBinary(settings);
    const result = await this.runCli({ binaryPath, args, cwd: folder.uri.fsPath, timeoutMs: INSTALL_HOOK_TIMEOUT_MS });
    const failure = findCliFailure(result);
    if (failure !== undefined) {
      this.showFailure(`install-hook failed: ${failure}`, "command");
      return;
    }
    const message = `${MESSAGE_PREFIX}: ${result.stdout.trim()}`;
    if (!isInsideExtension(binaryPath, this.extensionRoot)) {
      void vscode.window.showInformationMessage(message);
      return;
    }
    const warning = `${message}. The hook runs the bundled ${binaryPath}, which VS Code deletes when it updates the extension; set ${CONFIGURATION_SECTION}.path to a lasting binary and install the hook again.`;
    this.log.warn(warning);
    void vscode.window.showWarningMessage(warning);
  }

  private async relocate(folder: vscode.WorkspaceFolder, root: string, donorRoot: string | undefined, origin: RunOrigin): Promise<void> {
    if (this.runningRoots.has(root)) {
      const message = `${MESSAGE_PREFIX}: a relocation into ${root} is already running`;
      this.log.info(message);
      if (origin === "command") {
        void vscode.window.showInformationMessage(message);
      } else {
        vscode.window.setStatusBarMessage(message, STATUS_MESSAGE_TIMEOUT_MS);
      }
      return;
    }
    this.runningRoots.add(root);
    try {
      const result = await this.runRelocation(folder, root, donorRoot, origin === "automatic");
      await this.finishRelocation(result, origin);
    } finally {
      this.runningRoots.delete(root);
    }
  }

  /** @param isKeepExisting true for automatic runs, so the shards clangd already wrote survive; a command the user started replaces them. */
  private async runRelocation(
    folder: vscode.WorkspaceFolder,
    root: string,
    donorRoot: string | undefined,
    isKeepExisting: boolean,
  ): Promise<RelocationResult> {
    const settings = this.readSettings(folder);
    const args = buildRelocateArguments({ targetRoot: root, donorRoot, isRelocateCdb: settings.isRelocateCdb, isKeepExisting });
    const progressOptions = { location: vscode.ProgressLocation.Window, title: `${MESSAGE_PREFIX}: relocating index` };
    const processResult = await vscode.window.withProgress(progressOptions, () =>
      this.runCli({ binaryPath: this.resolveBinary(settings), args, cwd: root, timeoutMs: RELOCATE_TIMEOUT_MS }),
    );
    return interpretRelocationRun(processResult);
  }

  private async finishRelocation(result: RelocationResult, origin: RunOrigin): Promise<void> {
    if (result.kind === "failed") {
      this.showFailure(`relocation failed: ${result.reason}`, origin);
      return;
    }
    const isClangdActive = vscode.extensions.getExtension(CLANGD_EXTENSION_ID)?.isActive ?? false;
    if (isClangdRestartNeeded(result.summary, isClangdActive)) {
      await this.restartClangd();
    }
    const message = `${MESSAGE_PREFIX}: ${describeRelocation(result.summary)}`;
    if (hasRelocationProblems(result.summary)) {
      this.log.warn(message);
      this.showWithLogAction(vscode.window.showWarningMessage, message);
      return;
    }
    this.log.info(message);
    if (origin === "automatic") {
      vscode.window.setStatusBarMessage(`$(database) ${message}`, STATUS_MESSAGE_TIMEOUT_MS);
      return;
    }
    void vscode.window.showInformationMessage(message);
  }

  /** A failed restart is only logged: the shards are in place and clangd loads them when it next starts. */
  private async restartClangd(): Promise<void> {
    this.log.info("restarting clangd to load the relocated shards");
    try {
      await vscode.commands.executeCommand(CLANGD_RESTART_COMMAND);
    } catch (error) {
      this.log.error(`${CLANGD_RESTART_COMMAND} failed: ${String(error)}`);
    }
  }

  private resolveBinary(settings: Settings): string {
    return resolveBinaryPath({
      configuredPath: settings.binaryPath,
      extensionRoot: this.extensionRoot,
      platform: process.platform,
      arch: process.arch,
      isFile: isFile,
    });
  }

  private async runCli(run: CliRun): Promise<ProcessResult> {
    this.log.info(`running ${run.binaryPath} ${run.args.join(" ")}`);
    const result = await runProcess({
      file: run.binaryPath,
      args: run.args,
      cwd: run.cwd,
      timeoutMs: run.timeoutMs,
      maxBufferBytes: CLI_MAX_BUFFER_BYTES,
    });
    this.logProcessResult(result);
    return result;
  }

  private logProcessResult(result: ProcessResult): void {
    const outcome = result.kind === "exited" ? `exit code ${result.exitCode}` : result.reason;
    this.log.info(`finished: ${outcome}`);
    if (result.stdout !== "") {
      this.log.info(`stdout:\n${result.stdout.trimEnd()}`);
    }
    if (result.stderr !== "") {
      this.log.info(`stderr:\n${result.stderr.trimEnd()}`);
    }
  }

  /**
   * Failures of automatic runs are warnings, since the user did not ask for the relocation.
   * Does not wait for the notification, so a pending one blocks neither other folders nor another run.
   */
  private showFailure(message: string, origin: RunOrigin): void {
    const text = `${MESSAGE_PREFIX}: ${message}`;
    this.log.error(text);
    const show = origin === "automatic" ? vscode.window.showWarningMessage : vscode.window.showErrorMessage;
    this.showWithLogAction(show, text);
  }

  /** Does not wait for the notification. */
  private showWithLogAction(show: typeof vscode.window.showWarningMessage, text: string): void {
    void show(text, SHOW_LOG_ACTION).then((action) => {
      if (action === SHOW_LOG_ACTION) {
        this.log.show();
      }
    });
  }

  private readSettings(folder: vscode.WorkspaceFolder): Settings {
    const configuration = vscode.workspace.getConfiguration(CONFIGURATION_SECTION, folder.uri);
    return {
      binaryPath: configuration.get<string>("path", ""),
      isAutoRelocate: configuration.get<boolean>("autoRelocate", true),
      isRelocateCdb: configuration.get<boolean>("relocateCdb", true),
    };
  }
}

function isFile(filePath: string): boolean {
  return fs.statSync(filePath, { throwIfNoEntry: false })?.isFile() ?? false;
}

/** The only local workspace folder, or the one the user picks; `undefined` when there is none or the pick is cancelled. */
async function pickLocalFolder(placeHolder: string): Promise<vscode.WorkspaceFolder | undefined> {
  const localFolders = (vscode.workspace.workspaceFolders ?? []).filter((folder) => folder.uri.scheme === "file");
  if (localFolders.length === 0) {
    void vscode.window.showWarningMessage(`${MESSAGE_PREFIX}: open a folder of a git worktree first`);
    return undefined;
  }
  if (localFolders.length === 1) {
    return localFolders[0];
  }
  const items = localFolders.map((folder) => ({ label: folder.name, description: folder.uri.fsPath, folder }));
  const picked = await vscode.window.showQuickPick(items, { placeHolder });
  return picked?.folder;
}

interface DonorPickItem extends vscode.QuickPickItem {
  readonly worktree: WorktreeEntry;
}

async function pickDonor(targetRoot: string): Promise<WorktreeEntry | undefined> {
  const donors = selectDonorWorktrees(await listWorktrees(targetRoot), targetRoot);
  if (donors.length === 0) {
    void vscode.window.showWarningMessage(`${MESSAGE_PREFIX}: ${targetRoot} has no other worktrees`);
    return undefined;
  }
  const items: DonorPickItem[] = donors.map((worktree) => ({ label: describeCheckout(worktree), description: worktree.path, worktree }));
  const picked = await vscode.window.showQuickPick(items, { placeHolder: "Worktree to copy the clangd index from" });
  return picked?.worktree;
}
