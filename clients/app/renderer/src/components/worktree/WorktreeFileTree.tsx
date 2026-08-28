// mille's shipped tree, mounted for real (design record §8) and read-only in
// every direction: an empty command registry, no context menu, no drag. This
// room reads history; it does not edit worktrees.

import { useEffect, useMemo, useRef, useState } from "react";
import type { RefObject } from "react";
import type { FileExplorer } from "@vibecook/mille";
import { FileTree, FileTreeProvider } from "@vibecook/mille-ui";
import { createCommandRegistry } from "@vibecook/mille-ui/commands";
import { vibefieldIconTheme } from "@vibecook/mille-ui/icons/vibefield";
import type { WorktreeEngine } from "./types";

export interface WorktreeFileTreeProps {
  readonly engine: WorktreeEngine;
  /** The engine is still walking a root the room has moved on from, so the
   * tree is showing something true but stale. */
  readonly stale: boolean;
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
      const padding =
        (parseFloat(style.paddingTop) || 0) + (parseFloat(style.paddingBottom) || 0);
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

export function WorktreeFileTree({ engine, stale }: WorktreeFileTreeProps) {
  // Read-only: the registry is empty, so no command can reach a file.
  const commands = useMemo(() => createCommandRegistry([]), []);
  const content = useContentHeight(engine.root);

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
      style={content.height !== null ? { height: content.height } : undefined}
    >
      <FileTreeProvider fx={asTreeEngine(engine)} commands={commands}>
        <FileTree
          ariaLabel="Project worktree"
          iconTheme={vibefieldIconTheme}
          disableContextMenu
          disableDragDrop
          style={{ height: "100%" }}
        />
      </FileTreeProvider>
    </div>
  );
}
