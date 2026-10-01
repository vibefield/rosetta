import type { UsageResponse } from "@rosetta/protocol";
import type {
  CalendarHeatmapData,
  StreamgraphData,
  StreamgraphSeriesData,
} from "../components/charts";
import { createModelColors, formatModelLabel } from "./model-labels";
import { ACTIVITY_HEATMAP_WEEK_COUNT, getCalendarDayKey } from "./model";

const DAY_IN_MS = 86_400_000;
const STREAMGRAPH_DAY_COUNT = 30;
const MAX_VISIBLE_MODEL_SERIES = 3;
const MONTH_NAMES = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
] as const;

export interface ActivityStats {
  readonly activeDayCount: number;
  readonly currentStreakLength: number;
  readonly longestStreakLength: number;
}

function finiteMagnitude(value: number | undefined): number | null {
  if (value === undefined || !Number.isFinite(value) || value < 0) return null;
  return value;
}

function tokenCountLabel(value: number): string {
  return `${value.toLocaleString()} ${value === 1 ? "token" : "tokens"}`;
}

function parseCalendarDayKey(key: string): {
  year: number;
  monthIndex: number;
  dayOfMonth: number;
} {
  const [year, month, day] = key.split("-").map(Number);
  return { year, monthIndex: month - 1, dayOfMonth: day };
}

function longCalendarDateLabel(key: string): string {
  const { year, monthIndex, dayOfMonth } = parseCalendarDayKey(key);
  return `${MONTH_NAMES[monthIndex]} ${dayOfMonth}, ${year}`;
}

/** Converts the view model's dense daily series into heatmap columns. */
export function createCalendarHeatmapData(
  dailyTokenValues: readonly number[],
  gridStartTimestamp: number,
): CalendarHeatmapData {
  let previousMonthIndex = -1;
  let lastLabelledWeek = -4;

  const weeks = Array.from(
    { length: ACTIVITY_HEATMAP_WEEK_COUNT },
    (_, weekIndex) => {
      // Noon avoids crossing a calendar boundary when the range spans DST.
      const weekTimestamp =
        gridStartTimestamp + weekIndex * 7 * DAY_IN_MS + DAY_IN_MS / 2;
      const weekKey = getCalendarDayKey(weekTimestamp);
      const monthIndex = parseCalendarDayKey(weekKey).monthIndex;
      let label: string | undefined;

      if (
        monthIndex !== previousMonthIndex &&
        weekIndex < ACTIVITY_HEATMAP_WEEK_COUNT - 1
      ) {
        previousMonthIndex = monthIndex;
        if (weekIndex - lastLabelledWeek >= 4) {
          lastLabelledWeek = weekIndex;
          label = MONTH_NAMES[monthIndex];
        }
      }

      const days = Array.from({ length: 7 }, (_, dayOfWeek) => {
        const valueIndex = weekIndex * 7 + dayOfWeek;
        const timestamp =
          gridStartTimestamp + valueIndex * DAY_IN_MS + DAY_IN_MS / 2;
        const key = getCalendarDayKey(timestamp);
        const value = finiteMagnitude(dailyTokenValues[valueIndex]);
        return {
          id: key,
          value,
          label: `${longCalendarDateLabel(key)}: ${
            value === null ? "not available" : tokenCountLabel(value)
          }`,
        };
      });

      return {
        id: weekKey,
        ...(label ? { label } : {}),
        days,
      };
    },
  );

  return { weeks };
}

export function calculateActivityStats(
  dailyTokenValues: readonly number[],
  currentDayIndex: number,
): ActivityStats {
  const lastIndex = Math.min(
    Math.max(0, Math.floor(currentDayIndex)),
    Math.max(0, dailyTokenValues.length - 1),
  );
  let activeDayCount = 0;
  let longestStreakLength = 0;
  let runningStreakLength = 0;

  for (let index = 0; index <= lastIndex; index += 1) {
    if ((dailyTokenValues[index] ?? 0) > 0) {
      activeDayCount += 1;
      runningStreakLength += 1;
      longestStreakLength = Math.max(longestStreakLength, runningStreakLength);
    } else {
      runningStreakLength = 0;
    }
  }

  let currentStreakLength = 0;
  for (
    let index = lastIndex;
    index >= 0 && (dailyTokenValues[index] ?? 0) > 0;
    index -= 1
  ) {
    currentStreakLength += 1;
  }

  return { activeDayCount, currentStreakLength, longestStreakLength };
}

function createStreamgraphDomain(currentTimestamp: number): StreamgraphData["domain"] {
  const visibleTickIndices = new Set([0, 10, 20, STREAMGRAPH_DAY_COUNT - 1]);
  return Array.from({ length: STREAMGRAPH_DAY_COUNT }, (_, dayIndex) => {
    const timestamp =
      currentTimestamp - (STREAMGRAPH_DAY_COUNT - 1 - dayIndex) * DAY_IN_MS;
    const key = getCalendarDayKey(timestamp);
    const { monthIndex, dayOfMonth } = parseCalendarDayKey(key);
    return {
      id: key,
      ...(visibleTickIndices.has(dayIndex)
        ? { label: `${MONTH_NAMES[monthIndex]} ${dayOfMonth}` }
        : {}),
    };
  });
}

/** Aggregates protocol usage rows into aligned, presentation-ready layers. */
export function createModelUsageStreamgraphData(
  usage: UsageResponse,
  currentTimestamp: number,
): StreamgraphData {
  const domain = createStreamgraphDomain(currentTimestamp);
  const visibleDays = new Set(domain.map((point) => point.id));
  const tokensByModelAndDay = new Map<string, Map<string, number>>();
  const modelColors = createModelColors(usage.models.map(row => row.model));

  for (const row of usage.models) {
    if (!visibleDays.has(row.day) || !Number.isFinite(row.tokens) || row.tokens <= 0) continue;

    const tokensByDay = tokensByModelAndDay.get(row.model) ?? new Map<string, number>();
    tokensByDay.set(row.day, (tokensByDay.get(row.day) ?? 0) + row.tokens);
    tokensByModelAndDay.set(row.model, tokensByDay);
  }

  const allSeries = [...tokensByModelAndDay.entries()]
    .map(([modelId, tokensByDay]) => {
      const values = domain.map((point) => tokensByDay.get(point.id) ?? 0);
      return {
        id: modelId,
        label: formatModelLabel(modelId),
        color: modelColors[modelId],
        values,
        total: values.reduce((sum, value) => sum + value, 0),
      };
    })
    .filter((series) => series.total > 0)
    .sort(
      (left, right) =>
        right.total - left.total || left.id.localeCompare(right.id),
    );

  const series: StreamgraphSeriesData[] = allSeries
    .slice(0, MAX_VISIBLE_MODEL_SERIES)
    .map(({ id, label, color, values }) => ({ id, label, color, values }));
  const remainingSeries = allSeries.slice(MAX_VISIBLE_MODEL_SERIES);
  if (remainingSeries.length > 0) {
    const existingSeriesIds = new Set(allSeries.map((modelSeries) => modelSeries.id));
    let aggregateSeriesId = "__other_models__";
    while (existingSeriesIds.has(aggregateSeriesId)) aggregateSeriesId += "_";

    series.push({
      id: aggregateSeriesId,
      label: `other ×${remainingSeries.length}`,
      color: "var(--vf-text-faint)",
      values: domain.map((_, valueIndex) =>
        remainingSeries.reduce(
          (total, modelSeries) => total + modelSeries.values[valueIndex],
          0,
        ),
      ),
    });
  }

  return { domain, series };
}
