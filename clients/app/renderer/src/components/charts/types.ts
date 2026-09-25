/**
 * Presentation-ready chart data schemas.
 *
 * These types intentionally contain no protocol rows or application view
 * models. Callers translate domain data once, then the chart components only
 * decide how to render it.
 */

export interface CalendarHeatmapCellData {
  /** Stable key, normally an ISO calendar date. */
  readonly id: string;
  /** Non-negative magnitude. `null` means the period is not available yet. */
  readonly value: number | null;
  /** Human-readable description used for the native tooltip. */
  readonly label: string;
}

export interface CalendarHeatmapWeekData {
  /** Stable key for the column, normally the week's opening date. */
  readonly id: string;
  /** Optional axis label displayed above this column. */
  readonly label?: string;
  /** Ordered cells within the week. The app supplies Sunday through Saturday. */
  readonly days: readonly CalendarHeatmapCellData[];
}

export interface CalendarHeatmapData {
  readonly weeks: readonly CalendarHeatmapWeekData[];
}

export interface StreamgraphDomainPointData {
  /** Stable x-axis key, such as an ISO date. */
  readonly id: string;
  /** Optional visible tick label for this point. */
  readonly label?: string;
}

export interface StreamgraphSeriesData {
  /** Stable identifier, unique within the chart. */
  readonly id: string;
  readonly label: string;
  /** Any valid CSS color value. */
  readonly color: string;
  /**
   * Magnitudes aligned by index with `StreamgraphData.domain`.
   * Missing, non-finite, or negative values are rendered as zero.
   */
  readonly values: readonly number[];
}

export interface StreamgraphData {
  /** Ordered x-axis shared by every series. Point ids must be unique. */
  readonly domain: readonly StreamgraphDomainPointData[];
  /** Series values must use one consistent unit within a chart. */
  readonly series: readonly StreamgraphSeriesData[];
}

export interface TreemapItemData {
  /** Stable identifier, unique within the chart. */
  readonly id: string;
  readonly label: string;
  /** Non-negative magnitude used to calculate the item's area. */
  readonly value: number;
  /** Any valid CSS color value. */
  readonly color: string;
  /** The value is a lower bound (part of it could not be measured); the
   * chart marks it rather than presenting it as exact. */
  readonly partial?: boolean;
}

export interface TreemapData {
  readonly items: readonly TreemapItemData[];
}
