import type { CalendarHeatmapCellData, CalendarHeatmapData } from "../components/charts/types";

export type ActivityRange = "30d" | "90d" | "53w";

const RANGE_DAYS: Record<ActivityRange, number> = { "30d": 30, "90d": 90, "53w": 371 };

/** Calendar arithmetic in UTC on date keys, independent of DST and timezone. */
function shiftDay(key: string, count: number): string {
  const date = new Date(`${key}T12:00:00Z`);
  date.setUTCDate(date.getUTCDate() + count);
  return date.toISOString().slice(0, 10);
}

export interface ActivityRun {
  readonly start: string;
  readonly end: string;
  readonly length: number;
}

export interface ActivityDetails {
  readonly calendar: CalendarHeatmapData;
  readonly days: readonly CalendarHeatmapCellData[];
  readonly start: string;
  readonly end: string;
  readonly total: number | null;
  readonly recordedDayCount: number;
  readonly activeDayCount: number;
  readonly longestStreak: number;
  readonly busiestDay: CalendarHeatmapCellData | null;
  readonly runs: readonly ActivityRun[];
  readonly bestRun: ActivityRun | null;
  readonly currentRun: ActivityRun | null;
}

/** Every expanded figure comes from the same daily cells as the small calendar.
 * Missing/future days never become measured zeroes or average denominators. */
export function createActivityDetails(
  data: CalendarHeatmapData,
  range: ActivityRange,
  today: string,
): ActivityDetails {
  const firstDay = data.weeks[0]?.days[0]?.id ?? today;
  const cutoff = shiftDay(today, 1 - RANGE_DAYS[range]);
  const start = firstDay > cutoff ? firstDay : cutoff;
  const inRange = (key: string) => key >= start && key <= today;
  let previousMonth = "";
  let lastLabelledWeek = -4;
  const labelSpacing = range === "53w" ? 4 : 2;
  const calendar: CalendarHeatmapData = {
    weeks: data.weeks
      .filter((week) => week.days.some((day) => inRange(day.id)))
      .map((week, index) => {
        const first = week.days.find((day) => inRange(day.id))!;
        const month = first.id.slice(0, 7);
        const label = month !== previousMonth && index - lastLabelledWeek >= labelSpacing
          ? new Date(`${first.id}T12:00:00Z`).toLocaleDateString("en-US", { month: "short", timeZone: "UTC" }) : undefined;
        if (label) lastLabelledWeek = index;
        previousMonth = month;
        return {
          id: week.id, label,
          days: week.days.map((day) => ({
            ...day,
            value: inRange(day.id) && day.value !== null && Number.isFinite(day.value) && day.value >= 0
              ? day.value : null,
          })),
        };
      }),
  };
  const days = calendar.weeks.flatMap((week) => week.days).filter((day) => inRange(day.id));
  const runs: ActivityRun[] = [];
  let total = 0;
  let recordedDayCount = 0;
  let activeDayCount = 0;
  let streak = 0;
  let longestStreak = 0;
  let busiestDay: CalendarHeatmapCellData | null = null;
  let previousDay: string | null = null;
  let runStart = "";
  for (const day of days) {
    if (previousDay !== null && shiftDay(previousDay, 1) !== day.id) streak = 0;
    previousDay = day.id;
    if (day.value === null) { streak = 0; continue; }
    recordedDayCount++;
    total += day.value;
    if (day.value > 0) {
      activeDayCount++;
      if (streak === 0) runStart = day.id;
      else runs.pop();
      longestStreak = Math.max(longestStreak, ++streak);
      runs.push({ start: runStart, end: day.id, length: streak });
      if (busiestDay === null || day.value > busiestDay.value!) busiestDay = day;
    } else {
      streak = 0;
    }
  }
  return {
    calendar, days, start, end: today,
    total: recordedDayCount > 0 ? total : null,
    recordedDayCount, activeDayCount, longestStreak, busiestDay,
    runs,
    bestRun: runs.reduce<ActivityRun | null>((best, run) => !best || run.length >= best.length ? run : best, null),
    currentRun: runs.at(-1)?.end === today ? runs.at(-1)! : null,
  };
}
