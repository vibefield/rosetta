import { useMemo } from "react";
import type { CSSProperties, ReactNode } from "react";
import type { CalendarHeatmapCellData, CalendarHeatmapData } from "./types";

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
  /** Supplying a selection handler enables keyboard and pointer inspection. */
  readonly selectedDayId?: string;
  readonly onDaySelect?: (day: CalendarHeatmapCellData) => void;
  readonly onDayPreview?: (day: CalendarHeatmapCellData | null) => void;
  readonly dayMarkers?: Readonly<Record<string, "current" | "best" | "both">>;
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
  selectedDayId,
  onDaySelect,
  onDayPreview,
  dayMarkers,
}: CalendarHeatmapProps) {
  const availableDays = useMemo(
    () => data.weeks.flatMap((week) => week.days).filter((day) => day.value !== null),
    [data],
  );
  const tabbableDayId = availableDays.find((day) => day.id === selectedDayId)?.id
    ?? availableDays.at(-1)?.id;
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

      <div
        className="ah-calendar-heatmap__grid"
        role={onDaySelect ? "group" : "img"}
        aria-label={ariaLabel}
        onMouseLeave={() => onDayPreview?.(null)}
      >
        {data.weeks.map((week) => (
          <span className="ah-calendar-heatmap__week" key={week.id} aria-hidden={onDaySelect ? undefined : true}>
            {week.days.map((day) => {
              const intensity =
                day.value === null ? 0 : getIntensityLevel(day.value, peak);
              if (onDaySelect && day.value !== null) {
                return (
                  <button
                    type="button"
                    className="ah-calendar-heatmap__cell"
                    key={day.id}
                    data-day={day.id}
                    data-run={dayMarkers?.[day.id]}
                    data-empty={day.value === 0 ? "" : undefined}
                    style={{ "--heatmap-intensity": intensity } as HeatmapCellStyle}
                    aria-label={day.label}
                    aria-pressed={selectedDayId === day.id}
                    tabIndex={tabbableDayId === day.id ? 0 : -1}
                    onClick={() => onDaySelect(day)}
                    onMouseEnter={() => onDayPreview?.(day)}
                    onFocus={() => onDayPreview?.(day)}
                    onBlur={() => onDayPreview?.(null)}
                    onKeyDown={(event) => {
                      const calendarDays = data.weeks.flatMap((week) => week.days);
                      const index = calendarDays.findIndex((entry) => entry.id === day.id);
                      const offsets: Record<string, number> = {
                        ArrowUp: -1, ArrowDown: 1, ArrowLeft: -7, ArrowRight: 7,
                      };
                      const targetIndex = event.key === "Home"
                        ? calendarDays.findIndex((entry) => entry.id === availableDays[0]?.id)
                        : event.key === "End"
                          ? calendarDays.findIndex((entry) => entry.id === availableDays.at(-1)?.id)
                        : event.key in offsets ? index + offsets[event.key] : null;
                      if (targetIndex === null) return;
                      event.preventDefault();
                      const direction = targetIndex < index ? -1 : 1;
                      let target = Math.max(0, Math.min(calendarDays.length - 1, targetIndex));
                      while (calendarDays[target]?.value === null) target += direction;
                      const next = calendarDays[target];
                      if (!next || next.value === null) return;
                      onDaySelect(next);
                      event.currentTarget.closest(".ah-calendar-heatmap__grid")
                        ?.querySelector<HTMLButtonElement>(`[data-day="${next.id}"]`)?.focus();
                    }}
                  />
                );
              }
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
