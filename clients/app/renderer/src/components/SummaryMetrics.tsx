import { tokenText } from "../lib/format";
import { coverage, spendDescription } from "../lib/spend";
import type { Spend } from "../lib/spend";
import { SummaryMetric } from "./ui/SummaryMetric";

interface SummaryMetricsProps {
  readonly sessionCount: number;
  readonly messageCount: number;
  readonly tokenCount: number;
  readonly spend: Spend;
  readonly pending: boolean;
  /** Counts are lifetime totals; tokens and spend use this panel's window. */
  readonly period: string;
}

/** Portfolio and project summaries use one row and one value renderer. */
export function SummaryMetrics({
  sessionCount, messageCount, tokenCount, spend, pending, period,
}: SummaryMetricsProps) {
  const state = coverage(spend);
  return (
    <div className="ah-stats">
      <SummaryMetric label="sessions" value={sessionCount} unknown={pending && sessionCount === 0}
        title={`${sessionCount.toLocaleString()} sessions · all time`} />
      <SummaryMetric label="messages" value={messageCount} unknown={pending && messageCount === 0}
        title={`${messageCount.toLocaleString()} messages · all time`} />
      <SummaryMetric label="tokens" value={tokenCount} unknown={pending && tokenCount === 0}
        title={`${tokenCount.toLocaleString()} tokens · ${period}`} />
      <SummaryMetric label="estimated cost" value={spend.costMicroUsd} unit="usd" estimated
        lowerBound={state === "partial"} emptyValue={state === "unavailable" ? "n/p" : "—"}
        unknown={pending && tokenCount === 0}
        title={`Estimated cost · ${spendDescription(spend, tokenText)}${state === "partial" ? " · lower bound" : ""} · ${period}`} />
    </div>
  );
}
