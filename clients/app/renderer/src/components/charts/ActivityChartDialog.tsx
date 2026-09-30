import { useMemo, useState } from "react";
import type { CSSProperties } from "react";
import { CalendarHeatmap } from "./CalendarHeatmap";
import type { CalendarHeatmapCellData, CalendarHeatmapData } from "./types";
import { ColumnHeading, DialogShell, SegmentedControl } from "../ui";
import { CloseGlyph, ExitFullscreenGlyph, FullscreenGlyph } from "../../lib/icons";
import { createActivityDetails } from "../../lib/activity-details";
import type { ActivityRange } from "../../lib/activity-details";
import { tokenText } from "../../lib/format";
import "../../styles/activity-chart.css";

const RANGE_OPTIONS = [
  { value: "30d", label: "30 days" },
  { value: "90d", label: "90 days" },
  { value: "53w", label: "53 weeks" },
] as const;
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

function dateLabel(key: string, weekday = false): string {
  return new Date(`${key}T12:00:00`).toLocaleDateString("en-US", {
    ...(weekday ? { weekday: "long" as const } : {}),
    month: "short", day: "numeric", year: "numeric",
  });
}

export interface ActivityChartDialogProps {
  readonly name: string;
  readonly color: string;
  readonly data: CalendarHeatmapData;
  readonly today: string;
  readonly onClose: () => void;
}

export function ActivityChartDialog({ name, color, data, today, onClose }: ActivityChartDialogProps) {
  const [range, setRange] = useState<ActivityRange>("53w");
  const [fullscreen, setFullscreen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [preview, setPreview] = useState<CalendarHeatmapCellData | null>(null);
  const details = useMemo(() => createActivityDetails(data, range, today), [data, range, today]);
  const selectedDay = details.days.find((day) => day.id === selectedId && day.value !== null)
    ?? details.days.filter((day) => day.value !== null).at(-1) ?? null;
  const inspectedDay = details.days.find((day) => day.id === preview?.id && day.value !== null)
    ?? selectedDay;
  const monthlyPeak = Math.max(1, ...details.months.map((month) => month.total));
  const weekdayPeak = Math.max(1, ...details.weekdays.map((day) => day.average ?? 0));
  const dailyPeak = Math.max(0, ...details.days.map((day) => day.value ?? 0));
  const isShortRange = range !== "53w";
  const selectDay = (day: CalendarHeatmapCellData) => { setSelectedId(day.id); setPreview(null); };

  return (
    <DialogShell title={`${name} activity`} onClose={onClose} fullscreen={fullscreen}
      onExitFullscreen={() => setFullscreen(false)}>
      <header className="ah-fileview__head">
        <div className="ah-fileview__title">
          <ColumnHeading name={name} tint={color} />
          <span className="ah-fileview__kind">Activity</span>
        </div>
        <div className="ah-fileview__acts">
          <button className="ah-iconbtn" type="button" aria-pressed={fullscreen}
            title={fullscreen ? "Exit full screen (Esc)" : "View full screen"}
            aria-label={fullscreen ? "Exit full screen" : "View full screen"}
            onClick={() => setFullscreen((current) => !current)}>
            {fullscreen ? <ExitFullscreenGlyph /> : <FullscreenGlyph />}
          </button>
          <button className="ah-iconbtn" type="button" title="Close (Esc)"
            aria-label="Close activity chart" onClick={onClose}><CloseGlyph /></button>
        </div>
      </header>
      <div className="ah-activity" style={{ "--activity-color": color } as CSSProperties}>
        <div className="ah-activity__toolbar">
          <div><h2>Activity over time</h2><p>{dateLabel(details.start)} – {dateLabel(details.end)}</p></div>
          <SegmentedControl value={range} options={RANGE_OPTIONS} accessibleLabel="Activity date range"
            onValueChange={(value) => { setRange(value); setPreview(null); }} />
        </div>
        <dl className="ah-activity__stats">
          <div><dt>Total tokens</dt><dd>{details.total === null ? "—" : tokenText(details.total)}</dd></div>
          <div><dt>Active days</dt><dd>{details.total === null ? "—" : details.activeDayCount}
            <small> / {details.recordedDayCount} recorded</small></dd></div>
          <div><dt>Best streak in range</dt><dd>{details.total === null ? "—" : `${details.longestStreak}d`}</dd></div>
          <div><dt>Average / recorded day</dt><dd>{details.total === null ? "—"
            : tokenText(Math.round(details.total / details.recordedDayCount))}</dd></div>
        </dl>
        <section className="ah-activity__calendar ah-activity__panel" aria-labelledby="activity-calendar-title">
          <div className="ah-activity__sectionhead"><h3 id="activity-calendar-title">Daily tokens</h3>
            <p>Hover to inspect · click to select · arrow keys to move</p></div>
          <div className="ah-activity__calendar-main" data-short-range={isShortRange ? "" : undefined}
            style={{ "--activity-short-width": `${Math.max(330, details.calendar.weeks.length * 25 + 36)}px` } as CSSProperties}>
            <div className="ah-activity__calendar-scroll">
              <div className="ah-activity__calendar-layout" style={{
                "--activity-weeks": details.calendar.weeks.length,
                minWidth: Math.min(730, Math.max(330, details.calendar.weeks.length * 15)),
              } as CSSProperties}>
                <div className="ah-activity__weekdays" aria-hidden="true">{WEEKDAYS.map((day) => <span key={day}>{day}</span>)}</div>
                <CalendarHeatmap data={details.calendar} color={color}
                  ariaLabel={`Daily token activity for ${name}. Use arrow keys to navigate days.`}
                  summary={`${details.recordedDayCount} recorded days`}
                  selectedDayId={selectedDay?.id} onDaySelect={selectDay} onDayPreview={setPreview} />
              </div>
            </div>
            {isShortRange && <div className="ah-activity__daily">
              <p>Daily totals <span>{details.total === null ? "No recorded days" : `${tokenText(dailyPeak)} peak`}</span></p>
              <div className="ah-activity__daily-plot" role="group" aria-label="Daily token totals"
                onMouseLeave={() => setPreview(null)}>
                {details.days.map((day) => day.value === null ? <span key={day.id} /> :
                  <button key={day.id} type="button" tabIndex={-1} aria-label={day.label}
                    aria-pressed={selectedDay?.id === day.id} data-trend-day={day.id}
                    onClick={() => selectDay(day)} onMouseEnter={() => setPreview(day)}
                    onFocus={() => setPreview(day)} onBlur={() => setPreview(null)}>
                    <i style={{ height: `${dailyPeak > 0 ? day.value / dailyPeak * 100 : 0}%` }} />
                  </button>)}
              </div>
              <div className="ah-activity__daily-axis"><span>{dateLabel(details.start)}</span><span>{dateLabel(details.end)}</span></div>
            </div>}
          </div>
          <div className="ah-activity__inspection">
            <div>{inspectedDay ? <><span>{dateLabel(inspectedDay.id, true)}</span>
              <strong>{inspectedDay.value!.toLocaleString()} <small>tokens</small></strong>
              <span>{details.total ? `${(inspectedDay.value! / details.total * 100).toFixed(1)}% of this range`
                : "No recorded token activity on this day"}</span></>
              : <span>No recorded days in this range</span>}</div>
            {details.busiestDay && <button type="button" className="ah-activity__jump"
              onClick={() => selectDay(details.busiestDay!)}>Busiest day <span>{dateLabel(details.busiestDay.id)}</span></button>}
          </div>
        </section>
        <div className="ah-activity__breakdowns">
          <section className="ah-activity__panel" aria-labelledby="activity-months-title">
            <div className="ah-activity__sectionhead"><h3 id="activity-months-title">Monthly totals</h3>
              <p>Within the selected range</p></div>
            <ol className="ah-activity__months">
              {details.months.map((month) => {
                const label = new Date(`${month.id}-01T12:00:00`).toLocaleDateString("en-US", { month: "short", year: "2-digit" });
                return <li key={month.id} title={`${label}: ${month.recordedDays ? `${month.total.toLocaleString()} tokens` : "not recorded"}`}>
                  <span>{month.recordedDays ? tokenText(month.total) : "—"}</span>
                  <div className="ah-activity__month-track" aria-hidden="true"><i style={{ height: `${month.total / monthlyPeak * 100}%` }} /></div>
                  <span>{label}</span>
                </li>;
              })}
            </ol>
          </section>
          <section className="ah-activity__panel" aria-labelledby="activity-weekdays-title">
            <div className="ah-activity__sectionhead"><h3 id="activity-weekdays-title">By weekday</h3>
              <p>Average tokens / recorded day</p></div>
            <ol className="ah-activity__weekday-bars">
              {details.weekdays.map((day) => <li key={day.day}
                title={`${WEEKDAYS[day.day]}: ${day.recordedDays} recorded days`}>
                <span>{WEEKDAYS[day.day]}</span>
                <div aria-hidden="true"><i style={{ width: `${(day.average ?? 0) / weekdayPeak * 100}%` }} /></div>
                <b>{day.average === null ? "—" : tokenText(Math.round(day.average))}</b>
              </li>)}
            </ol>
          </section>
        </div>
      </div>
    </DialogShell>
  );
}
