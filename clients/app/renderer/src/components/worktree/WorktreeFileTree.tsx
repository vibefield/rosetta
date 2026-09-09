// Mille's shipped tree, with Rosetta's read-only commands: inspect a file,
// copy its path, or reveal it in the desktop file manager.

import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent, RefObject } from "react";
import type { Entry, FileExplorer } from "@vibecook/mille";
import { FileTree, FileTreeProvider } from "@vibecook/mille-ui";
import { vibefieldIconTheme } from "@vibecook/mille-ui/icons/vibefield";
import { desktopHost } from "../../lib/desktop-host";
import { createWorktreeFileCommands } from "./worktree-file-commands";
import type { WorktreeEngine } from "./types";

export interface WorktreeFileTreeProps {
  readonly engine: WorktreeEngine;
  /** The engine is still walking a root the room has moved on from, so the
   * tree is showing something true but stale. */
  readonly stale: boolean;
  /** A file the reader asked to see. Double-click or Enter, the way every
   * file tree has worked for thirty years — a single click still just
   * selects, because a row that throws a modal on every click is a row you
   * learn not to touch. */
  readonly onOpen: (entry: Entry) => void;
  /** The file the viewer currently has open, so the row wears the active
   * fill. The design record left this binding open (§10 delta 4) because a
   * reader with no open FILE had nothing to bind it to; now there is one. */
  readonly openEntryId: number | null;
  readonly onReceipt: (message: string) => void;
}

/** Mille's create/rename shortcuts also have direct engine fallbacks. Keep
 * those out of this read-only tree, while letting its own copy binding and
 * navigation run normally. Portaled menus and text inputs own their keys. */
function guardTreeKeys(event: KeyboardEvent<HTMLDivElement>): void {
  const target = event.target;
  if (!(target instanceof HTMLElement) || !target.closest('[role="tree"]')) return;
  if (target.closest('input, textarea, [contenteditable]:not([contenteditable="false"])')) {
    event.stopPropagation();
    return;
  }
  const key = event.key.toLowerCase();
  if (
    key === "f2" || key === "delete" || key === "backspace" ||
    ((event.ctrlKey || event.metaKey) && !event.altKey && ["x", "v", "n"].includes(key))
  ) {
    event.preventDefault();
    event.stopPropagation();
  }
}

/**
 * The tree's own height, so whatever follows it sits against the last row
 * rather than at the bottom of the column — the design record draws the rail
 * as one flow: tree, hairline, worktrees.
 *
 * A virtualized tree cannot size itself; it needs a definite height to have a
 * viewport at all. So measure what it is drawing and hand that back. `null`
 * means "not measurable", and the CSS then fills the column, which is also
 * what a tree taller than the rail wants — the wrapper's `flex-shrink` caps
 * the measured height there.
 */
function useContentHeight(resetKey: string): {
  ref: RefObject<HTMLDivElement | null>;
  height: number | null;
} {
  const ref = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState<number | null>(null);

  useEffect(() => {
    setHeight(null);
    const wrapper = ref.current;
    if (wrapper === null || typeof MutationObserver === "undefined") return;

    // The scroller's only child is the virtualizer's sizer, and its height is
    // the whole tree's — the rows inside it are a viewport-sized subset, so
    // counting them would measure the window, not the document.
    const measure = (): void => {
      const scroller = wrapper.querySelector<HTMLElement>('[role="tree"]');
      const sizer = scroller?.firstElementChild;
      if (!(scroller instanceof HTMLElement) || !(sizer instanceof HTMLElement)) {
        setHeight(null);
        return;
      }
      const style = getComputedStyle(scroller);
      const wrapperStyle = getComputedStyle(wrapper);
      const padding =
        (parseFloat(style.paddingTop) || 0) + (parseFloat(style.paddingBottom) || 0) +
        (parseFloat(wrapperStyle.paddingTop) || 0) + (parseFloat(wrapperStyle.paddingBottom) || 0);
      const next = sizer.offsetHeight + padding;
      // An unchanged height sets no state, which is what stops the loop this
      // would otherwise be: the height lands as an inline style, and that is
      // itself a mutation the observer sees.
      setHeight((current) => (current === next ? current : next));
    };

    measure();
    // Measured straight from the observer rather than off an animation frame.
    // The callback already arrives once per batch of records, so a frame buys
    // no further coalescing — and a window that is merely occluded gets no
    // frames at all, which left the tree stuck at whatever height it had when
    // it went to the back.
    const observer = new MutationObserver(measure);
    observer.observe(wrapper, { childList: true, subtree: true, attributeFilter: ["style"] });
    return () => observer.disconnect();
    // Re-measured from scratch per root: a project switch replaces the tree
    // wholesale, and carrying the old height across would size the new one to
    // the old one until its first mutation.
  }, [resetKey]);

  return { ref, height };
}

/** mille types `FileTreeProvider.fx` as the in-process engine, but the
 * embedding guide's own architecture puts the walker in a utility process and
 * the renderer on a port. The port client mirrors that surface — the one
 * place the room states it, rather than casting at every call site. */
function asTreeEngine(engine: WorktreeEngine): FileExplorer {
  return engine.fx as unknown as FileExplorer;
}

export function WorktreeFileTree({
  engine,
  stale,
  onOpen,
  openEntryId,
  onReceipt,
}: WorktreeFileTreeProps) {
  const environment = useRef({ host: desktopHost(), stale, onReceipt });
  environment.current = { host: desktopHost(), stale, onReceipt };
  const { commands, onSelectionChange, onFocusedIdChange } = useMemo(
    () => createWorktreeFileCommands(engine, () => environment.current),
    [engine],
  );
  const content = useContentHeight(`${engine.generation}:${engine.root}`);

  return (
    <div
      ref={content.ref}
      // `ah-rail*` is the design record's name for this column; room.css is
      // a port of the proto and keeps its vocabulary.
      className="ah-railtree"
      // No `data-mille-density="compact"`: the design record's rail is the
      // shipped 26px row (§8, "26px rows rounded inside an 8px gutter").
      // 22px is the density for a long editor rail.
      data-mille-theme="vibefield"
      data-stale={stale ? "" : undefined}
      onKeyDownCapture={guardTreeKeys}
      // macOS's native Edit > Copy can arrive as a copy event instead of a
      // keydown. Route it through the very same selection-aware command.
      onCopy={(event) => {
        const target = event.target;
        if (
          !(target instanceof HTMLElement) || !target.closest('[role="tree"]') ||
          target.closest('input, textarea, [contenteditable]:not([contenteditable="false"])')
        ) return;
        event.preventDefault();
        event.stopPropagation();
        void commands.dispatch("file.copyAbsolutePath");
      }}
      style={content.height !== null ? { height: content.height } : undefined}
    >
      <FileTreeProvider
        key={engine.generation}
        fx={asTreeEngine(engine)}
        commands={commands}
      >
        <FileTree
          ariaLabel="Project worktree"
          iconTheme={vibefieldIconTheme}
          disableDragDrop
          onSelectionChange={onSelectionChange}
          onFocusedIdChange={onFocusedIdChange}
          onOpen={(entry) => onOpen(entry)}
          // Marks the row without revealing it: `autoRevealActiveEntry`
          // defaults to false, and the reader who just clicked a row does not
          // need the tree to scroll it back under their cursor.
          activeEntry={openEntryId}
          style={{ height: "100%" }}
        />
      </FileTreeProvider>
    </div>
  );
}
