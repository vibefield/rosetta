// Reading one file, and every honest thing that can happen instead.
//
// Two of the five states are answered WITHOUT reading anything, from the size
// the tree already knows: an empty file, and a file past the view's limit.
// That ordering is the point — a viewer that discovers a 400 MB file by
// loading it has already done the damage by the time it can report it.

import { useEffect, useState } from "react";
import { BINARY_KIND, diffKind, looksBinary, resolveKind } from "./registry";
import type { FileHandle, FileSource, OpenFile } from "./types";

export type FileContent =
  | { readonly status: "reading" }
  | { readonly status: "empty" }
  | { readonly status: "too-large"; readonly limit: number }
  | { readonly status: "failed"; readonly reason: string }
  | { readonly status: "ready"; readonly file: OpenFile };

const READING: FileContent = { status: "reading" };

interface FileContentState {
  readonly handleKey: string | null;
  readonly source: FileSource | null;
  readonly asDiff: boolean;
  readonly value: FileContent;
}

/** What a failed read can be told about itself. mille raises a
 * `FileSystemError` carrying a POSIX code, which is the most useful half. */
function describe(error: unknown): string {
  if (error === null || typeof error !== "object") return "the read failed";
  const { code, message } = error as { code?: unknown; message?: unknown };
  if (typeof code === "string" && code !== "") {
    return typeof message === "string" && message !== "" ? `${code} · ${message}` : code;
  }
  return typeof message === "string" && message !== "" ? message : "the read failed";
}

function decode(bytes: Uint8Array): string {
  const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  // A byte-order mark is an encoding artefact, not the file's first
  // character, and left in place it shows up as a blank glyph on line one.
  return text.charCodeAt(0) === 0xfeff ? text.slice(1) : text;
}

/**
 * Read `handle` through `source`.
 *
 * `source` must be stable across renders — it identifies the engine the read
 * goes to, and a fresh object every render would re-read the file on every
 * render. The worktree column memoizes one per engine.
 */
export function useFileContent(
  handle: FileHandle | null,
  source: FileSource,
  /** Set when the caller is opening a CHANGE rather than a file — the one
   * decision the registry cannot make from a name. */
  asDiff = false,
): FileContent {
  const [state, setState] = useState<FileContentState>({
    handleKey: null,
    source: null,
    asDiff: false,
    value: READING,
  });

  useEffect(() => {
    if (handle === null) return;

    const publish = (value: FileContent) => {
      setState({ handleKey: handle.key, source, asDiff, value });
    };

    const kind = asDiff ? diffKind(handle.name) : resolveKind(handle.name);

    if (kind.viewer.payload === "compare") {
      if (source.compare === undefined) {
        publish({ status: "failed", reason: "this source has nothing to compare against" });
        return;
      }
      publish(READING);
      const comparing = new AbortController();
      void source
        .compare(handle, comparing.signal)
        .then(({ unified }) => {
          if (comparing.signal.aborted) return;
          publish({
            status: "ready",
            file: { handle, kind, bytes: new Uint8Array(0), text: null, unified },
          });
        })
        .catch((error) => {
          if (comparing.signal.aborted) return;
          publish({ status: "failed", reason: describe(error) });
        });
      return () => comparing.abort();
    }

    if (handle.size === 0) {
      publish({ status: "empty" });
      return;
    }
    if (handle.size > kind.viewer.maxBytes) {
      publish({ status: "too-large", limit: kind.viewer.maxBytes });
      return;
    }

    publish(READING);
    const controller = new AbortController();

    void (async () => {
      try {
        const bytes = await source.read(handle, controller.signal);
        if (controller.signal.aborted) return;

        // The name said text and the bytes disagree. The bytes win: this is
        // the one place a file's kind is decided by what is in it, and it can
        // only ever downgrade — a hex dump claims less, never more.
        const binary = kind.viewer.payload === "text" && looksBinary(bytes);
        const resolved = binary ? BINARY_KIND : kind;

        publish({
          status: "ready",
          file: {
            handle,
            kind: resolved,
            bytes,
            text: resolved.viewer.payload === "text" ? decode(bytes) : null,
            unified: null,
          },
        });
      } catch (error) {
        if (controller.signal.aborted) return;
        publish({ status: "failed", reason: describe(error) });
      }
    })();

    return () => controller.abort();
  }, [handle, source, asDiff]);

  // Effects change after render. Without an identity check, the first frame
  // after switching files carries the previous file's state — harmlessly
  // visible before, but unacceptable once an action can export those bytes.
  return state.handleKey === (handle?.key ?? null) &&
    state.source === source &&
    state.asDiff === asDiff
    ? state.value
    : READING;
}
