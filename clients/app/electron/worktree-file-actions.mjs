import { lstatSync } from "node:fs";
import path from "node:path";

const MAX_PATHS = 10_000;
const MAX_PATH_TEXT = 1024 * 1024;

function resolvePath(root, relativePath) {
  if (
    typeof relativePath !== "string" ||
    relativePath.includes("\0") ||
    relativePath.includes("\\") ||
    path.posix.isAbsolute(relativePath) ||
    path.win32.parse(relativePath).root !== "" ||
    (relativePath !== "" && relativePath.split("/").some((part) =>
      part === "" || part === "." || part === ".."))
  ) {
    throw new Error("The file path must stay inside the worktree.");
  }
  const absolutePath = path.resolve(root, relativePath);
  const relative = path.relative(root, absolutePath);
  if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
    throw new Error("The file path must stay inside the worktree.");
  }
  // Preserve the path displayed by the tree, including a symlink's own name.
  // These actions copy path text or reveal an entry; neither opens its contents.
  return absolutePath;
}

/** The same handler serves production IPC and the isolated Electron test. */
export function createWorktreeFileActionHandler({ getSession, writeClipboard, revealInFileManager }) {
  return async (event, request) => {
    try {
      const session = getSession();
      if (
        session === null || !session.ready || session.stopping ||
        event.sender !== session.contents ||
        !event.senderFrame || event.senderFrame !== session.contents.mainFrame ||
        !request || request.root !== session.root ||
        !Number.isSafeInteger(request.generation) || request.generation !== session.generation
      ) {
        throw new Error("This worktree has changed. Select the file again.");
      }
      if (request.action !== "copyPath" && request.action !== "revealInFileManager") {
        throw new Error("Unknown worktree file action.");
      }
      if (
        !Array.isArray(request.paths) || request.paths.length === 0 ||
        request.paths.length > MAX_PATHS ||
        (request.action === "revealInFileManager" && request.paths.length !== 1)
      ) {
        throw new Error("Select a valid set of files.");
      }
      let textLength = 0;
      const paths = request.paths.map((relativePath) => {
        const resolved = resolvePath(session.root, relativePath);
        textLength += resolved.length + 1;
        if (textLength > MAX_PATH_TEXT) throw new Error("Too many paths to copy at once.");
        return resolved;
      });
      if (request.action === "copyPath") {
        await writeClipboard(paths.join("\n"));
      } else {
        // lstat also accepts a dangling symlink, which Finder can still reveal.
        try {
          lstatSync(paths[0]);
        } catch {
          throw new Error("That file is no longer available in the worktree.");
        }
        await revealInFileManager(paths[0]);
      }
      return { ok: true };
    } catch (error) {
      return {
        ok: false,
        error: error instanceof Error ? error.message : "Could not complete the file action.",
      };
    }
  };
}
