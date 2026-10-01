import { fmtCompactNumber } from "../../lib/format";
import { fmtUsd } from "../../lib/spend";

export interface SummaryMetricProps {
  readonly label: string;
  readonly value: number | null;
  /** Currency values arrive in micro-USD, as they do everywhere in the app. */
  readonly unit?: "number" | "usd";
  readonly estimated?: boolean;
  readonly lowerBound?: boolean;
  readonly unknown?: boolean;
  readonly emptyValue?: string;
  readonly title?: string;
}

/** Every summary figure shares the same digits, unit, and coverage slots. */
export function SummaryMetric({
  label, value, unit = "number", estimated = false, lowerBound = false,
  unknown = false, emptyValue = "—", title,
}: SummaryMetricProps) {
  const figure = unknown || value === null ? null
    : unit === "usd" ? fmtUsd(value) : fmtCompactNumber(value);
  const description = unknown ? `Loading ${label}…` : title;

  return (
    <div className="ah-stat" title={description} role="group" aria-label={description ?? label}>
      <b>
        {figure === null ? unknown ? "—" : emptyValue : `${estimated ? "~" : ""}${figure.n}`}
        {figure !== null && (figure.u || lowerBound) && (
          <small>{`${figure.u}${lowerBound ? "+" : ""}`}</small>
        )}
      </b>
      <span>{label}</span>
    </div>
  );
}
