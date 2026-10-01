import { useId, useMemo, useState } from "react";
import type { CSSProperties } from "react";
import { createStreamgraphLayout } from "./streamgraph-layout";
import type { StreamgraphData, StreamgraphDomainPointData, StreamgraphSeriesData } from "./types";

const DEFAULT_VIEWBOX_WIDTH = 276;
const DEFAULT_VIEWBOX_HEIGHT = 60;

type SeriesColorStyle = CSSProperties & { "--series-color": string };

interface NormalizedSeries extends StreamgraphSeriesData {
  values: number[];
  total: number;
}

export interface StreamgraphProps {
  readonly data: StreamgraphData;
  readonly title: string;
  readonly ariaLabel: string;
  readonly note?: string;
  readonly emptyMessage?: string;
  readonly width?: number;
  readonly height?: number;
  /** Enlarges the presentation without changing the data or waveform. */
  readonly expanded?: boolean;
  readonly formatValue?: (value: number) => string;
  readonly formatPoint?: (point: StreamgraphDomainPointData) => string;
  readonly valueLabel?: string;
}

function finiteMagnitude(value: number | undefined): number {
  return Number.isFinite(value) ? Math.max(0, value ?? 0) : 0;
}

function defaultValueFormatter(value: number): string {
  return value.toLocaleString();
}

function defaultPointFormatter(point: StreamgraphDomainPointData): string {
  return point.label ?? point.id;
}

function formatShare(value: number, total: number): string {
  if (total <= 0) return "—";
  const share = value / total * 100;
  return value > 0 && share < 0.1 ? "<0.1%" : `${share.toFixed(1)}%`;
}

export function Streamgraph({
  data,
  title,
  ariaLabel,
  note,
  emptyMessage = "No data in this period",
  width = DEFAULT_VIEWBOX_WIDTH,
  height = DEFAULT_VIEWBOX_HEIGHT,
  expanded = false,
  formatValue = defaultValueFormatter,
  formatPoint = defaultPointFormatter,
  valueLabel = "value",
}: StreamgraphProps) {
  const gradientId = useId().replace(/:/g, "");
  const [hoveredPoint, setHoveredPoint] = useState<string | null>(null);
  const [focusedPoint, setFocusedPoint] = useState<string | null>(null);
  const viewBoxWidth = Number.isFinite(width) && width > 0 ? width : DEFAULT_VIEWBOX_WIDTH;
  const viewBoxHeight =
    Number.isFinite(height) && height > 0 ? height : DEFAULT_VIEWBOX_HEIGHT;
  const visibleSeries = useMemo<NormalizedSeries[]>(() => {
    return data.series
      .map((series) => {
        const values = data.domain.map((_, index) => finiteMagnitude(series.values[index]));
        return {
          ...series,
          values,
          total: values.reduce((total, value) => total + value, 0),
        };
      })
      .filter((series) => series.total > 0);
  }, [data]);

  const layerPaths = useMemo(
    () =>
      createStreamgraphLayout(visibleSeries, data.domain.length, {
        width: viewBoxWidth,
        height: viewBoxHeight,
      }),
    [data.domain.length, viewBoxHeight, viewBoxWidth, visibleSeries],
  );
  const seriesById = useMemo(
    () => new Map(visibleSeries.map((series) => [series.id, series])),
    [visibleSeries],
  );
  const hasData = layerPaths.length > 0;
  const pointIndices = useMemo(() => new Map(data.domain.map((point, index) => [point.id, index])), [data.domain]);
  const inspectedIndex = expanded && hasData
    ? pointIndices.get(hoveredPoint ?? "") ?? pointIndices.get(focusedPoint ?? "") ?? null
    : null;
  const inspectedPoint = inspectedIndex === null ? null : data.domain[inspectedIndex];
  const pointValues = inspectedIndex === null ? [] : visibleSeries.map(series => ({
    ...series, value: series.values[inspectedIndex],
  }));
  const pointTotal = pointValues.reduce((total, series) => total + series.value, 0);
  const inspectedX = inspectedIndex === null ? 0 : inspectedIndex / (data.domain.length - 1) * 100;
  const accessibleIndex = inspectedIndex ?? 0;
  const accessibleTotal = visibleSeries.reduce((total, series) => total + (series.values[accessibleIndex] ?? 0), 0);
  const accessibleValue = hasData ? `${formatPoint(data.domain[accessibleIndex])}: ${Math.round(accessibleTotal).toLocaleString()} ${valueLabel}. ${visibleSeries.map(series =>
    `${series.label}: ${Math.round(series.values[accessibleIndex]).toLocaleString()}`,
  ).join(", ")}` : undefined;
  const inspectPointer = (svg: SVGSVGElement, clientX: number) => {
    const rect = svg.getBoundingClientRect();
    if (rect.width <= 0) return;
    const index = Math.max(0, Math.min(data.domain.length - 1,
      Math.round((clientX - rect.left) / rect.width * (data.domain.length - 1))));
    setFocusedPoint(null);
    setHoveredPoint(data.domain[index].id);
  };

  return (
    <div className="ah-streamgraph" data-expanded={expanded ? "" : undefined}
      style={expanded ? { "--series-glow-strength": Math.min(1, 4 / visibleSeries.length) } as CSSProperties : undefined}>
      <div className="ah-chart-header">
        <p className="ah-chart-header__title">{title}</p>
        {note && <span className="ah-chart-header__note">{note}</span>}
      </div>

      {expanded && hasData && <p id={`${gradientId}-help`} className="ah-visually-hidden">
        Use left and right arrow keys to inspect each recorded point. Home and End select the first and last points.
        Band thickness represents {valueLabel}; vertical position does not represent a value.
      </p>}

      <div className="ah-streamgraph__plot" onPointerLeave={expanded ? () => setHoveredPoint(null) : undefined}>
        {hasData ? (
          <svg
            viewBox={`0 0 ${viewBoxWidth} ${viewBoxHeight}`}
            preserveAspectRatio="none"
            role={expanded ? "slider" : "img"}
            tabIndex={expanded ? 0 : undefined}
            aria-label={ariaLabel}
            aria-describedby={expanded ? `${gradientId}-help` : undefined}
            aria-orientation={expanded ? "horizontal" : undefined}
            aria-valuemin={expanded ? 0 : undefined}
            aria-valuemax={expanded ? data.domain.length - 1 : undefined}
            aria-valuenow={expanded ? accessibleIndex : undefined}
            aria-valuetext={expanded ? accessibleValue : undefined}
            data-inspected-point={inspectedPoint?.id}
            onPointerMove={expanded ? event => inspectPointer(event.currentTarget, event.clientX) : undefined}
            onPointerDown={expanded ? event => {
              // Pointer inspection should not move keyboard focus or pin its guide.
              event.preventDefault();
              inspectPointer(event.currentTarget, event.clientX);
            } : undefined}
            onFocus={expanded ? () => {
              setHoveredPoint(null);
              setFocusedPoint(data.domain[0].id);
            } : undefined}
            onBlur={expanded ? () => setFocusedPoint(null) : undefined}
            onKeyDown={expanded ? event => {
              if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
              event.preventDefault();
              const index = event.key === "Home" ? 0 : event.key === "End" ? data.domain.length - 1
                : Math.max(0, Math.min(data.domain.length - 1, accessibleIndex + (event.key === "ArrowLeft" ? -1 : 1)));
              setHoveredPoint(null);
              setFocusedPoint(data.domain[index].id);
            } : undefined}
          >
            {expanded && (
              <defs>
                {visibleSeries.map((series, index) => (
                  <linearGradient key={series.id} id={`${gradientId}-${index}`} x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0%" stopColor={series.color} stopOpacity="0.85" />
                    <stop offset="100%" stopColor={series.color} stopOpacity="0.25" />
                  </linearGradient>
                ))}
              </defs>
            )}
            {expanded && layerPaths.map(({ seriesId, path }) => (
              <path key={`glow-${seriesId}`} className="ah-streamgraph__glow" d={path}
                fill="none" stroke={seriesById.get(seriesId)?.color} strokeWidth="4"
                vectorEffect="non-scaling-stroke" aria-hidden="true" />
            ))}
            {layerPaths.map(({ seriesId, path }, index) => {
              const series = seriesById.get(seriesId);
              return series ? (
                <path key={seriesId} data-streamgraph-layer={seriesId} d={path}
                  fill={expanded ? `url(#${gradientId}-${index})` : series.color}>
                  {!expanded && <title>{series.label} · {formatValue(Math.round(series.total))}</title>}
                </path>
              ) : null;
            })}
            {expanded && layerPaths.map(({ seriesId, path }) => (
              <path key={`outline-${seriesId}`} className="ah-streamgraph__outline" d={path}
                fill="none" stroke={seriesById.get(seriesId)?.color} strokeWidth="1.25"
                vectorEffect="non-scaling-stroke" aria-hidden="true" />
            ))}
            {inspectedIndex !== null && <line className="ah-streamgraph__crosshair"
              x1={inspectedX / 100 * viewBoxWidth} x2={inspectedX / 100 * viewBoxWidth}
              y1="0" y2={viewBoxHeight} vectorEffect="non-scaling-stroke" aria-hidden="true" />}
          </svg>
        ) : (
          <p className="ah-streamgraph__empty">{emptyMessage}</p>
        )}
        {inspectedIndex !== null && inspectedPoint && <>
          {layerPaths.map(({ seriesId, samples }) => {
            const series = seriesById.get(seriesId)!;
            if (series.values[inspectedIndex] <= 0) return null;
            const [x, y] = samples[inspectedIndex];
            return <i key={seriesId} className="ah-streamgraph__point" data-streamgraph-point={seriesId}
              aria-hidden="true" style={{ left: `${x / viewBoxWidth * 100}%`, top: `${y / viewBoxHeight * 100}%`,
                "--series-color": series.color } as SeriesColorStyle} />;
          })}
          <div className="ah-streamgraph__readout" role="tooltip" tabIndex={-1} data-point={inspectedPoint.id}
            onPointerEnter={() => setHoveredPoint(inspectedPoint.id)}
            data-side={inspectedX > 50 ? "left" : "right"}
            style={{ "--inspect-x": `${inspectedX}%` } as CSSProperties}>
            <div className="ah-streamgraph__readout-head">
              <strong>{formatPoint(inspectedPoint)}</strong>
              <span>Total <b data-point-total={pointTotal}>{formatValue(Math.round(pointTotal))}</b> {valueLabel}</span>
            </div>
            <div className="ah-streamgraph__readout-rows" tabIndex={-1}>
              {pointValues.map(series => <div key={series.id} data-point-series={series.id}
                style={{ "--series-color": series.color } as SeriesColorStyle}>
                <span className="ah-streamgraph__readout-model"><i aria-hidden="true" />{series.label}</span>
                <b data-point-value={series.value} title={`${Math.round(series.value).toLocaleString()} ${valueLabel}`}>
                  {formatValue(Math.round(series.value))}
                </b>
                <small>{formatShare(series.value, pointTotal)}</small>
              </div>)}
            </div>
            {pointTotal === 0 && <p>No activity recorded</p>}
          </div>
        </>}
      </div>

      {hasData && (
        <>
          <div className="ah-streamgraph__axis" aria-hidden="true">
            {data.domain.map((point, pointIndex) =>
              point.label ? (
                <span
                  key={point.id}
                  data-first={pointIndex === 0 ? "" : undefined}
                  data-last={pointIndex === data.domain.length - 1 ? "" : undefined}
                  style={{
                    left: `${((pointIndex / (data.domain.length - 1)) * 100).toFixed(2)}%`,
                  }}
                >
                  {point.label}
                </span>
              ) : null,
            )}
          </div>
          <div className="ah-streamgraph__legend" tabIndex={expanded ? 0 : undefined}
            role={expanded ? "group" : undefined} aria-label={expanded ? "Model totals for the 30-day period" : undefined}>
            {visibleSeries.map((series) => (
              <span
                key={series.id}
                data-streamgraph-series={series.id}
                style={{ "--series-color": series.color } as SeriesColorStyle}
              >
                <i aria-hidden="true" />
                <em title={series.id}>{series.label}</em> <b title={Math.round(series.total).toLocaleString()}>{formatValue(Math.round(series.total))}</b>
              </span>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
