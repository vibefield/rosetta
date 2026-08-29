// The adapter between the tree and the viewer: it is the ONLY file that knows
// both what a mille engine is and what a `FileSource` is.
//
// Bytes come back over the port the tree is already using, which means the
// renderer never opens a file itself. There is no `node:fs` here, no new IPC
// channel, and no path handed to a privileged process to read — the request
// is an entry id, and the engine will only answer for entries inside the root
// it was forked against. The read is bounded by the same walk the tree is.

import type { Entry } from "@vibecook/mille";
import type { FileHandle, FileSource } from "../file-viewer";
import type { WorktreeEngine } from "./types";

/** `EntryKind.Directory`. A folder toggles; it does not open. */
const DIRECTORY = 1;

/** Deep enough for any real repository, shallow enough that a mirror that
 * somehow cycles cannot hang the renderer. */
const MAX_DEPTH = 64;

/**
 * The absolute path of an entry, walked up the mirror.
 *
 * `null` is a real answer: the mirror is viewport-bounded, so an ancestor can
 * have been evicted, and the column would rather show no path than a wrong
 * one. Nothing load-bearing depends on it — reading uses the id — so a null
 * costs the reader the path line and the two acts that need a path.
 */
function pathOf(engine: WorktreeEngine, id: number): string | null {
  const snapshot = engine.fx.getSnapshot();
  const segments: string[] = [];
  let current: Entry | null = snapshot.getById(id);

  for (let depth = 0; depth < MAX_DEPTH; depth += 1) {
    if (current === null) return null;
    // The root entry's own name is the folder the engine was pointed at, and
    // `engine.root` already spells that out in full.
    if (current.parentId === null) {
      const root = engine.root.endsWith("/") ? engine.root.slice(0, -1) : engine.root;
      return [root, ...segments.reverse()].join("/");
    }
    segments.push(current.name);
    current = snapshot.getById(current.parentId);
  }
  return null;
}

/** Can this entry be opened as a file at all? */
export function isOpenable(entry: Entry): boolean {
  return entry.kind !== DIRECTORY;
}

/** What the viewer needs to know about an entry the reader just opened. */
export function handleFor(engine: WorktreeEngine, entry: Entry): FileHandle {
  return {
    // Scoped to the root, so the same id in a re-forked engine on a different
    // project can never be mistaken for this one.
    key: `${engine.root}#${entry.id}`,
    name: entry.name,
    path: pathOf(engine, entry.id),
    size: entry.size,
    ref: entry.id,
  };
}

/**
 * A `FileSource` backed by one engine.
 *
 * Memoize this per engine: `useFileContent` treats a new source as a new
 * request, so a fresh object every render would re-read the open file every
 * render.
 */
export function createWorktreeFileSource(engine: WorktreeEngine): FileSource {
  return {
    async read(handle: FileHandle): Promise<Uint8Array> {
      const id = handle.ref;
      if (typeof id !== "number") throw new Error("that file is no longer in the tree");
      // The port has no cancel for a read in flight; the caller drops the
      // answer instead, which for a local file is the same thing a few
      // milliseconds later.
      return engine.fx.readFile(id);
    },
  };
}
