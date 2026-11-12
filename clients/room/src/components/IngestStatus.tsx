import { useId, useLayoutEffect, useRef } from "react";
import type { ReactNode } from "react";
import { useHostInputs } from "../host-inputs";
import type { RoomWaitMark } from "../host-inputs";
import { AgentMark, agentLabel } from "../lib/agents";
import {
  PAUSED_UPDATES, phaseHeading, readableText, readyConversations, runtimeActivity,
} from "../lib/ingest-activity";
import type { ActivityFeed, ActivityPhase, IngestActivity } from "../lib/ingest-activity";
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
  const { waitMark } = useHostInputs();
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
  if (waitMark !== undefined) return <MarkedIngestStatus activity={activity} waitMark={waitMark} titleId={titleId} />;
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
      <StatusLive phase={phase} interrupted={interrupted} />
      <h3 id={titleId}>
        {interrupted ? PAUSED_UPDATES.title : phase.title}
        {!interrupted && phase.count ? <span className="ah-ingest-status__count">{phase.count}</span> : null}
      </h3>
      <p className="ah-ingest-status__detail">
        {interrupted ? PAUSED_UPDATES.detail : phase.detail}
      </p>
      <div className="ah-ingest-status__amount">
        <span>{phase.amount}</span>
        {phase.fraction !== null && !ready && <b>{Math.floor(phase.fraction * 100)}%</b>}
      </div>
      <ProgressMeter key={phase.key} value={phase.fraction} label={phaseHeading(phase)} paused={paused} />
      {horizon && <p className="ah-ingest-status__horizon" aria-hidden="true">{horizon.line}</p>}
      <StatusFooter feed={feed} footer={footer} elapsed={elapsed} />
      <IngestionControls run={feed.ingest?.run} />
      {/* Past publication (search, step 4) every agent is done: the card
          keeps to the search it waits for, and leaves the column room for
          the portfolio above it. */}
      {phase.step < 4 && (feed.ingest?.runtimes.length ?? 0) > 0 && <details className="ah-ingest-status__agents">
        <summary>Agent details <span aria-hidden="true">⌄</span></summary>
        <AgentList feed={feed} paused={paused} />
      </details>}
    </section>
  );
}

/** The counts move with every tree the import opens: drawn, never announced.
 * The live regions say the step and each recency span reached, nothing else. */
function StatusLive({ phase, interrupted }: { readonly phase: ActivityPhase; readonly interrupted: boolean }) {
  return <>
    <span className="ah-visually-hidden" aria-live="polite" aria-atomic="true">
      {interrupted ? PAUSED_UPDATES.title : phase.spoken ?? phase.title}
    </span>
    <span className="ah-visually-hidden" aria-live="polite" aria-atomic="true">
      {phase.horizon?.reached ?? ""}
    </span>
  </>;
}

/** The connection and the timer. */
function StatusFooter({ feed, footer, elapsed }: {
  readonly feed: ActivityFeed; readonly footer: string; readonly elapsed: string;
}) {
  return <div className="ah-ingest-status__footer">
    <span title={feed.connectionError ?? (footer.includes("No new") ? "The service is connected; its work counters have not changed." : undefined)}>{footer}</span>
    <span>{elapsed}</span>
  </div>;
}

/** Each agent's own step, under Agent details. */
function AgentList({ feed, paused }: { readonly feed: ActivityFeed; readonly paused: boolean }) {
  return <ul>
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
  </ul>;
}

/**
 * The card in the host's mark (R10): it keeps its place at the head of the
 * first column and wears the mark where IMPORTING HISTORY stood, the step
 * beside it. Its words keep to the step's title and its count; a fraction the
 * step knows is a colourless hairline, and a step that knows none draws no
 * bar, the mark being the sign of working. The detail, the amount, the
 * recency line, the connection and the timer fold under Agent details, which
 * stand through search as well. Paused updates stand the mark at rest.
 */
function MarkedIngestStatus({ activity, waitMark, titleId }: {
  readonly activity: IngestActivity;
  readonly waitMark: RoomWaitMark;
  readonly titleId: string;
}) {
  const { phase, feed, footer, elapsed } = activity;
  const interrupted = feed.connectionError !== null;
  const paused = interrupted || !phase.active;
  const count = readyConversations(feed);
  const horizon = interrupted ? null : phase.horizon ?? null;
  const runtimes = phase.step < 4 ? (feed.ingest?.runtimes.length ?? 0) : 0;
  const sub = interrupted ? PAUSED_UPDATES.detail
    : phase.count ? readableText(count) : phase.amount;
  return (
    <section className="ah-ingest-status" aria-labelledby={titleId} data-marked=""
      data-state={interrupted ? "disconnected" : phase.key}
      data-ready-count={count.readable} data-ready-total={count.total}>
      <div className="ah-ingest-status__eyebrow">
        <span className="ah-ingest-status__mark">{waitMark.mark("card", { resting: paused })}</span>
        <span>{`Step ${phase.step} of 4`}</span>
      </div>
      <StatusLive phase={phase} interrupted={interrupted} />
      <h3 id={titleId}>{interrupted ? PAUSED_UPDATES.title : phase.title}</h3>
      <p className="ah-ingest-status__sub">{sub}</p>
      {phase.fraction !== null && (
        <ProgressMeter key={phase.key} value={phase.fraction} label={phaseHeading(phase)} paused={paused} />
      )}
      <details className="ah-ingest-status__agents">
        <summary>Agent details <span aria-hidden="true">⌄</span></summary>
        {!interrupted && <p className="ah-ingest-status__detail">{phase.detail}</p>}
        <div className="ah-ingest-status__amount">
          <span>{phase.amount}</span>
          {phase.fraction !== null && <b>{Math.floor(phase.fraction * 100)}%</b>}
        </div>
        {horizon && <p className="ah-ingest-status__horizon" aria-hidden="true">{horizon.line}</p>}
        <StatusFooter feed={feed} footer={footer} elapsed={elapsed} />
        <IngestionControls run={feed.ingest?.run} />
        {runtimes > 0 && <AgentList feed={feed} paused={paused} />}
      </details>
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
