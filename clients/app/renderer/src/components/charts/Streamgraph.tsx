import { useMemo } from "react";
import type { CSSProperties } from "react";
import { createStreamgraphLayout } from "./streamgraph-layout";
import type { StreamgraphData, StreamgraphSeriesData } from "./types";

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
  readonly formatValue?: (value: number) => string;
}

function finiteMagnitude(value: number | undefined): number {
  return Number.isFinite(value) ? Math.max(0, value ?? 0) : 0;
}

function defaultValueFormatter(value: number): string {
  return value.toLocaleString();
}

export function Streamgraph({
  data,
  title,
  ariaLabel,
  note,
  emptyMessage = "No data in this period",
  width = DEFAULT_VIEWBOX_WIDTH,
  height = DEFAULT_VIEWBOX_HEIGHT,
  formatValue = defaultValueFormatter,
}: StreamgraphProps) {
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

  return (
    <div className="ah-streamgraph">
      <div className="ah-chart-header">
        <p className="ah-chart-header__title">{title}</p>
        {note && <span className="ah-chart-header__note">{note}</span>}
      </div>

      <div className="ah-streamgraph__plot">
        {hasData ? (
          <svg
            viewBox={`0 0 ${viewBoxWidth} ${viewBoxHeight}`}
            preserveAspectRatio="none"
            role="img"
            aria-label={ariaLabel}
          >
            {layerPaths.map(({ seriesId, path }) => {
              const series = seriesById.get(seriesId);
              return series ? (
                <path key={seriesId} d={path} fill={series.color} />
              ) : null;
            })}
          </svg>
        ) : (
          <p className="ah-streamgraph__empty">{emptyMessage}</p>
        )}
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
          <div className="ah-streamgraph__legend">
            {visibleSeries.map((series) => (
              <span
                key={series.id}
                style={{ "--series-color": series.color } as SeriesColorStyle}
              >
                <i aria-hidden="true" />
                {series.label} <b>{formatValue(Math.round(series.total))}</b>
              </span>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
