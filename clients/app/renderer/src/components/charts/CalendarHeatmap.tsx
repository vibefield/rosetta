import { useMemo } from "react";
import type { CSSProperties, ReactNode } from "react";
import type { CalendarHeatmapData } from "./types";

const MAX_INTENSITY_LEVEL = 4;
const INTENSITY_LEVELS = [0, 1, 2, 3, 4] as const;

type HeatmapColorStyle = CSSProperties & { "--heatmap-color": string };
type HeatmapCellStyle = CSSProperties & { "--heatmap-intensity": number };

export interface CalendarHeatmapProps {
  readonly data: CalendarHeatmapData;
  readonly color: string;
  readonly ariaLabel: string;
  readonly summary?: ReactNode;
  readonly lowIntensityLabel?: string;
  readonly highIntensityLabel?: string;
}

function getIntensityLevel(value: number, peak: number): number {
  if (!Number.isFinite(value) || value <= 0 || peak <= 0) return 0;

  const ratio = value / peak;
  if (ratio > 0.66) return 4;
  if (ratio > 0.4) return 3;
  if (ratio > 0.18) return 2;
  return 1;
}

export function CalendarHeatmap({
  data,
  color,
  ariaLabel,
  summary,
  lowIntensityLabel = "less",
  highIntensityLabel = "more",
}: CalendarHeatmapProps) {
  const peak = useMemo(
    () =>
      data.weeks.reduce(
        (maximum, week) =>
          week.days.reduce(
            (weekMaximum, day) =>
              day.value === null || !Number.isFinite(day.value)
                ? weekMaximum
                : Math.max(weekMaximum, day.value),
            maximum,
          ),
        0,
      ),
    [data],
  );

  return (
    <div
      className="ah-calendar-heatmap"
      style={{ "--heatmap-color": color } as HeatmapColorStyle}
    >
      <div className="ah-calendar-heatmap__axis" aria-hidden="true">
        {data.weeks.map((week, weekIndex) =>
          week.label ? (
            <span
              key={week.id}
              style={{ left: `${((weekIndex / data.weeks.length) * 100).toFixed(2)}%` }}
            >
              {week.label}
            </span>
          ) : null,
        )}
      </div>

      <div className="ah-calendar-heatmap__grid" role="img" aria-label={ariaLabel}>
        {data.weeks.map((week) => (
          <span className="ah-calendar-heatmap__week" key={week.id} aria-hidden="true">
            {week.days.map((day) => {
              const intensity =
                day.value === null ? 0 : getIntensityLevel(day.value, peak);
              return (
                <span
                  className="ah-calendar-heatmap__cell"
                  key={day.id}
                  data-empty={day.value === 0 ? "" : undefined}
                  data-unavailable={day.value === null ? "" : undefined}
                  style={{ "--heatmap-intensity": intensity } as HeatmapCellStyle}
                  title={day.label}
                />
              );
            })}
          </span>
        ))}
      </div>

      <div className="ah-calendar-heatmap__footer">
        {summary === undefined ? <span /> : <span>{summary}</span>}
        <span className="ah-calendar-heatmap__legend" aria-hidden="true">
          {lowIntensityLabel}
          {INTENSITY_LEVELS.map((level) => (
            <i
              key={level}
              data-empty={level === 0 ? "" : undefined}
              style={
                { "--heatmap-intensity": Math.min(level, MAX_INTENSITY_LEVEL) } as HeatmapCellStyle
              }
            />
          ))}
          {highIntensityLabel}
        </span>
      </div>
    </div>
  );
}
