// §03 F — the delegate's child run, in place.
//
// A proven `child_session_id` opens under its own ledger line, the grammar
// one size down (13px), headed by the child's mark, name, role and range.
// At depth three, open the child's document at the column's full measure.
//
// It stays LAZY. The line it hangs from is closed until pressed, so a parent
// with nine delegations does not open nine websockets to draw nothing; the
// component mounts when the line opens, and that is the subscription.
import { useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { SessionRow } from "@vibefield/rosetta-protocol";
import { activityCuts } from "./activity-plan";
import { revealActivityTarget } from "./Activity";
import type { TimelineLayout } from "../../lib/transcript/use-transcript-tail";
import { clock } from "../../lib/format";
import { AgentMark, agentLabel } from "../../lib/agents";
import { useTimeline } from "../../lib/hooks";
import { ReadingSessionProvider, useRoom } from "../../room-context";
import type { AttachmentFact, PathFact } from "../../lib/transcript";
import { TranscriptDocument } from "./Document";
import { useTranscriptModel } from "./document-model";
import type { ThreadFocus, ThreadIndex } from "./thread-index";
import { messageDomId, sessionContains, subthreadTitle } from "./thread-index";
import { ThreadNavigationContext } from "./thread-navigation";

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
  readonly onOpenAttachment: (fact: AttachmentFact, row?: readonly AttachmentFact[]) => void;
  readonly onOpenPath: (path: PathFact) => void;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly homeDirectory: string | null;
}

export function Subthread(props: SubthreadProps) {
  const navigation = useContext(ThreadNavigationContext);
  const detached = navigation.depth >= 3 && navigation.open !== null;
  const onFocusPath = props.focus !== null && sessionContains(props.session.id, props.focus.sessionId, props.index.byId);
  useEffect(() => {
    if (detached && onFocusPath) navigation.open?.(props.session.id);
  }, [detached, onFocusPath, navigation.open, props.session.id]);
  if (detached) return <button type="button" className="tr-child-link" onClick={() => navigation.open?.(props.session.id)}>
    Open child transcript <span>{subthreadTitle(props.session)} ↗</span>
  </button>;
  return <EmbeddedSubthread {...props} />;
}

function EmbeddedSubthread({
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
  const { sessions } = useRoom();
  const feed = useTimeline(sessions, shown ? session.id : null);
  const cuts = useMemo(() => activityCuts(feed.rows, feed.watermark), [feed.rows, feed.watermark]);
  const model = useTranscriptModel(feed.rows, session, homeDirectory, Date.now(), { cutBefore: cuts.cutBefore });
  const [highlightedTimelineId, setHighlightedTimelineId] = useState<string | null>(null);
  const focusTries = useRef(0);
  const displayedLayout = useRef<TimelineLayout | null>(null);
  const onTimelineLayout = useCallback((layout: TimelineLayout) => { displayedLayout.current = layout; }, []);
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
      const element = displayedLayout.current?.elementForAnchor?.(targetTimelineId)
        ?? document.getElementById(messageDomId(session.id, drawnBy));
      if (element) {
        revealActivityTarget(element);
        setHighlightedTimelineId(element.dataset.timelineId ?? drawnBy);
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
  const range = session.activity
    ? `${clock(session.activity.first_at)} → ${clock(session.activity.last_at)}`
    : "Activity time unknown";

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
      <ReadingSessionProvider value={session.id}>
      <div className="tr">
        {!feed.snapshotReady && <p className="tr-quiet" role="status">Loading messages…</p>}
        {feed.snapshotReady && feed.earlier.kind !== "none" && <div className="tr-activity__loading" role="status">
          {feed.earlier.kind === "fetching" || feed.earlier.kind === "revealing" ? "Loading earlier messages…" : <>
            <span>{feed.earlier.kind === "failed" ? "Could not load earlier messages." : "Earlier messages are not loaded."}</span>
            <button type="button" onClick={feed.loadEarlier}>{feed.earlier.kind === "failed" ? "Retry" : "Load earlier messages"}</button>
          </>}
        </div>}
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
          onTimelineLayout={onTimelineLayout}
          nested
        />
      </div>
      </ReadingSessionProvider>
      )}
    </div>
  );
}
