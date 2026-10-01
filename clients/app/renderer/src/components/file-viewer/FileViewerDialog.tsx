// The popup. A modal, on purpose: reading a file is a DETOUR from reading
// history, and a surface you must dismiss says that, where a fifth column
// would quietly become a second reading surface competing with the measure.
// It also keeps H-2 — the room's columns change content, never geometry, and
// a layer above them changes neither.
//
// The material is the one the design system already provisioned and nothing
// had yet spent: `--vf-shadow-dialog`, `--vf-backdrop-dim`, and the stage
// receding to 0.98 behind it (DESIGN.md §5 and motion law M5). No new tokens.
//
// Radix owns the mechanics a modal must not get wrong — focus trap, focus
// restore, Escape, scroll lock, `aria-modal`, the portal. It was already a
// dependency of this app and had no user; this is its user.

import { useEffect, useState } from "react";
import type { KeyboardEventHandler, ReactNode } from "react";
import { DialogShell } from "../ui";
import { diffKind, imageKind, resolveKind } from "./registry";
import { useFileContent } from "./use-file-content";
import { FileViewerHeader } from "./FileViewerHeader";
import type { HeaderAction } from "./FileViewerHeader";
import type { FileHandle, FileSource, ViewerActionId, ViewBodyProps } from "./types";
import { fmtBytes } from "../../lib/format";

export interface FileViewerDialogProps {
  readonly renderMarkdown?: ViewBodyProps["renderMarkdown"];
  /** The file being read, or `null` for a closed dialog. */
  readonly handle: FileHandle | null;
  /** Where bytes come from. Must be stable across renders. */
  readonly source: FileSource;
  /** The project's §2.6 accent, so the file wears the colour of the thing it
   * belongs to — the same marker its project and session already carry. */
  readonly tint: string | null;
  readonly homeDirectory: string | null;
  readonly onClose: () => void;
  /** The room's receipt line, so an act is never silent. */
  readonly onReceipt?: (text: string) => void;
  /** Hand the currently displayed bytes to the OS browser. Absent in a
   * browser tab, and absent is why the act is a capability rather than an
   * assumption. */
  readonly openInBrowser: ((name: string, bytes: Uint8Array) => Promise<boolean>) | null;
  /** Open this handle as a CHANGE rather than as a file. The registry reads
   * kinds off a name, and a name cannot say which of the two acts is being
   * served. */
  readonly asDiff?: boolean;
  readonly gallery?: {
    readonly key: string;
    readonly footer: ReactNode;
    readonly previous: (() => void) | null;
    readonly next: (() => void) | null;
    readonly onKeyDown: KeyboardEventHandler<HTMLDivElement>;
    readonly onRetry: () => void;
  };
}

export function FileViewerDialog({
  handle,
  source,
  tint,
  homeDirectory,
  onClose,
  onReceipt,
  openInBrowser,
  asDiff = false,
  renderMarkdown,
  gallery,
}: FileViewerDialogProps) {
  const [attempt, setAttempt] = useState(0);
  const content = useFileContent(handle, source, asDiff, attempt, Boolean(gallery));
  const retry = () => { gallery?.onRetry(); setAttempt(value => value + 1); };

  // The reader's choice of mode, kept only while it names a mode the current
  // view actually has — so opening a `.md` after an image does not land on a
  // switch position that no longer exists.
  const [chosen, setChosen] = useState<string | null>(null);
  const [fullscreen, setFullscreen] = useState(false);
  const fileKey = gallery?.key ?? handle?.key ?? null;
  useEffect(() => {
    setChosen(null);
    setFullscreen(false);
  }, [fileKey]);

  if (handle === null) return null;

  // Before the bytes land, the name is the only evidence there is — and it is
  // what the reader clicked, so the band can be right about it immediately.
  // A read that disagrees (a `.png` full of text, a `.json` full of NULs)
  // corrects the chip when it resolves.
  const kind =
    content.status === "ready"
      ? content.file.kind
      : asDiff
        ? diffKind(handle.name)
        : gallery ? imageKind(handle.name) : resolveKind(handle.name);
  const modes = kind.viewer.modes ?? [];
  const mode = chosen !== null && modes.some((m) => m.id === chosen) ? chosen : (modes[0]?.id ?? "");

  const perform = (action: ViewerActionId): HeaderAction | null => {
    if (action === "copy-path") {
      const path = handle.path;
      if (path === null) return null;
      return {
        id: action,
        label: "Copy path",
        glyph: "copy",
        run: () => {
          void navigator.clipboard
            .writeText(path)
            .then(() => onReceipt?.("path copied"))
            .catch(() => onReceipt?.("the clipboard refused"));
        },
      };
    }
    const open = openInBrowser ?? null;
    if (open === null) return null;
    const bytes =
      content.status === "ready" && content.file.handle.key === handle.key
        ? content.file.bytes
        : content.status === "empty"
          ? new Uint8Array(0)
          : null;
    return {
      id: action,
      label: "Open in browser",
      glyph: "external",
      disabled: bytes === null,
      run: () => {
        if (bytes === null) return;
        void open(handle.name, bytes)
          .then((opened) => {
            // The shell decides, not this room: it re-checks the extension
            // and byte limit before handing its snapshot to the OS. A
            // refusal is a fact the reader is told, not a silent no-op.
            onReceipt?.(opened ? "opened in your browser" : "the shell would not open that");
          })
          .catch(() => onReceipt?.("the shell would not open that"));
      },
    };
  };

  const actions = (kind.viewer.actions ?? [])
    .map(perform)
    .filter((action): action is HeaderAction => action !== null);

  const Body = kind.viewer.Body;

  return (
    <DialogShell
      title={`${handle.name}${handle.path !== null ? ` — ${handle.path}` : ""}`}
      onClose={onClose}
      fullscreen={fullscreen}
      onExitFullscreen={() => setFullscreen(false)}
      onKeyDown={gallery?.onKeyDown}
    >
          <FileViewerHeader
            handle={handle.size < 0 && content.status === "ready" && !content.file.image ? { ...handle, size: content.file.bytes.byteLength } : handle}
            kind={kind}
            tint={tint}
            homeDirectory={homeDirectory}
            modes={modes}
            mode={mode}
            onModeChange={setChosen}
            actions={actions}
            fullscreen={fullscreen}
            onToggleFullscreen={() => setFullscreen((current) => !current)}
            onClose={onClose}
          />

          <div className="ah-fileview__body" data-gallery={gallery?.footer ? "" : undefined}>
            {content.status === "ready" ? (
              <Body key={`${handle.key}:${attempt}`} file={content.file} mode={mode} renderMarkdown={renderMarkdown} onRetry={gallery ? retry : undefined} />
            ) : content.status === "reading" ? (
              // Styled to appear only if the read is slow enough to be worth
              // mentioning; a local file that lands in three milliseconds
              // never flashes this.
              <div className="ah-fileview__state" data-waiting>
                <b>{gallery ? "Loading image…" : "Reading"}</b>
                {!gallery && <span>{fmtBytes(handle.size)} from the worktree engine.</span>}
              </div>
            ) : content.status === "empty" ? (
              <div className="ah-fileview__state">
                <b>This file is empty</b>
                <span>Zero bytes on disk. There is nothing to show, and nothing is wrong.</span>
              </div>
            ) : content.status === "too-large" ? (
              <div className="ah-fileview__state">
                <b>Too large to open here</b>
                <span>
                  {handle.size < 0 ? "The image exceeds" : `${fmtBytes(handle.size)}, against`} a {fmtBytes(content.limit)} limit for a{" "}
                  {kind.label} file. The limit is here so the window stays answerable.
                </span>
              </div>
            ) : (
              <div className="ah-fileview__state">
                <b>Could not read this file</b>
                <span>{content.reason}</span>
                {gallery && <button className="ah-fileview__retry" type="button" onClick={retry}>Retry</button>}
              </div>
            )}
            {gallery?.footer && <>
              <button type="button" className="ah-image-gallery__step ah-image-gallery__step--previous"
                aria-label="Previous image" title="Previous image (←)" aria-disabled={gallery.previous === null}
                onClick={() => gallery.previous?.()}>‹</button>
              <button type="button" className="ah-image-gallery__step ah-image-gallery__step--next"
                aria-label="Next image" title="Next image (→)" aria-disabled={gallery.next === null}
                onClick={() => gallery.next?.()}>›</button>
            </>}
          </div>
          {gallery?.footer}
    </DialogShell>
  );
}
