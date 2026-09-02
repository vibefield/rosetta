// The document — §03's anatomy, drawn.
//
// One prompt, one head, then the reply's segments in the order they happened.
// The turns are wrapped in sittings (§07), because a sticky pill's containing
// block is its parent and only a section per sitting makes the next pill push
// the previous one OUT instead of stacking on it.
//
// Everything the shipped column did keeps working here: the live watermark,
// the focus landing from a search hit, subthreads interleaved by timestamp for
// a parent-only link and anchored under their own delegate line when the link
// is proven, and the honest states for a session the daemon has not reached.
import { Fragment } from "react";
import type { ReactNode } from "react";
import type { SessionRow } from "@rosetta/protocol";
import type { LedgerLine, Segment } from "../../lib/transcript";
import { linkedChildSessionIds } from "../../lib/transcript";
import type { TimelineFeed } from "../../lib/hooks";
import { EmptyState } from "../ui";
import { Anchor } from "./Anchor";
import { DayPill, Sitting } from "./DayPill";
import type { TranscriptModel } from "./document-model";
import type { TurnPart } from "./document-plan";
import { Ledger } from "./Ledger";
import { GapMarker, Marker, Watermark } from "./Marker";
import { Prompt } from "./Prompt";
import { Prose, SegmentActs } from "./Prose";
import { ResponseHead } from "./ResponseHead";
import { Subthread } from "./Subthread";
import type { ThreadFocus, ThreadIndex } from "./thread-index";
import { messageDomId, sessionContains } from "./thread-index";
import { Thinking } from "./Thinking";

export interface TranscriptDocumentProps {
  readonly session: SessionRow;
  readonly model: TranscriptModel;
  readonly feed: TimelineFeed;
  /** The cold backfill is running: a streaming session is being INDEXED by the
   * daemon, not run by an agent — the watermark must not claim life. */
  readonly duringBackfill: boolean;
  readonly index: ThreadIndex;
  readonly focus: ThreadFocus | null;
  readonly highlightedTimelineId: string | null;
  readonly onFocusConsumed: () => void;
  readonly onReceipt: (text: string) => void;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly homeDirectory: string | null;
  /** The day list only exists where a menu can be drawn: the root column. */
  readonly onPillPress: ((element: HTMLElement, dayKey: string | null) => void) | null;
  /** The head of the document (§08), mounted above the first sitting. */
  readonly head?: ReactNode;
  readonly nested?: boolean;
}

export function TranscriptDocument({
  session,
  model,
  feed,
  duringBackfill,
  index,
  focus,
  highlightedTimelineId,
  onFocusConsumed,
  onReceipt,
  getRuntimeKind,
  homeDirectory,
  onPillPress,
  head,
  nested = false,
}: TranscriptDocumentProps) {
  const runtimeKind = getRuntimeKind(session.runtime_id);
  const live = session.stream_state === "streaming" && !duringBackfill;
  const focusTimelineId = focus?.sessionId === session.id ? focus.timelineId : null;
  const domIdOf = (timelineId: string) => messageDomId(session.id, timelineId);

  // Children this document owns. A proven `child_session_id` anchors the run
  // under its own line; everything else is interleaved by start time.
  const directChildren = index.childrenByParent.get(session.id) ?? [];
  const claimed = new Set<string>();
  const childrenByRow = new Map<string, SessionRow[]>();
  for (const turn of model.turns) {
    for (const segment of turn.segments) {
      if (segment.kind !== "ledger") continue;
      for (const line of segment.lines) {
        for (const row of line.rows) {
          for (const childId of linkedChildSessionIds(row)) {
            const child = index.byId.get(childId);
            if (child === undefined || claimed.has(child.id)) continue;
            if (child.parent_session_id !== session.id) continue;
            claimed.add(child.id);
            const key = line.rows[0].timeline_id;
            const existing = childrenByRow.get(key);
            if (existing) existing.push(child);
            else childrenByRow.set(key, [child]);
          }
        }
      }
    }
  }
  const floating = directChildren.filter((child) => !claimed.has(child.id));

  const renderChild = (child: SessionRow, anchored: boolean) => (
    <Subthread
      key={child.id}
      session={child}
      anchored={anchored}
      duringBackfill={duringBackfill}
      index={index}
      focus={focus}
      onFocusConsumed={onFocusConsumed}
      onReceipt={onReceipt}
      getRuntimeKind={getRuntimeKind}
      homeDirectory={homeDirectory}
    />
  );

  const renderSegment = (segment: Segment): ReactNode => {
    const rowId = segment.kind === "ledger" ? segment.rows[0].timeline_id : segment.row.timeline_id;
    const hit = highlightedTimelineId === rowId;
    switch (segment.kind) {
      case "ledger":
        return (
          <Ledger
            key={segment.key}
            lines={segment.lines}
            summary={segment.summary}
            domIdOf={domIdOf}
            highlightedTimelineId={highlightedTimelineId}
            focusTimelineId={focusTimelineId}
            alive={(line) =>
              live && (feed.watermark === null || line.rows[0].ordinal > feed.watermark)
            }
            hasChildren={(line) => (childrenByRow.get(line.rows[0].timeline_id) ?? []).length > 0}
            childFocused={(line) =>
              focus !== null &&
              (childrenByRow.get(line.rows[0].timeline_id) ?? []).some((child) =>
                sessionContains(child.id, focus.sessionId, index.byId),
              )
            }
            subthreadsFor={(line: LedgerLine) =>
              (childrenByRow.get(line.rows[0].timeline_id) ?? []).map((child) =>
                renderChild(child, true),
              )
            }
          />
        );
      case "thinking":
        return (
          <Thinking
            key={segment.key}
            text={segment.text}
            spanMs={segment.spanMs}
            domId={domIdOf(rowId)}
            timelineId={rowId}
            hit={hit}
            anchor={
              <Anchor
                at={segment.at}
                sessionId={session.id}
                timelineId={rowId}
                onReceipt={onReceipt}
              />
            }
          />
        );
      case "marker":
        return (
          <Marker
            key={segment.key}
            text={segment.text}
            tone={segment.tone}
            domId={domIdOf(rowId)}
            timelineId={rowId}
            hit={hit}
          />
        );
      case "summary":
        return (
          <div
            className="tr-summary"
            key={segment.key}
            id={domIdOf(rowId)}
            data-timeline-id={rowId}
            data-hit={hit || undefined}
          >
            <span className="eyebrow">{segment.purpose} summary</span>
            {segment.text}
          </div>
        );
      case "prose":
        return (
          <div
            className="tr-seg tr-prose ah-syntax"
            key={segment.key}
            id={domIdOf(rowId)}
            data-timeline-id={rowId}
            data-hit={hit || undefined}
          >
            <Anchor
              at={segment.at}
              sessionId={session.id}
              timelineId={rowId}
              onReceipt={onReceipt}
            />
            {segment.text !== null && <Prose text={segment.text} onReceipt={onReceipt} />}
            {segment.citationLabels.length > 0 && (
              <p className="tr-cites">
                {segment.citationLabels.map((label, at) => (
                  <span key={at}>{label}</span>
                ))}
              </p>
            )}
            <SegmentActs text={segment.text ?? ""} onReceipt={onReceipt} />
          </div>
        );
    }
  };

  // D33's line: the first thing drawn that arrived after the reader did.
  const ordinalOf = new Map(feed.rows.map((row) => [row.timeline_id, row.ordinal]));
  const watermarkKey = (() => {
    if (feed.watermark === null) return null;
    for (const { parts } of model.plan) {
      for (const part of parts) {
        const ordinal = part.firstRowId === null ? undefined : ordinalOf.get(part.firstRowId);
        if (ordinal !== undefined && ordinal > feed.watermark) return part.key;
      }
    }
    return null;
  })();

  const showPills = model.sittings.length > 1;
  let floatingAt = 0;
  const drainFloating = (until: number | null): ReactNode[] => {
    const out: ReactNode[] = [];
    while (
      floatingAt < floating.length &&
      (until === null || floating[floatingAt].started_at <= until)
    ) {
      out.push(renderChild(floating[floatingAt], false));
      floatingAt += 1;
    }
    return out;
  };

  const renderPart = (part: TurnPart) => {
    const gapAt = part.firstRowId === null ? undefined : model.gaps.get(part.firstRowId);
    return (
      <Fragment key={part.key}>
        {drainFloating(part.at)}
        {gapAt !== undefined && (
          <GapMarker
            count={gapAt}
            onPress={() =>
              onReceipt(
                `${gapAt} ${gapAt === 1 ? "message is" : "messages are"} in the session but not in this page`,
              )
            }
          />
        )}
        {watermarkKey === part.key && <Watermark text="live from here" />}
        <div className="tr-turn" data-timeline-id={part.firstRowId ?? undefined}>
          {part.prompts.length > 0 && (
            <Prompt
              prompts={part.prompts}
              sessionId={session.id}
              domIdOf={domIdOf}
              landedTimelineId={highlightedTimelineId}
              onReceipt={onReceipt}
            />
          )}
          {part.before.map(renderSegment)}
          {(part.showHead || part.after.length > 0) && (
            <div className="tr-resp">
              {part.showHead && part.turn.head !== null && (
                <ResponseHead
                  head={part.turn.head}
                  runtimeKind={runtimeKind}
                  open={live && part.turn.head.endedAt === null}
                />
              )}
              {part.after.map(renderSegment)}
            </div>
          )}
        </div>
      </Fragment>
    );
  };

  const empty = model.plan.length === 0 && directChildren.length === 0;
  return (
    <>
      {head}
      {model.plan.map(({ sitting, parts }) => (
        <Sitting key={sitting.key} sitting={sitting}>
          {showPills && (
            <DayPill
              sitting={sitting}
              onPress={(event) => onPillPress?.(event.currentTarget, sitting.dayKey)}
            />
          )}
          {parts.map(renderPart)}
        </Sitting>
      ))}
      {drainFloating(null)}
      {empty &&
        feed.status !== "connecting" &&
        (nested ? (
          <p className="tr-quiet">
            {session.stream_state === "indexed"
              ? "No readable messages in this subthread."
              : "This subthread has not been indexed yet."}
          </p>
        ) : session.stream_state !== "indexed" ? (
          <EmptyState
            title={
              session.stream_state === "streaming"
                ? "Being read right now"
                : "This transcript is in line"
            }
            description="rosettad is still reaching it — it opens the moment its turn comes."
          >
            <div className="ah-meter">
              <i style={{ width: "8%" }} />
            </div>
          </EmptyState>
        ) : (
          <EmptyState
            title="Nothing readable yet"
            description="This session has no readable timeline rows in the index."
          />
        ))}
      {session.stream_state === "streaming" && (
        <Watermark
          text={duringBackfill ? "indexing" : "live"}
          title={
            duringBackfill
              ? "rosettad is reading this transcript"
              : "this session is still running"
          }
        />
      )}
    </>
  );
}
