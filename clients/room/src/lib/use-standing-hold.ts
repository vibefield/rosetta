// How long a hold stands (R10): it is late (nothing for 120ms, so a wait
// answered at once shows no hold at all), and a hold that has begun to show
// stands at least 800ms — its mark's 400 in, then a beat to read the line —
// before it fades away over 280ms (120ms under reduced motion, as hold.css
// fades it). What comes next waits for it, so the room's next face never draws
// under a hold that is still there. A carried hold (the host's own stood where
// it stands) is shown from its first frame.
import { useEffect, useRef, useState } from "react";
import { REDUCED_MOTION_QUERY } from "./motion";

export const HOLD_LATE_MS = 120;
export const HOLD_FLOOR_MS = 800;
export const HOLD_LEAVE_MS = 280;
export const HOLD_LEAVE_REDUCED_MS = 120;

/** The leave as hold.css draws it: one short fade under reduced motion. */
function leaveMs(): number {
  const reduced = typeof window !== "undefined" && window.matchMedia?.(REDUCED_MOTION_QUERY).matches === true;
  return reduced ? HOLD_LEAVE_REDUCED_MS : HOLD_LEAVE_MS;
}

type Phase = "hold" | "floor" | "leave" | "gone";

interface Standing {
  readonly phase: Phase;
  /** When the hold began to show (its late passed), epoch ms. */
  readonly shownAt: number;
}

/** What a hold does when its wait is over at `now`: a hold that never began
 * to show is gone at once; one that has stands out its floor; past its floor
 * it leaves. */
export function afterWait(now: number, shownAt: number): "gone" | "floor" | "leave" {
  if (now < shownAt) return "gone";
  return now < shownAt + HOLD_FLOOR_MS ? "floor" : "leave";
}

/** The hold that stands now — the one asked for, or the last one while its
 * floor and its leave run — whether it is leaving, and whether it is carried
 * (only the room's first hold can be: one that comes back after it went is
 * late again); null when none stands. */
export function useStandingHold<T>(hold: T | null, carried: boolean): {
  readonly hold: T;
  readonly leaving: boolean;
  readonly carried: boolean;
} | null {
  const last = useRef<T | null>(hold);
  if (hold !== null) last.current = hold;
  const shownOnce = useRef(false);
  const wentAway = useRef(false);
  const asked = hold !== null;
  const [standing, setStanding] = useState<Standing>(() => ({
    phase: asked ? "hold" : "gone",
    shownAt: Date.now() + (carried ? 0 : HOLD_LATE_MS),
  }));
  if (asked && standing.phase !== "hold") {
    // a wait (again): a hold that was gone is late again; one still standing keeps its clock
    setStanding({
      phase: "hold",
      shownAt: standing.phase === "gone" ? Date.now() + HOLD_LATE_MS : standing.shownAt,
    });
  } else if (!asked && standing.phase === "hold") {
    setStanding({ phase: afterWait(Date.now(), standing.shownAt), shownAt: standing.shownAt });
  }
  useEffect(() => {
    if (standing.phase === "floor") {
      const timer = setTimeout(
        () => setStanding((now) => (now.phase === "floor" ? { ...now, phase: "leave" } : now)),
        Math.max(0, standing.shownAt + HOLD_FLOOR_MS - Date.now()),
      );
      return () => clearTimeout(timer);
    }
    if (standing.phase === "leave") {
      const timer = setTimeout(
        () => setStanding((now) => (now.phase === "leave" ? { ...now, phase: "gone" } : now)),
        leaveMs(),
      );
      return () => clearTimeout(timer);
    }
    return undefined;
  }, [standing.phase, standing.shownAt]);
  if (standing.phase === "gone" || last.current === null) {
    if (shownOnce.current) wentAway.current = true;
    return null;
  }
  shownOnce.current = true;
  return { hold: last.current, leaving: standing.phase === "leave", carried: carried && !wentAway.current };
}
