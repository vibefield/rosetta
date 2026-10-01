import { useLayoutEffect, useMemo, useState } from "react";
import type { CSSProperties } from "react";
import { CalendarHeatmap } from "./CalendarHeatmap";
import { DayActivityChart } from "./DayActivityChart";
import type { CalendarHeatmapCellData, CalendarHeatmapData } from "./types";
import { ColumnHeading, DialogShell, SegmentedControl } from "../ui";
import { CloseGlyph, ExitFullscreenGlyph, FullscreenGlyph } from "../../lib/icons";
import { createActivityDetails } from "../../lib/activity-details";
import type { ActivityRange, ActivityRun } from "../../lib/activity-details";
import { useDayUsage } from "../../lib/use-day-usage";
import { tokenText } from "../../lib/format";
import "../../styles/activity-chart.css";

const RANGE_OPTIONS = [
  { value: "30d", label: "30 days" },
  { value: "90d", label: "90 days" },
  { value: "53w", label: "53 weeks" },
] as const;
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

function dateLabel(key: string, weekday = false, year = false): string {
  return new Date(`${key}T12:00:00`).toLocaleDateString("en-US", {
    ...(weekday ? { weekday: "short" as const } : {}),
    ...(weekday || year ? { year: "numeric" as const } : {}),
    month: "short", day: "numeric",
  });
}

export interface ActivityChartDialogProps {
  readonly projectId: string;
  readonly name: string;
  readonly color: string;
  readonly data: CalendarHeatmapData;
  readonly today: string;
  readonly offsetMinutes: number;
  readonly currentTimestamp: number;
  readonly modelColors: Readonly<Record<string, string>>;
  readonly onClose: () => void;
}

export function ActivityChartDialog({
  projectId, name, color, data, today, offsetMinutes, currentTimestamp, modelColors, onClose,
}: ActivityChartDialogProps) {
  const [range, setRange] = useState<ActivityRange>("53w");
  const [fullscreen, setFullscreen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [preview, setPreview] = useState<CalendarHeatmapCellData | null>(null);
  const [calendarElement, setCalendarElement] = useState<HTMLDivElement | null>(null);
  const [calendarWidth, setCalendarWidth] = useState(1000);
  const [calendarPage, setCalendarPage] = useState<number | null>(null);
  useLayoutEffect(() => {
    const node = calendarElement;
    if (!node) return;
    const measure = () => setCalendarWidth(node.clientWidth);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [calendarElement]);
  const details = useMemo(() => createActivityDetails(data, range, today), [data, range, today]);
  const history = useMemo(() => createActivityDetails(data, "53w", today), [data, today]);
  const currentIsBest = Boolean(history.currentRun && history.currentRun.start === history.bestRun?.start);
  const availableDays = details.days.filter(day => day.value !== null);
  const selectedDay = availableDays.find(day => day.id === selectedId)
    ?? availableDays.filter(day => day.value! > 0).at(-1) ?? availableDays.at(-1) ?? null;
  const selectedIndex = availableDays.findIndex(day => day.id === selectedDay?.id);
  // Page weeks only when the window cannot keep the day targets readable.
  // Day selection and the previous/next-day actions follow their calendar page.
  const maxWeeks = Math.max(1, Math.floor((calendarWidth - 30) / 16));
  const pageCount = Math.max(1, Math.ceil(details.calendar.weeks.length / maxWeeks));
  const weeksPerPage = Math.max(1, Math.ceil(details.calendar.weeks.length / pageCount));
  const selectedWeek = details.calendar.weeks.findIndex(week => week.days.some(day => day.id === selectedDay?.id));
  const page = Math.min(pageCount - 1, calendarPage ?? Math.floor(Math.max(0, selectedWeek) / weeksPerPage));
  const visibleCalendar = { weeks: details.calendar.weeks.slice(page * weeksPerPage, (page + 1) * weeksPerPage) };
  const calendarLayoutWidth = Math.min(calendarWidth, Math.max(300, visibleCalendar.weeks.length * 16 + 30));
  const usage = useDayUsage(projectId, selectedDay?.id ?? null, offsetMinutes);
  const modelRows = usage.data?.models ?? [];
  const hourlyTotal = modelRows.reduce((sum, row) => sum + row.tokens, 0);
  const selectDay = (day: CalendarHeatmapCellData) => { setSelectedId(day.id); setPreview(null); setCalendarPage(null); };
  const markers: Record<string, "current" | "best" | "both"> = {};
  for (const day of history.days) {
    const current = history.currentRun && day.id >= history.currentRun.start && day.id <= history.currentRun.end;
    const best = history.bestRun && day.id >= history.bestRun.start && day.id <= history.bestRun.end;
    if (current || best) markers[day.id] = current && best ? "both" : current ? "current" : "best";
  }
  const jumpToRun = (run: ActivityRun) => {
    if (run.end < details.start) setRange("53w");
    setSelectedId(run.end); setPreview(null); setCalendarPage(null);
  };

  return <DialogShell title={`${name} activity`} onClose={onClose} fullscreen={fullscreen}
    onExitFullscreen={() => setFullscreen(false)}>
    <header className="ah-fileview__head">
      <div className="ah-fileview__title"><ColumnHeading name={name} tint={color} />
        <span className="ah-fileview__kind">Activity</span></div>
      <div className="ah-fileview__acts">
        <button className="ah-iconbtn" type="button" aria-pressed={fullscreen}
          title={fullscreen ? "Exit full screen (Esc)" : "View full screen"}
          aria-label={fullscreen ? "Exit full screen" : "View full screen"}
          onClick={() => setFullscreen(current => !current)}>
          {fullscreen ? <ExitFullscreenGlyph /> : <FullscreenGlyph />}</button>
        <button className="ah-iconbtn" type="button" title="Close (Esc)"
          aria-label="Close activity chart" onClick={onClose}><CloseGlyph /></button>
      </div>
    </header>
    <div className="ah-activity" style={{ "--activity-color": color } as CSSProperties}>
      <section className="ah-activity__calendar" aria-labelledby="activity-calendar-title">
        <div className="ah-activity__toolbar">
          <div><h2 id="activity-calendar-title">Daily activity</h2>
            <p>{dateLabel(details.start, false, true)} – {dateLabel(details.end, false, true)} · {details.activeDayCount} active days</p></div>
          <div className="ah-activity__calendar-controls">
            <SegmentedControl value={range} options={RANGE_OPTIONS} accessibleLabel="Activity date range"
              onValueChange={value => { setRange(value); setPreview(null); setCalendarPage(null); }} />
            {pageCount > 1 && <div className="ah-activity__calendar-nav" aria-label="Calendar pages">
              <button type="button" aria-label="Earlier calendar weeks" disabled={page === 0}
                onClick={() => { setCalendarPage(page - 1); setPreview(null); }}>←</button>
              <span>{page + 1}/{pageCount}</span>
              <button type="button" aria-label="Later calendar weeks" disabled={page === pageCount - 1}
                onClick={() => { setCalendarPage(page + 1); setPreview(null); }}>→</button>
            </div>}
          </div>
        </div>
        <div className="ah-activity__calendar-scroll" ref={setCalendarElement}>
          <div className="ah-activity__calendar-layout" data-short-range={range !== "53w" ? "" : undefined} style={{
            "--activity-weeks": visibleCalendar.weeks.length,
            "--activity-footer-width": `${Math.max(0, calendarLayoutWidth - 30)}px`,
            width: calendarLayoutWidth,
          } as CSSProperties}>
            <div className="ah-activity__weekdays" aria-hidden="true">{WEEKDAYS.map(day => <span key={day}>{day}</span>)}</div>
            <CalendarHeatmap data={visibleCalendar} color={color}
              ariaLabel={`Daily token activity for ${name}. Use arrow keys to navigate days.`}
              summary={preview ? `${dateLabel(preview.id)} · ${tokenText(preview.value!)} tokens`
                : "Select a day"}
              selectedDayId={selectedDay?.id} onDaySelect={selectDay} onDayPreview={setPreview}
              dayMarkers={markers} />
          </div>
        </div>
        <div className="ah-activity__runs" aria-label="Activity runs in the 53-week history">
          {history.currentRun && <button type="button" className="ah-activity__run" data-run-link="current"
            title={`${dateLabel(history.currentRun.start, false, true)} – ${dateLabel(history.currentRun.end, false, true)}`}
            onClick={() => jumpToRun(history.currentRun!)}><i data-kind={currentIsBest ? "both" : "current"} />
            <span>{currentIsBest ? "Current & longest run" : "Current run"} <b>{history.currentRun.length} days</b></span>
            <small>{dateLabel(history.currentRun.start)} – {dateLabel(history.currentRun.end)}</small></button>}
          {history.bestRun && !currentIsBest && <button type="button" className="ah-activity__run" data-run-link="best"
            title={`${dateLabel(history.bestRun.start, false, true)} – ${dateLabel(history.bestRun.end, false, true)}`}
            onClick={() => jumpToRun(history.bestRun!)}><i data-kind="best" />
            <span>Longest run <b>{history.bestRun.length} days</b></span>
            <small>{dateLabel(history.bestRun.start)} – {dateLabel(history.bestRun.end)}</small></button>}
          {details.busiestDay && <button type="button" className="ah-activity__jump"
            onClick={() => selectDay(details.busiestDay!)}>Busiest day <span>{dateLabel(details.busiestDay.id)}</span></button>}
        </div>
      </section>
      <section className="ah-activity__day" aria-labelledby="activity-day-title" aria-busy={Boolean(selectedDay && usage.status === "loading")}>
        <div className="ah-activity__dayhead">
          <div>
            <h2 id="activity-day-title">{selectedDay ? dateLabel(selectedDay.id, true) : "No recorded days"}</h2>
            <p>{selectedDay ? `${tokenText(usage.status === "ready" && hourlyTotal > 0 ? hourlyTotal : selectedDay.value!)} tokens · hourly activity by model · local time`
              : "Activity will appear here when usage is recorded."}</p></div>
          <div className="ah-activity__daynav">
            <button type="button" aria-label="Previous recorded day" disabled={selectedIndex <= 0}
              onClick={() => selectDay(availableDays[selectedIndex - 1])}>←</button>
            <button type="button" aria-label="Next recorded day" disabled={selectedIndex < 0 || selectedIndex >= availableDays.length - 1}
              onClick={() => selectDay(availableDays[selectedIndex + 1])}>→</button>
          </div>
        </div>
        {!selectedDay ? <div className="ah-activity__empty">Choose a recorded day in the calendar to explore it.</div>
          : usage.status === "loading" ? <div className="ah-activity__empty" role="status">Loading hourly activity…</div>
          : usage.status === "error" ? <div className="ah-activity__empty" role="status">
            <strong>Hourly activity couldn’t be loaded</strong><span>Try loading this day again.</span>
            <button type="button" onClick={usage.retry}>Retry</button></div>
          : hourlyTotal === 0 ? <div className="ah-activity__empty">
            <strong>{selectedDay.value === 0 ? "No token activity on this day" : "Hourly breakdown unavailable"}</strong>
            <span>{selectedDay.value === 0 ? "Choose another day to see when and which models were used."
              : "The calendar has a daily total, but no hourly usage was returned for this day."}</span></div>
          : <><DayActivityChart key={selectedDay.id} rows={modelRows} modelColors={modelColors}
            lastHour={selectedDay.id === today ? new Date(currentTimestamp).getHours() : 23} />
            {hourlyTotal !== selectedDay.value && <p className="ah-activity__updated">Usage has changed since the calendar was loaded.</p>}</>}
      </section>
    </div>
  </DialogShell>;
}
