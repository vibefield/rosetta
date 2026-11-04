import { useId, useLayoutEffect, useRef } from "react";
import type { ReactNode } from "react";
import { AgentMark, agentLabel } from "../lib/agents";
import { phaseHeading, readyConversations, runtimeActivity } from "../lib/ingest-activity";
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
  const count = readyConversations(feed);
  const horizon = interrupted ? null : phase.horizon ?? null;
  // The end collapses in place to one line, which the column then folds away.
  if (ready && !interrupted) return (
    <section className="ah-ingest-status" aria-labelledby={titleId} data-state="complete" data-compact=""
      data-ready-count={count.readable} data-ready-total={count.total}>
      <span className="ah-visually-hidden" aria-live="polite" aria-atomic="true">{phase.title}</span>
      <h3 id={titleId}>{phase.title} <span aria-hidden="true">✓</span></h3>
    </section>
  );
  return (
    // `data-ready-count` / `data-ready-total` are the readable conversations
    // and their total, carried (inert) even where the copy leaves them out.
    <section className="ah-ingest-status" aria-labelledby={titleId}
      data-state={interrupted ? "disconnected" : ready ? "complete" : phase.key}
      data-ready-count={count.readable} data-ready-total={count.total}>
      <div className="ah-ingest-status__eyebrow">
        <span>{ready ? "ALL SET" : "IMPORTING HISTORY"}</span>
        <span>{ready ? "✓" : `Step ${phase.step} of 4`}</span>
      </div>
      {/* The counts move with every tree the import opens: drawn, never
          announced. The live regions say the step and each recency span
          reached, nothing else. */}
      <span className="ah-visually-hidden" aria-live="polite" aria-atomic="true">
        {interrupted ? "Progress updates paused" : phase.spoken ?? phase.title}
      </span>
      <span className="ah-visually-hidden" aria-live="polite" aria-atomic="true">
        {phase.horizon?.reached ?? ""}
      </span>
      <h3 id={titleId}>
        {interrupted ? "Progress updates paused" : phase.title}
        {!interrupted && phase.count ? <span className="ah-ingest-status__count">{phase.count}</span> : null}
      </h3>
      <p className="ah-ingest-status__detail">
        {interrupted ? "Waiting for the background service. Reconnecting automatically…" : phase.detail}
      </p>
      <div className="ah-ingest-status__amount">
        <span>{phase.amount}</span>
        {phase.fraction !== null && !ready && <b>{Math.floor(phase.fraction * 100)}%</b>}
      </div>
      <ProgressMeter key={phase.key} value={phase.fraction} label={phaseHeading(phase)} paused={paused} />
      {horizon && <p className="ah-ingest-status__horizon" aria-hidden="true">{horizon.line}</p>}
      <div className="ah-ingest-status__footer">
        <span title={feed.connectionError ?? (footer.includes("No new") ? "The service is connected; its work counters have not changed." : undefined)}>{footer}</span>
        <span>{elapsed}</span>
      </div>
      <IngestionControls run={feed.ingest?.run} />
      {/* Past publication (search, step 4) every agent is done: the card
          keeps to the search it waits for, and leaves the column room for
          the portfolio above it. */}
      {phase.step < 4 && (feed.ingest?.runtimes.length ?? 0) > 0 && <details className="ah-ingest-status__agents">
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

/**
 * The card's place in the projects column, whose height follows the card's
 * and eases when the end changes it: ALL SET's collapse to one line, then
 * its fold to nothing (`leaving`), so the rows below glide rather than jump.
 * Before the end the height follows at once. Reduced motion jumps
 * (ingest.css). The height is written straight to the element from a layout
 * effect and a ResizeObserver, so it never trails the card by a frame.
 */
export function IngestStatusFold({ activity, children }: {
  readonly activity: IngestActivity | null;
  readonly children: ReactNode;
}) {
  const outer = useRef<HTMLDivElement>(null);
  const inner = useRef<HTMLDivElement>(null);
  const ending = activity?.phase.key === "ready";
  const open = activity !== null && activity.leaving !== true;
  useLayoutEffect(() => {
    const box = outer.current;
    const content = inner.current;
    if (box === null || content === null) return;
    const fit = () => { box.style.height = open ? `${content.offsetHeight}px` : "0px"; };
    fit();
    const observer = new ResizeObserver(fit);
    observer.observe(content);
    return () => observer.disconnect();
  }, [open]);
  if (activity === null) return null;
  return (
    <div ref={outer} className="ah-ingest-fold" data-ending={ending ? "" : undefined}
      data-leaving={open ? undefined : ""}>
      <div ref={inner} className="ah-ingest-fold__content">{children}</div>
    </div>
  );
}
