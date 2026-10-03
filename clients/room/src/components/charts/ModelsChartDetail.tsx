import { useMemo } from "react";
import type { UsageResponse } from "@rosetta/protocol";
import { tokenText } from "../../lib/format";
import { createModelUsageStreamgraphData } from "../../lib/chart-data";
import { Streamgraph } from "./Streamgraph";
import type { StreamgraphDomainPointData } from "./types";
import "../../styles/models-chart.css";

const dateFormatter = new Intl.DateTimeFormat("en-US", {
  weekday: "short", month: "short", day: "numeric", year: "numeric", timeZone: "UTC",
});
function formatUsageDay(point: StreamgraphDomainPointData): string {
  return dateFormatter.format(new Date(`${point.id}T12:00:00Z`));
}

export interface ModelsChartDetailProps {
  readonly usage: UsageResponse | null;
  readonly currentTimestamp: number;
}

export function ModelsChartDetail({ usage, currentTimestamp }: ModelsChartDetailProps) {
  const data = useMemo(() => usage
    ? createModelUsageStreamgraphData(usage, currentTimestamp, { groupOthers: false })
    : { domain: [], series: [] },
    [usage, currentTimestamp]);

  return (
    <div className="ah-models" aria-busy={usage === null}>
      {usage === null ? <p className="ah-models__loading" role="status">Loading model usage…</p>
        : <Streamgraph data={data} expanded
          title="Tokens per day · 30 days"
          note="no zero · thickness is the total"
          ariaLabel="Inspect daily tokens by model over 30 days"
          emptyMessage="No model-attributed usage in this window"
          formatValue={tokenText} formatPoint={formatUsageDay} valueLabel="tokens" />}
    </div>
  );
}
