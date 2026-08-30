// The adapter between the git column and the file viewer — the only file that
// knows both what a `GitClient` is and what a `FileSource` is.
//
// The viewer shows one file. A diff is one file with two versions, so the
// same dialog serves both: the column hands it a handle whose `ref` names the
// side of the index to diff against, and the viewer neither knows nor asks
// where the text came from.

import type { FileHandle, FileSource } from "../file-viewer";
import type { DiffRequest, GitClient } from "./types";

/** What the git column puts in a handle's opaque `ref`. */
interface GitRef {
  readonly request: DiffRequest;
}

const isGitRef = (value: unknown): value is GitRef =>
  value !== null && typeof value === "object" && "request" in value;

/** A handle for one side of one file's change. */
export function diffHandle(
  request: DiffRequest,
  options: { readonly root: string; readonly label: string },
): FileHandle {
  const name = request.path.slice(request.path.lastIndexOf("/") + 1);
  return {
    // The side is part of the identity: the staged and unstaged halves of one
    // file are two different documents, and switching between them must
    // remount rather than reuse the last read.
    key: `${options.root}#${request.sha ?? (request.staged === true ? "index" : "worktree")}#${request.path}`,
    name,
    path: `${options.root}/${request.path}`,
    // A diff's size is not the file's size, and the viewer's byte limits are
    // about what it must decode. git's output is bounded host-side.
    size: 0,
    ref: { request } satisfies GitRef,
  };
}

/** Which side this handle is a view of, for the dialog's caption. */
export function diffSideLabel(request: DiffRequest): string {
  if (request.sha !== undefined) return request.sha.slice(0, 7);
  return request.staged === true ? "staged" : "unstaged";
}

/**
 * A `FileSource` that answers with diffs.
 *
 * `read` is never called — every kind this source serves declares the
 * `compare` payload — but the contract requires it, and throwing names the
 * mistake rather than returning empty bytes that would render as a blank
 * file.
 */
export function createGitFileSource(client: GitClient): FileSource {
  return {
    read() {
      return Promise.reject(new Error("a diff is read through compare(), not read()"));
    },
    async compare(handle: FileHandle) {
      if (!isGitRef(handle.ref)) throw new Error("that file is no longer in the change set");
      const answer = await client.diff(handle.ref.request);
      if (answer.error !== null) throw new Error(answer.error);
      return { unified: answer.text };
    },
  };
}
