// §07 — time: days and sittings, in one pill.
//
// A day change draws the day; a pause of fifteen minutes or more draws the
// resumption time and the length of the silence; when both happen at once they
// share ONE pill — `Tue 1 Sep · 00:02 · after 44m`. Two marks for one moment
// would be the same fact told twice. A pill with a clock says how far back it
// is: the clock alone today, the weekday on one of the six days
// before, the day earlier — inside a day as where the day turned.
//
// The pill is the one marker that is an OBJECT, because it travels: it sits on
// its hairline in the flow and pins to the top edge while the reader scrolls
// through its day (Slack's rule), then goes back to its hairline the moment its
// day scrolls into view. It never fades — a pinned day is information, not a
// hint; only the rows under it dissolve, into the top fade.
//
// Every sitting is its own `section`, and that is load-bearing: a sticky
// element's containing block is its parent, so sections are what make the next
// pill push the previous one OUT instead of stacking on it.
import type { MouseEvent, ReactNode } from "react";
import { useEffect } from "react";
import { duration } from "../../lib/format";
import type { DayEntry, Sitting as SittingModel } from "../../lib/transcript";
import { useReaderMode } from "./reader-context";

export interface PillText {
  readonly lead: string;
  readonly after: string | null;
}

/**
 * A pill with a clock — the reader came back after a pause — leads with that
 * clock by how far back its day is (`20:21` today,
 * `Fri · 20:21` on one of the six days before, `Fri 6 Nov · 20:21` earlier),
 * then `after 3h 37m`; where the day turned and inside a day alike. A pill
 * with no clock — the day turned with no pause — keeps its day alone: `Today`,
 * `Yesterday`, `Mon 31 Aug`. Null when the sitting is neither — a session that
 * lives on one day with no pause draws no pill at all, because the column
 * head's range already says when it was.
 */
export function pillText(sitting: SittingModel): PillText | null {
  const after = sitting.pauseMs === null ? null : `after ${duration(0, sitting.pauseMs)}`;
  if (sitting.clockLabel !== null) return { lead: sitting.clockLabel, after };
  if (sitting.dayChanged && sitting.dayLabel !== null) return { lead: sitting.dayLabel, after };
  return null;
}

export interface DayPillProps {
  readonly sitting: SittingModel;
  readonly onPress: (event: MouseEvent<HTMLButtonElement>) => void;
}

export function DayPill({ sitting, onPress }: DayPillProps) {
  // In the reader the day is a label: it opens no menu.
  const reading = useReaderMode();
  const text = pillText(sitting);
  if (text === null) return null;
  return (
    <div
      className={sitting.isFirst ? "tr-day tr-day--first" : "tr-day"}
      data-stuck="false"
      data-day-key={sitting.dayKey ?? undefined}
    >
      {reading ? (
        <span className="tr-day__pill">
          {text.lead}
          {text.after !== null && <span className="after">{text.after}</span>}
        </span>
      ) : (
        <button
          type="button"
          className="tr-day__pill"
          title="Jump to a day"
          aria-haspopup="menu"
          onClick={onPress}
        >
          {text.lead}
          {text.after !== null && <span className="after">{text.after}</span>}
        </button>
      )}
    </div>
  );
}

export function Sitting({
  sitting,
  children,
}: {
  readonly sitting: SittingModel;
  readonly children: ReactNode;
}) {
  return (
    <section className="tr-sitting" data-sitting-key={sitting.key}>
      {children}
    </section>
  );
}

export interface DayMenuProps {
  readonly days: readonly DayEntry[];
  readonly currentDayKey: string | null;
  /** Pixels from the frame's top — set from the pressed pill's own rect. */
  readonly top: number;
  readonly onJump: (day: DayEntry) => void;
  readonly onDismiss: () => void;
}

export function DayMenu({ days, currentDayKey, top, onJump, onDismiss }: DayMenuProps) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onDismiss();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onDismiss]);
  return (
    <div className="tr-daymenu" role="menu" style={{ top }}>
      {days.map((day) => (
        <button
          type="button"
          role="menuitem"
          key={day.dayKey}
          aria-current={day.dayKey === currentDayKey ? "true" : undefined}
          onClick={() => onJump(day)}
        >
          {day.dayLabel}
          <span>
            {day.sittingCount} {day.sittingCount === 1 ? "sitting" : "sittings"}
          </span>
        </button>
      ))}
    </div>
  );
}

/**
 * Stamp `data-stuck` on whichever pill is currently pinned to the top edge.
 * The record does this from the scroll handler rather than an
 * IntersectionObserver because the question is "is this element AT the edge",
 * which a rect comparison answers exactly and a threshold only approximates.
 */
export function markStuckPills(scroller: HTMLElement | null): void {
  if (scroller === null) return;
  // The sticky edge is the SCROLLPORT's padding box plus the pill's declared
  // inset. Read both from CSS so the stamp follows the layout instead of
  // keeping a second, slightly different copy of its geometry here.
  const padding = Number.parseFloat(getComputedStyle(scroller).paddingTop) || 0;
  const edge = scroller.getBoundingClientRect().top + padding;
  const scrolled = scroller.scrollTop > 0;
  for (const day of scroller.querySelectorAll<HTMLElement>(".tr-day")) {
    const rect = day.getBoundingClientRect();
    const inset = Number.parseFloat(getComputedStyle(day).top) || 0;
    const top = edge + inset;
    // Pinned means AT the edge and still in view: a pill whose section has
    // ended is being pushed out, and a pushed-out pill is not chrome.
    const stuck = scrolled && rect.top - top <= 1 && rect.bottom > edge;
    day.setAttribute("data-stuck", stuck ? "true" : "false");
  }
}
