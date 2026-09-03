// §03 F — the delegate's child run, in place.
//
// A child session is part of the calling document, not a sibling to navigate
// to: a proven `child_session_id` opens the subthread under its own ledger
// line, the whole grammar one size down (13px) on the ledger's rail, headed by
// the child's mark, name, role and range.
//
// It stays LAZY. The line it hangs from is closed until pressed, so a parent
// with nine delegations does not open nine websockets to draw nothing; the
// component mounts when the line opens, and that is the subscription.
import { useEffect, useRef, useState } from "react";
import type { SessionRow } from "@rosetta/protocol";
import { clock } from "../../lib/format";
import { AgentMark, agentLabel } from "../../lib/agents";
import { useTimeline } from "../../lib/hooks";
import type { AttachmentFact, PathFact } from "../../lib/transcript";
import { TranscriptDocument } from "./Document";
import { useTranscriptModel } from "./document-model";
import type { ThreadFocus, ThreadIndex } from "./thread-index";
import { messageDomId, sessionContains, subthreadTitle } from "./thread-index";

export interface SubthreadProps {
  readonly session: SessionRow;
  /** The link was proven, so this run sits under its own line rather than
   * being interleaved by a start time nobody claimed causes anything. */
  readonly anchored: boolean;
  readonly duringBackfill: boolean;
  readonly index: ThreadIndex;
  readonly focus: ThreadFocus | null;
  readonly onFocusConsumed: () => void;
  readonly onReceipt: (text: string) => void;
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
  readonly onOpenPath: (path: PathFact) => void;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly homeDirectory: string | null;
}

export function Subthread({
  session,
  anchored,
  duringBackfill,
  index,
  focus,
  onFocusConsumed,
  onReceipt,
  onOpenAttachment,
  onOpenPath,
  getRuntimeKind,
  homeDirectory,
}: SubthreadProps) {
  const onFocusPath =
    focus !== null && sessionContains(session.id, focus.sessionId, index.byId);
  // Anchored runs were revealed by pressing their own delegate line, so they
  // open with it. A run placed only by its start time keeps a disclosure: a
  // parent with nine of them must not open nine subscriptions to draw nothing.
  const [open, setOpen] = useState(anchored || onFocusPath);
  const shown = open || onFocusPath;
  const feed = useTimeline(shown ? session.id : null);
  const model = useTranscriptModel(feed.rows, session, homeDirectory, Date.now());
  const [highlightedTimelineId, setHighlightedTimelineId] = useState<string | null>(null);
  const focusTries = useRef(0);
  const targetTimelineId = focus?.sessionId === session.id ? focus.timelineId : null;

  useEffect(() => {
    if (onFocusPath) setOpen(true);
  }, [onFocusPath]);

  useEffect(() => {
    focusTries.current = 0;
    // Consuming the focus nulls the target; clearing here would wipe the ring
    // in the frame it was drawn.
    if (targetTimelineId !== null) setHighlightedTimelineId(null);
  }, [targetTimelineId]);

  useEffect(() => {
    // The same rule the column keeps: a feed that has not delivered yet says
    // nothing is above it, and that is not evidence of anything.
    if (
      targetTimelineId === null ||
      (feed.status !== "live" && feed.status !== "resyncing")
    ) {
      return;
    }
    const drawnBy = model.representatives.get(targetTimelineId) ?? null;
    if (drawnBy !== null) {
      const element = document.getElementById(messageDomId(session.id, drawnBy));
      if (element) {
        element.scrollIntoView({ block: "center" });
        setHighlightedTimelineId(drawnBy);
        onFocusConsumed();
        window.setTimeout(() => setHighlightedTimelineId(null), 1500);
      }
      return;
    }
    // The anchor search still pages blindly upward — `loadEarlier` exists for
    // exactly this, and it is the one caller the deprecation keeps.
    if (feed.earlier.kind !== "none" && focusTries.current < 8) {
      focusTries.current += 1;
      feed.loadEarlier();
    } else if (feed.earlier.kind === "none") {
      onFocusConsumed();
      onReceipt("That subthread message is older than the loaded history");
    }
  }, [targetTimelineId, session.id, feed, model, onFocusConsumed, onReceipt]);

  const kind = getRuntimeKind(session.runtime_id);
  const range =
    session.updated_at > session.started_at
      ? `${clock(session.started_at)} → ${clock(session.updated_at)}`
      : clock(session.started_at);

  return (
    <div className="tr-sub" data-anchored={anchored || undefined}>
      <button
        type="button"
        className="tr-sub__head"
        aria-expanded={shown}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="mark">
          <AgentMark kind={kind} />
        </span>
        <span className="tr-sub__name" title={subthreadTitle(session)}>
          {subthreadTitle(session)}
          <span className="tr-sub__role">
            {" · "}
            {agentLabel(kind)}
            {session.role === "main" ? "" : ` · ${session.role}`}
          </span>
        </span>
        <span className="when">{range}</span>
      </button>
      {shown && (
      <div className="tr">
        <TranscriptDocument
          session={session}
          model={model}
          feed={feed}
          duringBackfill={duringBackfill}
          index={index}
          focus={onFocusPath ? focus : null}
          highlightedTimelineId={highlightedTimelineId}
          onFocusConsumed={onFocusConsumed}
          onReceipt={onReceipt}
          onOpenAttachment={onOpenAttachment}
          onOpenPath={onOpenPath}
          getRuntimeKind={getRuntimeKind}
          homeDirectory={homeDirectory}
          onPillPress={null}
          nested
        />
      </div>
      )}
    </div>
  );
}
