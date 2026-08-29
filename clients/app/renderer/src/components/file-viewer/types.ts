// The file viewer's contract, and the whole reason it is a peer of the
// worktree column rather than a part of it: nothing here knows what a mille
// engine is, what a project is, or where the bytes came from. Hand it a name,
// a size and something that can produce bytes, and it shows the file.
//
// That seam is what lets the same dialog serve a search result, a session's
// footprint or a dropped path later without being rewritten.

import type { ComponentType } from "react";
import type { Language } from "./syntax";

/** A file the viewer has been asked to show. */
export interface FileHandle {
  /** Stable identity for this open — remounts the body and cancels the last
   * read when it changes. */
  readonly key: string;
  readonly name: string;
  /** Absolute path, when the caller can derive one. `null` costs the reader
   * the path line and the acts that need a path; it never costs them the
   * file, because reading goes through the source, not the path. */
  readonly path: string | null;
  /** Bytes on disk, known from the tree BEFORE any read — which is how the
   * viewer can refuse a file that is too large without opening it. */
  readonly size: number;
  /**
   * Whatever the source needs to find the file again — an entry id, a
   * handle, a URL. OPAQUE here on purpose: the only code allowed to read it
   * is the source that wrote it, which is what keeps the viewer free of any
   * knowledge of where files come from.
   */
  readonly ref?: unknown;
}

/** Where bytes come from. One method, so a caller can be a mille engine, a
 * fixture, or a test. */
export interface FileSource {
  read(handle: FileHandle, signal: AbortSignal): Promise<Uint8Array>;
}

/** A view's own switch, rendered as the room's one switch shape. */
export interface ViewMode {
  readonly id: string;
  readonly label: string;
}

/**
 * An act the dialog can offer for a file. Named rather than supplied as a
 * closure: a view is a data table, and the side effects live in the one place
 * that is allowed to have them.
 */
export type ViewerActionId = "copy-path" | "open-in-browser";

export interface ViewerDefinition {
  readonly id: string;
  /** How the bytes must arrive. `"text"` also gets `OpenFile.text` decoded. */
  readonly payload: "text" | "bytes";
  /** The most this view will read. Over it, the dialog says so and reads
   * nothing — a limit the reader is told is a fact; a hang is not. */
  readonly maxBytes: number;
  readonly modes?: readonly ViewMode[];
  readonly actions?: readonly ViewerActionId[];
  readonly Body: ComponentType<ViewBodyProps>;
}

/** What the registry decided a file is. */
export interface FileKind {
  readonly viewer: ViewerDefinition;
  /** The chip beside the name: "TypeScript", "Markdown", "PNG". */
  readonly label: string;
  /** Set when the body highlights source. */
  readonly language: Language | null;
  /** Set when the body needs a blob: `image/png`, `image/svg+xml`. */
  readonly mime: string | null;
}

/** A file that has been read. */
export interface OpenFile {
  readonly handle: FileHandle;
  readonly kind: FileKind;
  readonly bytes: Uint8Array;
  /** Decoded UTF-8, or `null` for a view whose payload is `"bytes"`. */
  readonly text: string | null;
}

export interface ViewBodyProps {
  readonly file: OpenFile;
  /** The current mode's id, or `""` for a view with no modes. */
  readonly mode: string;
}
