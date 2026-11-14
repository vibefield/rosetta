// The transcript's clock (§07). A pill's words are made when
// the document is drawn, by the viewer's own calendar days — so a document left
// open over midnight is drawn again at midnight, and a pill read today and
// looked at tomorrow says tomorrow's words.
//
// The clock is re-read at the viewer's next midnight (one timer, re-armed each
// read) and whenever the window comes back (`focus`, `visibilitychange`), in
// case the timer slept with the machine. The value changes only when the DAY
// does, so a document keyed by it is re-derived once a day, never per tick.
import { useEffect, useState } from "react";
import { calendarDayKey, msToNextViewerDay } from "../../lib/transcript";

export interface ViewerDay {
  /** The clock as read when the viewer's current day began for this document. */
  readonly now: number;
  /** `YYYY-MM-DD`, viewer-local: what a memo over the document keys on. */
  readonly dayKey: string;
}

/** A timer this far past midnight reads the new day, not the old one's last instant. */
const PAST_MIDNIGHT_MS = 1_000;

function readViewerDay(): ViewerDay {
  const now = Date.now();
  return { now, dayKey: calendarDayKey(now) };
}

export function useViewerDay(): ViewerDay {
  const [day, setDay] = useState(readViewerDay);
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const refresh = (): void => {
      const read = readViewerDay();
      setDay((current) => (current.dayKey === read.dayKey ? current : read));
      if (timer !== undefined) clearTimeout(timer);
      timer = setTimeout(refresh, msToNextViewerDay(read.now) + PAST_MIDNIGHT_MS);
    };
    const onVisibility = (): void => {
      if (document.visibilityState === "visible") refresh();
    };
    refresh();
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      if (timer !== undefined) clearTimeout(timer);
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, []);
  return day;
}
