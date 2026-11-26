import * as path from "node:path";

/** Name of the CLI, also the command looked up on `PATH` when no binary is bundled. */
export const BINARY_NAME = "jw-clangd-index";
const BUNDLED_BINARY_DIR = "bin";

export interface BinaryPathRequest {
  /** The `jwClangdIndex.path` setting; empty means unset. */
  readonly configuredPath: string;
  readonly extensionRoot: string;
  readonly platform: string;
  readonly arch: string;
  readonly isFile: (filePath: string) => boolean;
}

/** The configured binary, else the bundled `bin/<platform>-<arch>/jw-clangd-index`, else the one on `PATH`. */
export function resolveBinaryPath(request: BinaryPathRequest): string {
  const configuredPath = request.configuredPath.trim();
  if (configuredPath !== "") {
    return configuredPath;
  }
  const bundledPath = bundledBinaryPath(request.extensionRoot, request.platform, request.arch);
  return request.isFile(bundledPath) ? bundledPath : BINARY_NAME;
}

/** Whether `binaryPath` is inside the extension, whose versioned directory VS Code deletes on update. */
export function isInsideExtension(binaryPath: string, extensionRoot: string): boolean {
  const relativePath = path.relative(extensionRoot, binaryPath);
  return relativePath !== "" && !relativePath.startsWith("..") && !path.isAbsolute(relativePath);
}

function bundledBinaryPath(extensionRoot: string, platform: string, arch: string): string {
  return path.join(extensionRoot, BUNDLED_BINARY_DIR, `${platform}-${arch}`, BINARY_NAME);
}
