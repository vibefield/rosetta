// H-3, in one place: the worktree column is a GUEST, and the measure is not.
// Below a frame the room cannot seat everyone in, the guest closes itself
// rather than squeeze the transcript — reading is the job, and when something
// has to give it is not the thing the room exists for.
//
// The column's whole lifecycle lives here because the two halves are one
// decision: what the room shows, and what the engine is therefore asked to
// walk. Keeping them together is what stops the shell from forking a walker
// for a column nobody can see.

import { useEffect, useRef, useState } from "react";
import { desktopHost } from "../../lib/desktop-host";

/** Below this the guest closes itself (design record H-3). 260 + 300 + 280
 * of columns leaves the transcript its full measure down to ~1100px once the
 * guest is out of the way, so the guest is what pays. */
const GUEST_MIN_FRAME_WIDTH = 1360;

export interface WorktreeGuest {
  /** Whether a worktree can be shown at all: a browser tab has no engine to
   * walk one, so the toggle has nothing to offer. */
  readonly available: boolean;
  /** Whether the column is currently open. */
  readonly visible: boolean;
  /** Open a closed column, close an open one. */
  toggle(): void;
}

function useFrameWiderThan(minimumWidth: number): boolean {
  const [wide, setWide] = useState(() => window.innerWidth >= minimumWidth);
  useEffect(() => {
    const measure = () => setWide(window.innerWidth >= minimumWidth);
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [minimumWidth]);
  return wide;
}

/**
 * Whether column ③ is open, and the act of flipping it — plus the standing
 * instruction to the shell to keep an engine on `projectRoot` while it is.
 */
export function useWorktreeGuest(projectRoot: string | null): WorktreeGuest {
  const available = desktopHost() !== null;
  const [wanted, setWanted] = useState(true);
  // An explicit choice outranks the automatic close: once the reader has
  // pressed the toggle, the width heuristic stops speaking for them.
  const [chosen, setChosen] = useState(false);
  const frameWide = useFrameWiderThan(GUEST_MIN_FRAME_WIDTH);

  // A wide room keeps the guest's empty state visible before a project is
  // chosen. A missing root suppresses the engine, not the column explaining it.
  const visible = available && wanted && (chosen || frameWide);

  const toggle = () => {
    // Flips what is SHOWN, not what was last wanted — the first press on an
    // already-auto-closed column must never do nothing.
    if (visible) {
      setWanted(false);
      return;
    }
    setWanted(true);
    setChosen(true);
  };

  // Point the engine at the selected project's root. Killing and re-forking
  // the walker is the shell's concern, so this only speaks when the root
  // actually changes — a re-render is not news.
  const openedRoot = useRef<string | null>(null);
  useEffect(() => {
    if (!visible || projectRoot === null) return;
    if (openedRoot.current === projectRoot) return;
    openedRoot.current = projectRoot;
    void desktopHost()?.openWorktree(projectRoot);
  }, [visible, projectRoot]);

  return { available, visible, toggle };
}
