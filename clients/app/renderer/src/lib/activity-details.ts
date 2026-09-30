import type { CalendarHeatmapCellData, CalendarHeatmapData } from "../components/charts/types";

export type ActivityRange = "30d" | "90d" | "53w";

const RANGE_DAYS: Record<ActivityRange, number> = { "30d": 30, "90d": 90, "53w": 371 };

/** Calendar arithmetic in UTC on date keys, independent of DST and timezone. */
function shiftDay(key: string, count: number): string {
  const date = new Date(`${key}T12:00:00Z`);
  date.setUTCDate(date.getUTCDate() + count);
  return date.toISOString().slice(0, 10);
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
  readonly months: readonly { id: string; total: number; recordedDays: number }[];
  readonly weekdays: readonly { day: number; average: number | null; recordedDays: number }[];
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
  const months = new Map<string, { id: string; total: number; recordedDays: number }>();
  const weekdays = Array.from({ length: 7 }, (_, day) => ({ day, total: 0, recordedDays: 0 }));
  let total = 0;
  let recordedDayCount = 0;
  let activeDayCount = 0;
  let streak = 0;
  let longestStreak = 0;
  let busiestDay: CalendarHeatmapCellData | null = null;
  let previousDay: string | null = null;
  for (const day of days) {
    const monthId = day.id.slice(0, 7);
    const month = months.get(monthId) ?? { id: monthId, total: 0, recordedDays: 0 };
    months.set(monthId, month);
    if (previousDay !== null && shiftDay(previousDay, 1) !== day.id) streak = 0;
    previousDay = day.id;
    if (day.value === null) { streak = 0; continue; }
    recordedDayCount++;
    total += day.value;
    month.total += day.value;
    month.recordedDays++;
    const weekday = weekdays[new Date(`${day.id}T12:00:00Z`).getUTCDay()];
    weekday.total += day.value;
    weekday.recordedDays++;
    if (day.value > 0) {
      activeDayCount++;
      longestStreak = Math.max(longestStreak, ++streak);
      if (busiestDay === null || day.value > busiestDay.value!) busiestDay = day;
    } else {
      streak = 0;
    }
  }
  return {
    calendar, days, start, end: today,
    total: recordedDayCount > 0 ? total : null,
    recordedDayCount, activeDayCount, longestStreak, busiestDay,
    months: [...months.values()],
    weekdays: weekdays.map(({ day, total, recordedDays }) => ({
      day, recordedDays, average: recordedDays > 0 ? total / recordedDays : null,
    })),
  };
}
