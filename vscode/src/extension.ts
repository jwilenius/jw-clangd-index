import * as vscode from "vscode";

import { RelocationController } from "./relocationController";
import { SerialQueue } from "./serialQueue";

const OUTPUT_CHANNEL_NAME = "jw-clangd-index";

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel(OUTPUT_CHANNEL_NAME, { log: true });
  const controller = new RelocationController(log, context.extensionPath);
  // One folder at a time, including those added later, so that relocations do not compete for the disk.
  const queue = new SerialQueue<vscode.WorkspaceFolder>(
    (folder) => controller.autoRelocate(folder),
    (folder, error) => log.error(`automatic relocation into ${folder.uri.fsPath} failed: ${String(error)}`),
  );
  context.subscriptions.push(
    log,
    vscode.commands.registerCommand("jwClangdIndex.relocate", () => controller.relocateCommand()),
    vscode.commands.registerCommand("jwClangdIndex.relocateFrom", () => controller.relocateFromCommand()),
    vscode.commands.registerCommand("jwClangdIndex.installHook", () => controller.installHookCommand()),
    vscode.workspace.onDidChangeWorkspaceFolders((event) => queue.add(event.added)),
  );
  queue.add(vscode.workspace.workspaceFolders ?? []);
}

export function deactivate(): void {}
