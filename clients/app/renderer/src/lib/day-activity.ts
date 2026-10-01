import type { ModelHour } from "@rosetta/protocol";
import { createModelColors, formatModelLabel } from "./model-labels.ts";

/** Recorded hourly buckets, never estimated by distributing a day's total. */
export function createDayActivity(rows: readonly ModelHour[], colors?: Readonly<Record<string, string>>) {
  const modelColors = { ...createModelColors(rows.map(row => row.model)), ...colors };
  const models = new Map<string, number[]>();
  for (const row of rows) {
    if (!Number.isInteger(row.hour) || row.hour < 0 || row.hour > 23
      || !Number.isFinite(row.tokens) || row.tokens <= 0) continue;
    const values = models.get(row.model) ?? Array<number>(24).fill(0);
    values[row.hour] += row.tokens;
    models.set(row.model, values);
  }
  const series = [...models].map(([id, values]) => ({
    id, label: formatModelLabel(id), color: modelColors[id], values,
    total: values.reduce((sum, value) => sum + value, 0),
  })).sort((a, b) => b.total - a.total || a.id.localeCompare(b.id));
  const hours = Array.from({ length: 24 }, (_, hour) =>
    series.reduce((sum, model) => sum + model.values[hour], 0));
  const total = hours.reduce((sum, value) => sum + value, 0);
  const peakHour = total > 0 ? hours.indexOf(Math.max(...hours)) : null;
  return { series, hours, total, peakHour };
}

export function hourLabel(hour: number): string {
  return `${hour % 12 || 12} ${hour < 12 ? "AM" : "PM"}`;
}

type Point = readonly [x: number, y: number];

/** C1 cubic interpolation through the samples. Bounded tangents keep each
 * segment between its endpoints, including zero buckets and sharp peaks.
 * Flat end tangents also meet the pending-hours guide without a corner. */
export function createHourlyCurve(points: readonly Point[]): string {
  if (!points.length) return "";
  const slopes = points.slice(1).map((point, index) =>
    (point[1] - points[index][1]) / (point[0] - points[index][0]));
  const tangents = points.map((_, index) => {
    if (index === 0 || index === points.length - 1) return 0;
    const before = slopes[index - 1];
    const after = slopes[index];
    return before * after > 0
      ? Math.sign(before) * Math.min(Math.abs(before), Math.abs(after)) : 0;
  });
  let path = `M${points[0][0]},${points[0][1]}`;
  for (let index = 1; index < points.length; index++) {
    const [x0, y0] = points[index - 1];
    const [x1, y1] = points[index];
    const step = (x1 - x0) / 3;
    path += ` C${x0 + step},${y0 + tangents[index - 1] * step}`
      + ` ${x1 - step},${y1 - tangents[index] * step} ${x1},${y1}`;
  }
  return path;
}
