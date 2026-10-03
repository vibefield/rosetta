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
import { useRoom } from "../../room-context";

/** Below this room width the guest closes itself (design record H-3). 260 + 300 + 280
 * of columns leaves the transcript its full measure down to ~1100px once the
 * guest is out of the way, so the guest is what pays. */
const GUEST_MIN_FRAME_WIDTH = 1360;

export interface WorktreeGuest {
  /** Whether a worktree can be shown at all: a host without a worktree
   * engine (a browser tab) has nothing to walk one with, so the toggle has
   * nothing to offer. */
  readonly available: boolean;
  /** Whether the column is currently open. */
  readonly visible: boolean;
  /** A shell-level refusal before a process generation could start. */
  readonly error: string | null;
  /** Open a closed column, close an open one. */
  toggle(): void;
  /** Explicitly start a fresh generation, even for the same root. */
  retry(): void;
}

/**
 * Whether column ③ is open, and the act of flipping it — plus the standing
 * instruction to the host to keep an engine on `projectRoot` while it is.
 * `roomWidth` is the room's own measured width, not the window's: a host may
 * seat the room in less than its window. Null until measured.
 */
export function useWorktreeGuest(projectRoot: string | null, roomWidth: number | null): WorktreeGuest {
  const engine = useRoom().host?.worktree ?? null;
  const available = engine !== null;
  const [wanted, setWanted] = useState(true);
  // An explicit choice outranks the automatic close: once the reader has
  // pressed the toggle, the width heuristic stops speaking for them.
  const [chosen, setChosen] = useState(false);
  const [retryRevision, setRetryRevision] = useState(0);
  const [failure, setFailure] = useState<{ root: string; message: string } | null>(null);
  const openedRoot = useRef<string | null>(null);
  const requestId = useRef(0);
  const frameWide = roomWidth !== null && roomWidth >= GUEST_MIN_FRAME_WIDTH;

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

  const retry = () => {
    openedRoot.current = null;
    setFailure(null);
    setRetryRevision((revision) => revision + 1);
  };

  // Point the engine at the selected project's root. Killing and re-forking
  // the walker is the shell's concern, so this only speaks when the root
  // actually changes — a re-render is not news.
  useEffect(() => {
    if (!visible || projectRoot === null) return;
    if (openedRoot.current === projectRoot) return;
    if (engine === null) return;
    const id = (requestId.current += 1);
    openedRoot.current = projectRoot;
    setFailure(null);
    void engine
      .open(projectRoot)
      .then((result) => {
        if (requestId.current !== id) return;
        if (result.ok) return;
        openedRoot.current = null;
        setFailure({ root: projectRoot, message: result.error });
      })
      .catch((reason: unknown) => {
        if (requestId.current !== id) return;
        openedRoot.current = null;
        setFailure({
          root: projectRoot,
          message: reason instanceof Error ? reason.message : "Could not open the worktree.",
        });
      });
  }, [visible, projectRoot, retryRevision, engine]);

  return {
    available,
    visible,
    error: failure?.root === projectRoot ? failure.message : null,
    toggle,
    retry,
  };
}
