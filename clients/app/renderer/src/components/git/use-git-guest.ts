// H-3, with two subjects.
//
// "The rail is a guest; the measure is not" — and once there are two guests,
// the record adds one rule: when both want a room that seats one, **the newer
// takes the seat and the room says which one left**. A guest that vanished
// silently would read as a broken button, which is the bug H-3 caused once
// already.

import { useEffect, useRef, useState } from "react";
import { desktopHost } from "../../lib/desktop-host";

/** Below this, one guest. 260 + 300 + 280 + 280 of columns plus a 520px
 * measure is what the design record costs out at 1640. */
const BOTH_GUESTS_MIN_WIDTH = 1640;
/** Below this, no guest at all — the measure is what the room is for. */
const ONE_GUEST_MIN_WIDTH = 1360;

export type GuestName = "worktree" | "git";

export interface Guests {
  readonly available: boolean;
  readonly worktreeVisible: boolean;
  readonly gitVisible: boolean;
  toggle(guest: GuestName): void;
}

function useFrameWidth(): number {
  const [width, setWidth] = useState(() => window.innerWidth);
  useEffect(() => {
    const measure = () => setWidth(window.innerWidth);
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, []);
  return width;
}

/**
 * Which guests are open, and the act of flipping one.
 *
 * `onEvicted` is called with the guest that lost its seat, so the room can
 * say so — the receipt is not optional decoration, it is what stops an
 * auto-close from reading as a bug.
 */
export function useGuests(
  projectRoot: string | null,
  onEvicted: (guest: GuestName) => void,
): Guests {
  const available = desktopHost() !== null && projectRoot !== null;
  const width = useFrameWidth();
  // Newest first: the head of this list is the guest that most recently took
  // a seat, which is what makes "the newer takes the seat" a fact about
  // order rather than a special case.
  const [wanted, setWanted] = useState<readonly GuestName[]>(["worktree"]);

  const seats = width >= BOTH_GUESTS_MIN_WIDTH ? 2 : width >= ONE_GUEST_MIN_WIDTH ? 1 : 0;
  const seated = available ? wanted.slice(0, seats) : [];

  // The room says which guest the frame closed. Announced from an effect
  // rather than during render, because a receipt is a side effect and a
  // resize can fire many of them.
  const [announced, setAnnounced] = useState<GuestName | null>(null);
  const evicted = available && wanted.length > seats ? (wanted[seats] ?? null) : null;
  useEffect(() => {
    if (evicted === announced) return;
    setAnnounced(evicted);
    if (evicted !== null) onEvicted(evicted);
    // `onEvicted` is the room's receipt function and is stable enough; what
    // must not re-fire is the announcement itself, which `announced` guards.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [evicted, announced]);

  const toggle = (guest: GuestName) => {
    setWanted((current) =>
      current.includes(guest) && current.slice(0, seats).includes(guest)
        ? current.filter((name) => name !== guest)
        : // Taking a seat puts this guest at the head, which is what evicts
          // the other one when the room seats fewer than both want.
          [guest, ...current.filter((name) => name !== guest)],
    );
  };

  // Point the shell at the selected project's root while ANY guest is open.
  // One UtilityProcess serves both — the tree over mille's port and git over
  // its own — so the fork is a fact about the room having a guest at all,
  // not about which one. It speaks only when the root actually changes; a
  // re-render is not news, and a re-fork would drop both ports.
  const openedRoot = useRef<string | null>(null);
  const anyGuest = seated.length > 0;
  useEffect(() => {
    if (!anyGuest || projectRoot === null) return;
    if (openedRoot.current === projectRoot) return;
    openedRoot.current = projectRoot;
    void desktopHost()?.openWorktree(projectRoot);
  }, [anyGuest, projectRoot]);

  return {
    available,
    worktreeVisible: seated.includes("worktree"),
    gitVisible: seated.includes("git"),
    toggle,
  };
}
