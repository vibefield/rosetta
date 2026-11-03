import { useId } from "react";
import { AgentMark, agentLabel } from "../lib/agents";
import { phaseHeading, readySessions, readySuffix, runtimeActivity } from "../lib/ingest-activity";
import type { IngestActivity } from "../lib/ingest-activity";
import { IngestionControls } from "./IngestionControls";

function ProgressMeter({ value, label, paused = false }: {
  readonly value: number | null; readonly label: string; readonly paused?: boolean;
}) {
  return <div className="ah-ingest-meter" role="progressbar" aria-label={label}
    aria-valuemin={0} aria-valuemax={100}
    aria-valuenow={value === null ? undefined : Math.floor(value * 100)}
    data-indeterminate={value === null ? "" : undefined} data-paused={paused ? "" : undefined}>
    <i style={value === null ? undefined : { width: `${value * 100}%` }} />
  </div>;
}

export function IngestStatus({ activity }: { readonly activity: IngestActivity | null }) {
  const titleId = useId();
  if (!activity) return null;
  const { phase, feed, footer, elapsed } = activity;
  const interrupted = feed.connectionError !== null;
  const ready = phase.key === "ready";
  const paused = interrupted || !phase.active;
  return (
    // `data-ready-count` is the count "· N ready" draws, carried (inert)
    // even where the copy leaves it out: zero, resumed, or another step.
    <section className="ah-ingest-status" aria-labelledby={titleId}
      data-state={interrupted ? "disconnected" : ready ? "complete" : phase.key}
      data-ready-count={readySessions(feed.ingest)}>
      <div className="ah-ingest-status__eyebrow">
        <span>{ready ? "ALL SET" : "IMPORTING HISTORY"}</span>
        <span>{ready ? "✓" : `Step ${phase.step} of 4`}</span>
      </div>
      <h3 id={titleId} aria-live="polite" aria-atomic="true">
        {interrupted ? "Progress updates paused" : phase.title}
        {/* The count moves with every tree the import opens: drawn, but kept
            out of what the live region announces (the meter names it). */}
        {!interrupted && phase.ready ? <span aria-hidden="true">{readySuffix(phase)}</span> : null}
      </h3>
      <p className="ah-ingest-status__detail">
        {interrupted ? "Waiting for the background service. Reconnecting automatically…" : phase.detail}
      </p>
      <div className="ah-ingest-status__amount">
        <span>{phase.amount}</span>
        {phase.fraction !== null && !ready && <b>{Math.floor(phase.fraction * 100)}%</b>}
      </div>
      <ProgressMeter key={phase.key} value={phase.fraction} label={phaseHeading(phase)} paused={paused} />
      <div className="ah-ingest-status__footer">
        <span title={feed.connectionError ?? (footer.includes("No new") ? "The service is connected; its work counters have not changed." : undefined)}>{footer}</span>
        <span>{elapsed}</span>
      </div>
      <IngestionControls run={feed.ingest?.run} />
      {!ready && (feed.ingest?.runtimes.length ?? 0) > 0 && <details className="ah-ingest-status__agents">
        <summary>Agent details <span aria-hidden="true">⌄</span></summary>
        <ul>
          {feed.ingest?.runtimes.map((runtime) => {
            const status = runtimeActivity(runtime, feed.ingest?.phase === "restoring");
            return <li key={runtime.kind}>
              <AgentMark kind={runtime.kind} />
              <div>
                <div className="ah-ingest-status__agent-name"><strong>{agentLabel(runtime.kind)}</strong>
                  {runtime.state === "done" && <span aria-label="Ready to browse">✓</span>}
                </div>
                <span className="ah-ingest-status__agent-caption">{status.label}</span>
                {runtime.state !== "done" && <ProgressMeter value={status.fraction}
                  label={`${agentLabel(runtime.kind)}: ${status.label}`} paused={paused || runtime.state === "queued"} />}
              </div>
            </li>;
          })}
        </ul>
      </details>}
    </section>
  );
}
