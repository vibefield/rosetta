// The file viewer's contract, and the whole reason it is a peer of the
// worktree column rather than a part of it: nothing here knows what a mille
// engine is, what a project is, or where the bytes came from. Hand it a name,
// a size and something that can produce bytes, and it shows the file.
//
// That seam is what lets the same dialog serve a search result, a session's
// footprint or a dropped path later without being rewritten.

import type { ComponentType, ReactNode } from "react";
import type { Language } from "./syntax";

/** A file the viewer has been asked to show. */
export interface FileHandle {
  /** Stable identity for this open — remounts the body and cancels the last
   * read when it changes. */
  readonly key: string;
  readonly name: string;
  /** Absolute path, when the caller can derive one. `null` costs the reader
   * the path line and path-only acts; it never costs them the
   * file, because reading goes through the source, not the path. */
  readonly path: string | null;
  /** Known byte length; a negative value means the source has not supplied
   * a size yet. Reads also check their actual length before rendering. */
  readonly size: number;
  /**
   * Whatever the source needs to find the file again — an entry id, a
   * handle, a URL. OPAQUE here on purpose: the only code allowed to read it
   * is the source that wrote it, which is what keeps the viewer free of any
   * knowledge of where files come from.
   */
  readonly ref?: unknown;
}

/** Where bytes come from. A caller can be a mille engine, a fixture, or a
 * test. */
export interface FileSource {
  read(handle: FileHandle, signal: AbortSignal): Promise<Uint8Array>;
  /** Sized, browser-cacheable resources. The source owns address validation;
   * a viewer never needs to copy full-image bytes to show a small thumbnail. */
  image?: ((handle: FileHandle) => ImageResource | null) | undefined;
  /**
   * A unified diff of this handle, for a view whose payload is `"compare"`.
   *
   * Optional, because most sources have one version of a file and nothing to
   * compare it to. A source that omits it must never resolve a kind that
   * asks for one — the registry decides that from the name, and a diff is
   * only ever asked for by a caller that built the handle.
   */
  compare?(handle: FileHandle, signal: AbortSignal): Promise<{ readonly unified: string }>;
}

/** The sized renditions a source may address. */
export type ImageRendition = "full" | "preview" | "rail" | "tile" | "tile2x";

/** An image address: now, once the source has it, or not at all. */
export type ImageUrl = string | null | PromiseLike<string | null>;

/** Sized, browser-loadable renditions of one image, asked for one at a time:
 * a source that must fetch before it can address an image fetches only the
 * rendition a view shows. */
export interface ImageResource {
  /** Stable for this image: the same key asks for the same bytes. */
  readonly key: string;
  url(rendition: ImageRendition): ImageUrl;
  readonly width?: number | undefined;
  readonly height?: number | undefined;
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
  /** How the content must arrive. `"text"` also gets `OpenFile.text`
   * decoded; `"compare"` asks the source for a unified diff instead of
   * bytes, and the file's own size limit does not apply — git bounds its
   * own output, and a one-line change to a huge file is a small diff. */
  readonly payload: "text" | "bytes" | "compare";
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
  /** The unified diff, for a `"compare"` view. */
  readonly unified: string | null;
  readonly image?: ImageResource;
  readonly imageAttempt?: number;
}

export interface ViewBodyProps {
  readonly onRetry?: (() => void) | undefined;
  /** Optional rich Markdown presentation supplied by the content owner. */
  readonly renderMarkdown?: ((text: string) => ReactNode) | undefined;
  readonly file: OpenFile;
  /** The current mode's id, or `""` for a view with no modes. */
  readonly mode: string;
}
