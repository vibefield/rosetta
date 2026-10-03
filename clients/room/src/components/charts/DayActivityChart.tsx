import { useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { ModelHour } from "@rosetta/protocol";
import { createDayActivity, createHourlyCurve, hourLabel } from "../../lib/day-activity";
import { tokenText } from "../../lib/format";

const hourX = (hour: number) => hour / 23 * 1000;

/** Smooth curves pass through the hourly samples on a shared token scale.
 * Dashed future guides carry no estimated usage or gradient fill. */
export function DayActivityChart({ rows, lastHour = 23, modelColors }: {
  readonly rows: readonly ModelHour[];
  readonly lastHour?: number;
  readonly modelColors?: Readonly<Record<string, string>>;
}) {
  const activity = useMemo(() => createDayActivity(rows, modelColors), [rows, modelColors]);
  const chartId = useId().replace(/:/g, "");
  const [hoverHour, setHoverHour] = useState<number | null>(null);
  const [focusedHour, setFocusedHour] = useState<number | null>(null);
  const [muted, setMuted] = useState<ReadonlySet<string>>(() => new Set());
  const bodyRef = useRef<HTMLDivElement>(null);
  const [legendHeight, setLegendHeight] = useState(400);
  useLayoutEffect(() => {
    const node = bodyRef.current;
    if (!node) return;
    const measure = () => setLegendHeight(node.clientHeight);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  const modelsPerPage = Math.max(1, Math.min(6, Math.floor((legendHeight - 46) / 28)));
  const [modelPage, setModelPage] = useState(0);
  const pageCount = Math.max(1, Math.ceil(activity.series.length / modelsPerPage));
  const page = Math.min(modelPage, pageCount - 1);
  const legendModels = activity.series.slice(page * modelsPerPage, (page + 1) * modelsPerPage);
  const futureHour = hoverHour !== null && hoverHour > lastHour ? hoverHour : null;
  const inspectedHour = futureHour === null ? hoverHour ?? focusedHour : null;
  const rawPeak = Math.max(1, ...activity.series.flatMap(model => model.values.slice(0, lastHour + 1)));
  const power = 10 ** Math.floor(Math.log10(rawPeak));
  const maximum = Math.ceil(rawPeak / power / 2) * power * 2;
  const y = (tokens: number) => 220 - tokens / maximum * 220;
  const paths = useMemo(() => activity.series.map((model, index) => {
    const points = model.values.slice(0, lastHour + 1).map((tokens, hour) =>
      [hourX(hour), 220 - tokens / maximum * 220] as const);
    const line = createHourlyCurve(points);
    return { ...model, line, gradientId: `${chartId}-model-${index}`,
      area: `${line} L${hourX(lastHour)},220 L0,220 Z`,
      future: `M${hourX(lastHour)},${points.at(-1)![1]} H1000` };
  }), [activity.series, lastHour, maximum, chartId]);
  const visibleSeries = paths.filter(model => !muted.has(model.id));

  return <div className="ah-daychart">
    <div className="ah-daychart__readout" aria-live="polite" aria-atomic="true">
      {futureHour !== null ? <><strong>{hourLabel(futureHour)} – {hourLabel((futureHour + 1) % 24)}</strong>
        <span>Awaiting usage</span></> : inspectedHour !== null ? <><strong>{hourLabel(inspectedHour)} – {hourLabel((inspectedHour + 1) % 24)}</strong>
        <span title="Tokens across all models">{tokenText(activity.hours[inspectedHour]!)} tokens</span></>
        : <><strong>{activity.peakHour === null ? "No token activity" : `Busiest hour · ${hourLabel(activity.peakHour)}`}</strong>
          <span title="Tokens across all models">{activity.peakHour === null ? "" : `${tokenText(activity.hours[activity.peakHour]!)} tokens`}</span></>}
    </div>
    <div className="ah-daychart__body" ref={bodyRef}>
      <div className="ah-daychart__graph">
        <span className="ah-daychart__unit" title="Includes input, output and cache tokens. Usage reported only as session totals appears at its report hour.">Tokens / hour</span>
        <div className="ah-daychart__plot">
          {[0, 1, 2, 3, 4].map(tick => <div key={tick} className="ah-daychart__gridline"
            style={{ top: `${tick * 25}%` }}><span>{tokenText(maximum * (4 - tick) / 4)}</span></div>)}
          {lastHour < 23 && <div className="ah-daychart__future" style={{ left: `${(lastHour + .5) / 23 * 100}%` }} />}
          {activity.peakHour !== null && inspectedHour === null && <i className="ah-daychart__peak"
            style={{ left: `${activity.peakHour / 23 * 100}%` }} aria-hidden="true" />}
          <svg viewBox="0 0 1000 220" preserveAspectRatio="none" tabIndex={0}
            role="slider" aria-label="Inspect hourly model usage. Use left and right arrow keys."
            aria-valuemin={0} aria-valuemax={lastHour} aria-valuenow={inspectedHour ?? 0}
            aria-valuetext={`${hourLabel(inspectedHour ?? 0)}: ${tokenText(activity.hours[inspectedHour ?? 0]!)} tokens`}
            data-hour={inspectedHour ?? undefined}
            data-future-hour={futureHour ?? undefined}
            onMouseMove={event => {
              const rect = event.currentTarget.getBoundingClientRect();
              const hour = Math.max(0, Math.min(23, Math.round((event.clientX - rect.left) / rect.width * 23)));
              setHoverHour(hour);
            }} onMouseLeave={() => setHoverHour(null)} onFocus={() => setFocusedHour(0)}
            onBlur={() => setFocusedHour(null)} onKeyDown={event => {
              if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
              event.preventDefault(); setHoverHour(null);
              setFocusedHour(hour => event.key === "Home" ? 0 : event.key === "End" ? lastHour
                : Math.max(0, Math.min(lastHour, (hour ?? 0) + (event.key === "ArrowLeft" ? -1 : 1))));
            }}>
            <title>Tokens by model for each hour of the selected day</title>
            <defs>{paths.map(model => <linearGradient key={model.id} id={model.gradientId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor={model.color} stopOpacity=".24" />
              <stop offset="55%" stopColor={model.color} stopOpacity=".09" />
              <stop offset="100%" stopColor={model.color} stopOpacity="0" />
            </linearGradient>)}</defs>
            {lastHour > 0 && visibleSeries.map(model => <path key={model.id} data-model-area={model.id}
              className="ah-daychart__area" d={model.area} fill={`url(#${model.gradientId})`} />)}
            {visibleSeries.map(model => <path key={model.id} className="ah-daychart__glow"
              d={model.line} fill="none" stroke={model.color} strokeWidth="6" vectorEffect="non-scaling-stroke" />)}
            {visibleSeries.map(model => <path key={model.id} data-model-line={model.id}
              className="ah-daychart__line" d={model.line}
              fill="none" stroke={model.color} strokeWidth="2.25" vectorEffect="non-scaling-stroke" />)}
            {lastHour < 23 && visibleSeries.map(model => <path key={model.id} data-model-future={model.id}
              className="ah-daychart__pending" d={model.future} fill="none" stroke={model.color}
              strokeWidth="1.5" vectorEffect="non-scaling-stroke" aria-hidden="true" />)}
            {(inspectedHour !== null || futureHour !== null) && <line
              x1={hourX(inspectedHour ?? futureHour!)} x2={hourX(inspectedHour ?? futureHour!)} y1="0" y2="220"
              className="ah-daychart__crosshair" vectorEffect="non-scaling-stroke" aria-hidden="true" />}
          </svg>
          {(inspectedHour !== null || lastHour < 23 && futureHour === null) && visibleSeries.map(model => {
            const hour = inspectedHour ?? lastHour;
            return <i key={model.id} className="ah-daychart__point" aria-hidden="true" style={{
              left: `${hour / 23 * 100}%`, top: `${y(model.values[hour]!) / 220 * 100}%`,
              "--model-color": model.color,
            } as CSSProperties} />;
          })}
          {lastHour < 23 && <span className="ah-daychart__future-label">Later today · awaiting usage</span>}
          <div className="ah-daychart__axis" aria-hidden="true">{[0, 6, 12, 18, 23].map(hour =>
            <span key={hour} style={{ left: `${hour / 23 * 100}%` }}>{hourLabel(hour)}</span>)}</div>
        </div>
      </div>
      <div className="ah-daychart__models">
        <div className="ah-daychart__legendhead"><span title="Model share of the selected day's tokens. Click a model to show or hide its line.">Models · day share</span>
          <span>{futureHour !== null ? hourLabel(futureHour) : inspectedHour === null ? "Tokens" : hourLabel(inspectedHour)}</span></div>
        {legendModels.map(model => <button type="button" key={model.id} data-model-toggle={model.id}
          aria-pressed={!muted.has(model.id)} title={`${model.label}: ${model.total.toLocaleString()} tokens. Click to ${muted.has(model.id) ? "show" : "hide"} its line.`}
          style={{ "--model-color": model.color } as CSSProperties} onClick={() => setMuted(current => {
            const next = new Set(current);
            if (next.has(model.id)) next.delete(model.id); else next.add(model.id);
            return next;
          })}>
          <span className="ah-daychart__modelname"><i /><span>{model.label}</span></span>
          <b>{futureHour !== null ? "—" : tokenText(inspectedHour === null ? model.total : model.values[inspectedHour]!)}</b>
          <span className="ah-daychart__share"><i style={{ width: `${model.total / activity.total * 100}%` }} /></span>
          <small>{(model.total / activity.total * 100).toFixed(0)}%</small>
        </button>)}
        {pageCount > 1 && <div className="ah-daychart__modelnav" aria-label="Model pages">
          <button type="button" aria-label="Previous models" disabled={page === 0} onClick={() => setModelPage(page - 1)}>←</button>
          <span>{page + 1}/{pageCount}</span>
          <button type="button" aria-label="Next models" disabled={page === pageCount - 1} onClick={() => setModelPage(page + 1)}>→</button>
        </div>}
      </div>
    </div>
  </div>;
}
