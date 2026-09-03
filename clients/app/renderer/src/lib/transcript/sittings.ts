// §07 — days and sittings. Every mature chat client cuts a long thread the same
// two ways: a day divider when the calendar turns, and a pause mark when the
// silence means the people left. Here ONE pill carries both cuts, because two
// marks for one moment would be the same fact told twice.
//
// Fifteen minutes is the threshold, measured (§07): across six real sessions
// the longest silence INSIDE agent activity was five minutes in five of them,
// so fifteen is three times the quietest working pause. WeChat's five would cut
// a session mid-reply; iMessage's hour would miss every lunch.
//
// Pure over the ordered entries' `at`. `now` is a parameter, never a clock read.
import type { TimelineEntry } from "@rosetta/protocol";
import { dayLabel } from "../format.ts";

/** A silence this long or longer means the reader left and came back. */
export const SITTING_PAUSE_MS = 15 * 60_000;

/** Adjacent successful setting commands inside this window may share one
 * compact configuration group. Conversational prompts never use this rule. */
export const PROMPT_CLUSTER_MS = 5 * 60_000;

const MINUTES_TO_MS = 60_000;

/** The viewer's offset, minutes east of UTC. */
function viewerUtcOffsetMinutes(): number {
  return -new Date().getTimezoneOffset();
}

/**
 * The viewer-local day a timestamp falls in, as `YYYY-MM-DD`. The same form
 * `lib/model.ts`'s `getCalendarDayKey` buckets the heatmap in — copied rather
 * than imported because that module pulls in JSX through the agent marks, and
 * this one must stay importable by a plain node test.
 */
export function calendarDayKey(timestamp: number): string {
  const date = new Date(timestamp + viewerUtcOffsetMinutes() * MINUTES_TO_MS);
  const month = String(date.getUTCMonth() + 1).padStart(2, "0");
  const day = String(date.getUTCDate()).padStart(2, "0");
  return `${date.getUTCFullYear()}-${month}-${day}`;
}

/**
 * Precision follows distance, in the words the session list already uses:
 * `Today`, `Yesterday`, `Mon 24 Aug` — and the year only when it is not this
 * one. `lib/format.ts`'s `dayLabel` is canonical for the first three; this adds
 * §07's year rule on top of it so the pill and the session list never disagree.
 */
export function sittingDayLabel(timestamp: number, now: number): string {
  const then = new Date(timestamp);
  if (then.getFullYear() !== new Date(now).getFullYear()) {
    return then.toLocaleDateString("en-GB", {
      day: "numeric",
      month: "short",
      year: "numeric",
    });
  }
  return dayLabel(timestamp, now);
}

export interface Sitting {
  /** Stable across renders: the first row's `timeline_id`. */
  readonly key: string;
  /** `YYYY-MM-DD`, viewer-local. Null when no row in the sitting is stamped. */
  readonly dayKey: string | null;
  /** `Today` · `Yesterday` · `Mon 31 Aug` · `24 Aug 2025`. */
  readonly dayLabel: string | null;
  /** The pill's time, drawn only when the reader actually came back: null for
   * the first sitting and for a day that turned without a pause. */
  readonly resumedAt: number | null;
  /** The silence before this sitting. Null when the cut was a day change with
   * no pause of fifteen minutes or more. */
  readonly pauseMs: number | null;
  /** The cut crossed a calendar day, so the pill names it. True for the first
   * sitting, whose pill is the day alone. */
  readonly dayChanged: boolean;
  readonly isFirst: boolean;
  readonly startedAt: number | null;
  readonly endedAt: number | null;
  readonly rows: readonly TimelineEntry[];
}

interface OpenSitting {
  rows: TimelineEntry[];
  resumedAt: number | null;
  pauseMs: number | null;
  dayChanged: boolean;
}

/**
 * Fold ordered entries into sittings: open a new one before any entry whose
 * calendar day differs from the previous stamped entry's, or whose gap from it
 * is SITTING_PAUSE_MS or more.
 *
 * A row with `at: null` NEVER cuts — a missing stamp is not evidence that the
 * reader left. Nor does a stamp that runs backwards: real transcripts carry
 * out-of-order stamps (a compaction summary lands before the command that
 * caused it), and a negative gap is a record artefact, not a silence.
 */
export function sittings(rows: readonly TimelineEntry[], now = Date.now()): Sitting[] {
  const open: OpenSitting[] = [];
  let previousAt: number | null = null;

  for (const row of rows) {
    const at = row.at;
    const current = open.at(-1);
    if (current === undefined) {
      open.push({ rows: [row], resumedAt: null, pauseMs: null, dayChanged: true });
      if (at !== null) previousAt = at;
      continue;
    }
    if (at === null || previousAt === null) {
      current.rows.push(row);
      continue;
    }
    const gap = at - previousAt;
    const pauseMs = gap >= SITTING_PAUSE_MS ? gap : null;
    const dayChanged = calendarDayKey(at) !== calendarDayKey(previousAt);
    if (pauseMs === null && !dayChanged) {
      current.rows.push(row);
      previousAt = at;
      continue;
    }
    open.push({
      rows: [row],
      // The time is what the reader resumed at, so it is drawn only when there
      // was something to resume from.
      resumedAt: pauseMs === null ? null : at,
      pauseMs,
      dayChanged,
    });
    previousAt = at;
  }

  return open.map((sitting, index) => {
    const stamps = sitting.rows
      .map((row) => row.at)
      .filter((value): value is number => value !== null);
    const startedAt = stamps.length === 0 ? null : Math.min(...stamps);
    const endedAt = stamps.length === 0 ? null : Math.max(...stamps);
    return {
      key: sitting.rows[0].timeline_id,
      dayKey: startedAt === null ? null : calendarDayKey(startedAt),
      dayLabel: startedAt === null ? null : sittingDayLabel(startedAt, now),
      resumedAt: sitting.resumedAt,
      pauseMs: sitting.pauseMs,
      dayChanged: sitting.dayChanged,
      isFirst: index === 0,
      startedAt,
      endedAt,
      rows: sitting.rows,
    };
  });
}

/** One row of the pill's menu: `Mon 31 Aug · 1 sitting`. */
export interface DayEntry {
  readonly dayKey: string;
  readonly dayLabel: string;
  readonly sittingCount: number;
  /** Pressing the row lands the transcript on this sitting's first row. */
  readonly firstSittingKey: string;
  readonly startedAt: number | null;
}

/**
 * The day list the pill opens into, in document order. A sitting that could
 * not name its day is left out rather than filed under a guess.
 */
export function dayList(list: readonly Sitting[]): DayEntry[] {
  const days: DayEntry[] = [];
  const byKey = new Map<string, number>();
  for (const sitting of list) {
    if (sitting.dayKey === null || sitting.dayLabel === null) continue;
    const seen = byKey.get(sitting.dayKey);
    if (seen === undefined) {
      byKey.set(sitting.dayKey, days.length);
      days.push({
        dayKey: sitting.dayKey,
        dayLabel: sitting.dayLabel,
        sittingCount: 1,
        firstSittingKey: sitting.key,
        startedAt: sitting.startedAt,
      });
      continue;
    }
    const existing = days[seen];
    days[seen] = { ...existing, sittingCount: existing.sittingCount + 1 };
  }
  return days;
}

/** `3 sittings` on the session row — the same count, from the same threshold. */
export function sittingCount(rows: readonly TimelineEntry[], now = Date.now()): number {
  return sittings(rows, now).length;
}

/**
 * The time half of the setting-group rule. Whether both rows are successful
 * human setting commands, or a response stood between them, is the document's
 * business; `turns.ts` owns those checks and sets
 * `Turn.clusteredWithPrevious`.
 */
export function withinPromptCluster(
  previousAt: number | null,
  currentAt: number | null,
): boolean {
  if (previousAt === null || currentAt === null) return false;
  const gap = currentAt - previousAt;
  return gap >= 0 && gap <= PROMPT_CLUSTER_MS;
}
