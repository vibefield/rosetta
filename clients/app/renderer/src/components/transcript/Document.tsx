// The document — §03's anatomy, drawn.
//
// Primary speech stays visible; adjacent secondary events share one activity
// fold (§04B), after ownership and team chronology. Blocks retain their
// sittings (§07), because a sticky pill's containing
// block is its parent and only a section per sitting makes the next pill push
// the previous one OUT instead of stacking on it.
//
// Everything the shipped column did keeps working here: the live watermark,
// the focus landing from a search hit, subthreads interleaved by timestamp for
// a parent-only link and anchored under their own delegate line when the link
// is proven, and the honest states for a session the daemon has not reached.
import { Fragment, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { ComponentProps, ReactNode } from "react";
import type { ParticipantColor, SessionRow, TimelineEntry } from "@rosetta/protocol";
import type {
  AttachmentFact,
  LedgerLine,
  PathFact,
  RuntimeProfile,
  Segment,
} from "../../lib/transcript";
import {
  collaborationStamp,
  linkedChildSessionIds,
  participantPresentation,
  runtimeProfile,
  runSummary,
  standaloneWorkerSessions,
  taskBoundedReportWork,
  teamMemberPresentations,
  teamChronology,
  teamParticipants,
  teamSessionIds,
  workerThreadLinks,
  workflowAttemptIndex,
  workflowAttemptsFor,
} from "../../lib/transcript";
import type { TimelineFeed } from "../../lib/hooks";
import type { TimelineLayout } from "../../lib/transcript/use-transcript-tail";
import { useTeamTimelines } from "../../lib/hooks";
import { client } from "../../lib/rosetta";
import { lastActive } from "../../lib/format";
import { sessionLastActive } from "../../lib/session-activity";
import { EmptyState } from "../ui";
import { Anchor } from "./Anchor";
import { DayPill, Sitting } from "./DayPill";
import { transcriptModel } from "./document-model";
import type { TranscriptModel } from "./document-model";
import { activityCuts, activityPlan, activityRows } from "./activity-plan";
import type { ActivityItem, ReadingBlock } from "./activity-plan";
import { ActivityGroup, ContextEvent, revealActivityTarget } from "./Activity";
import { DisclosureState } from "./disclosure-state";
import { ThreadNavigationContext } from "./thread-navigation";
import { Ledger } from "./Ledger";
import { GapMarker, Marker, Watermark } from "./Marker";
import { Compaction } from "./Compaction";
import { Prompt } from "./Prompt";
import { SegmentActs } from "./Prose";
import { AuthoredMessage } from "./AuthoredMessage";
import { ResponseHead } from "./ResponseHead";
import { Subthread } from "./Subthread";
import type { ThreadFocus, ThreadIndex } from "./thread-index";
import { messageDomId, sessionContains } from "./thread-index";
import { ThoughtLine } from "./Thinking";
import type { TeamReportWorkPresentation } from "./Team";
import { KnownTeamRoster, TeamEvent, TeamWorkthread } from "./Team";

/** Mounted only by an open work disclosure; ordinary team traffic carries no private work. */
function LazyReportWork({ report, ...props }: {
  report: TimelineEntry;
} & Omit<ComponentProps<typeof ReportWorkDocument>, "rows" | "startedAt" | "endedAt">) {
  const [rows, setRows] = useState<TimelineEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    setRows(null);
    setError(null);
    void client.reportWork(report.session_id, report.timeline_id, controller.signal).then(
      (rows) => { if (!controller.signal.aborted) setRows(rows); },
      (error: unknown) => { if (!controller.signal.aborted) setError(error instanceof Error ? error.message : "The recorded work is unavailable"); },
    );
    return () => controller.abort();
  }, [report]);
  if (error !== null) return <p className="tr-quiet">{error}</p>;
  if (rows === null) return <p className="tr-quiet">Loading recorded work…</p>;
  const work = taskBoundedReportWork(report, rows);
  if (work === null) return <p className="tr-quiet">The recorded work is no longer available.</p>;
  return (
    <>
      <p className="tr-quiet">{work.actionCount} {work.actionCount === 1 ? "action" : "actions"}</p>
      <ReportWorkDocument {...props} rows={work.rows} startedAt={work.startedAt} endedAt={work.endedAt} />
    </>
  );
}

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
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
  readonly onOpenPath: (path: PathFact) => void;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly homeDirectory: string | null;
  /** The day list only exists where a menu can be drawn: the root column. */
  readonly onPillPress: ((element: HTMLElement, dayKey: string | null) => void) | null;
  /** The head of the document (§08), mounted above the first sitting. */
  readonly head?: ReactNode;
  readonly nested?: boolean;
  /** Root scroll surface observes the actual rendered team chronology. */
  readonly onTimelineLayout?: (layout: TimelineLayout) => void;
}

interface LegacyWorkflowChildIndex {
  readonly key: string;
  readonly ids: ReadonlySet<string>;
  readonly complete: boolean;
}

function staticTimelineFeed(rows: readonly TimelineEntry[]): TimelineFeed {
  const nothing = () => undefined;
  return {
    rows: [...rows],
    snapshotReady: true,
    timelineMeta: null,
    status: "live",
    watermark: null,
    earlier: { kind: "none" },
    earlierNotice: null,
    earlierAbove: 0,
    landed: null,
    holdEarlier: nothing,
    revealEarlier: nothing,
    retryEarlier: nothing,
    dismissEarlierNotice: nothing,
    canLoadEarlier: false,
    loadEarlier: nothing,
    loadAnchor: async () => false,
  };
}

function ReportWorkDocument({
  session,
  rows,
  name,
  color,
  runtime,
  startedAt,
  endedAt,
  index,
  onReceipt,
  onOpenAttachment,
  onOpenPath,
  getRuntimeKind,
  homeDirectory,
}: {
  readonly session: SessionRow;
  readonly rows: readonly TimelineEntry[];
  readonly name: string | null;
  readonly color: ParticipantColor | null;
  readonly runtime: RuntimeProfile;
  readonly startedAt: number | null;
  readonly endedAt: number | null;
  readonly index: ThreadIndex;
  readonly onReceipt: (text: string) => void;
  readonly onOpenAttachment: (fact: AttachmentFact) => void;
  readonly onOpenPath: (path: PathFact) => void;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly homeDirectory: string | null;
}) {
  const boundedSession = useMemo(
    () => ({ ...session, stream_state: "indexed" as const }),
    [session],
  );
  const feed = useMemo(() => staticTimelineFeed(rows), [rows]);
  const model = useMemo(
    () => transcriptModel(rows, boundedSession, homeDirectory, Date.now()),
    [rows, boundedSession, homeDirectory],
  );
  const scopedIndex = useMemo<ThreadIndex>(() => {
    const linked = new Set(rows.flatMap((row) => linkedChildSessionIds(row)));
    const childrenByParent = new Map(index.childrenByParent);
    childrenByParent.set(
      session.id,
      (index.childrenByParent.get(session.id) ?? []).filter((child) => linked.has(child.id)),
    );
    return { byId: index.byId, childrenByParent };
  }, [rows, session.id, index]);

  return (
    <TeamWorkthread
      name={name}
      color={color}
      runtime={runtime}
      startedAt={startedAt}
      endedAt={endedAt}
    >
      <TranscriptDocument
        session={boundedSession}
        model={model}
        feed={feed}
        duringBackfill={false}
        index={scopedIndex}
        focus={null}
        highlightedTimelineId={null}
        onFocusConsumed={() => undefined}
        onReceipt={onReceipt}
        onOpenAttachment={onOpenAttachment}
        onOpenPath={onOpenPath}
        getRuntimeKind={getRuntimeKind}
        homeDirectory={homeDirectory}
        onPillPress={null}
        nested
      />
    </TeamWorkthread>
  );
}

export function TranscriptDocument({
  session,
  model: baseModel,
  feed,
  duringBackfill,
  index,
  focus,
  highlightedTimelineId: parentHighlightedTimelineId,
  onFocusConsumed,
  onReceipt,
  onOpenAttachment,
  onOpenPath,
  getRuntimeKind,
  homeDirectory,
  onPillPress,
  head,
  nested = false,
  onTimelineLayout,
}: TranscriptDocumentProps) {
  const navigation = useContext(ThreadNavigationContext);
  const childNavigation = useMemo(() => ({ ...navigation, depth: navigation.depth + 1 }), [navigation]);
  const runtimeKind = getRuntimeKind(session.runtime_id);
  // Stream state describes indexing. Only newly arriving conversation rows
  // can receive an activity accent, and it expires with recorded activity.
  const recentActivity = !duringBackfill &&
    lastActive(sessionLastActive(session), Date.now()) === "now";
  const directChildren = index.childrenByParent.get(session.id) ?? [];
  const teamIds = useMemo(
    () => teamSessionIds(session.id, directChildren, feed.rows),
    [session.id, directChildren, feed.rows],
  );
  const teamIdSet = useMemo(() => new Set(teamIds), [teamIds]);
  const teamFeed = useTeamTimelines(teamIds);
  const chronology = useMemo(
    () => teamChronology(feed.rows, teamFeed.rowsBySession),
    [feed.rows, teamFeed.rowsBySession],
  );
  const displayedRows = chronology.rows;
  const cuts = useMemo(() => activityCuts(feed.rows, feed.watermark, displayedRows), [feed.rows, feed.watermark, displayedRows]);
  const model = useMemo(
    () => {
        const derived =
          displayedRows === feed.rows
            ? baseModel
            : transcriptModel(displayedRows, session, homeDirectory, Date.now(), {
                sharedCollaborationSessionIds: teamIdSet,
                cutBefore: cuts.cutBefore,
              });
        if (chronology.duplicateRepresentatives.size === 0) return { ...derived, gaps: cuts.gaps };
        return {
          ...derived,
          gaps: cuts.gaps,
          representatives: new Map([
            ...derived.representatives,
            ...chronology.duplicateRepresentatives,
          ]),
        };
    },
    [
      displayedRows,
      feed.rows,
      baseModel,
      session,
      homeDirectory,
      teamIdSet,
      chronology.duplicateRepresentatives,
      cuts,
    ],
  );
  const reading = useMemo(() => activityPlan(model.plan, {
    gaps: cuts.gaps, watermarkId: cuts.watermarkId, partial: !feed.snapshotReady || feed.earlier.kind !== "none",
  }), [model.plan, cuts, feed.snapshotReady, feed.earlier.kind]);
  const workflowAttempts = useMemo(() => workflowAttemptIndex(displayedRows), [displayedRows]);
  const teamMembers = useMemo(
    () =>
      teamMemberPresentations(
        feed.rows,
        index.byId,
        teamFeed.rowsBySession,
        teamFeed.responseRuntimesBySession,
        feed.snapshotReady && feed.earlier.kind === "none",
      ),
    [
      feed.rows,
      feed.snapshotReady,
      feed.earlier.kind,
      index.byId,
      teamFeed.rowsBySession,
      teamFeed.responseRuntimesBySession,
    ],
  );
  const knownTeamSessions = useMemo(
    () => directChildren.filter((child) => teamIdSet.has(child.id)),
    [directChildren, teamIdSet],
  );
  const participants = useMemo(
    () =>
      teamParticipants(
        teamMembers,
        knownTeamSessions,
        teamFeed.rowsBySession,
        teamFeed.responseRuntimesBySession,
      ),
    [
      teamMembers,
      knownTeamSessions,
      teamFeed.rowsBySession,
      teamFeed.responseRuntimesBySession,
    ],
  );
  const sessionByTimelineId = useMemo(
    () => new Map(displayedRows.map((row) => [row.timeline_id, row.session_id])),
    [displayedRows],
  );
  const focusTimelineId =
    focus !== null && (focus.sessionId === session.id || teamIdSet.has(focus.sessionId))
      ? (chronology.duplicateRepresentatives.get(focus.timelineId) ?? focus.timelineId)
      : null;
  const domIdOf = (timelineId: string) =>
    messageDomId(sessionByTimelineId.get(timelineId) ?? session.id, timelineId);
  useLayoutEffect(() => {
    onTimelineLayout?.({
      sessionId: session.id,
      rows: displayedRows,
      ready: feed.snapshotReady && (feed.rows[0]?.session_id ?? session.id) === session.id,
      readySessionIds: [session.id, ...(teamFeed.historyReady ? teamIds : [])],
      elementForRow: (row) => document.getElementById(messageDomId(row.session_id, row.timeline_id)),
      elementForAnchor: (timelineId) => {
        const representative = model.representatives.get(timelineId) ?? timelineId;
        return document.getElementById(messageDomId(sessionByTimelineId.get(representative) ?? session.id, representative));
      },
    });
  }, [onTimelineLayout, session.id, displayedRows, feed.snapshotReady, feed.rows, teamFeed.historyReady, teamIds, model.representatives, sessionByTimelineId]);
  const teamFocus = focus !== null && teamIdSet.has(focus.sessionId) ? focus : null;
  const [teamHighlight, setTeamHighlight] = useState<string | null>(null);
  const teamFocusRequest = useRef<string | null>(null);
  const highlightedTimelineId = teamHighlight ?? parentHighlightedTimelineId;

  useEffect(() => {
    if (focus === null || focus.sessionId !== session.id) return;
    const drawnBy = chronology.duplicateRepresentatives.get(focus.timelineId);
    if (drawnBy === undefined) return;
    const element = document.getElementById(
      messageDomId(sessionByTimelineId.get(drawnBy) ?? session.id, drawnBy),
    );
    if (element === null) return;
    revealActivityTarget(element);
    setTeamHighlight(drawnBy);
    onFocusConsumed();
    window.setTimeout(() => setTeamHighlight(null), 1500);
  }, [
    focus,
    session.id,
    chronology.duplicateRepresentatives,
    sessionByTimelineId,
    onFocusConsumed,
  ]);

  useEffect(() => {
    if (teamFocus === null) {
      teamFocusRequest.current = null;
      return;
    }
    const drawnBy = model.representatives.get(teamFocus.timelineId) ?? null;
    if (drawnBy !== null) {
      const element = document.getElementById(
        messageDomId(sessionByTimelineId.get(drawnBy) ?? session.id, drawnBy),
      );
      if (element !== null) {
        revealActivityTarget(element);
        setTeamHighlight(drawnBy);
        onFocusConsumed();
        window.setTimeout(() => setTeamHighlight(null), 1500);
      }
      return;
    }

    const source = teamFeed.rowsBySession
      .get(teamFocus.sessionId)
      ?.find((row) => row.timeline_id === teamFocus.timelineId);
    if (source !== undefined) {
      onFocusConsumed();
      onReceipt(
        source.body.type === "collaboration" && source.body.event.channel === "coordinator"
          ? "That message is private team context, not shared team chronology"
          : "That team-member row is private work, not a delivered boundary message",
      );
      return;
    }

    const requestKey = `${teamFocus.sessionId}\u0000${teamFocus.timelineId}`;
    if (teamFocusRequest.current === requestKey) return;
    teamFocusRequest.current = requestKey;
    void teamFeed
      .loadAnchor(teamFocus.sessionId, teamFocus.timelineId)
      .then((loaded) => {
        if (loaded) return;
        onFocusConsumed();
        onReceipt("That team message is no longer available");
      })
      .catch((error: unknown) => {
        onFocusConsumed();
        onReceipt(error instanceof Error ? error.message : "Could not load that team message");
      });
  }, [
    teamFocus,
    teamFeed.loadAnchor,
    teamFeed.rowsBySession,
    model,
    onFocusConsumed,
    onReceipt,
    session.id,
    sessionByTimelineId,
  ]);

  // Children this document owns. A proven `child_session_id` anchors an
  // ordinary Agent worker under its earliest line. Team members are consumed
  // above as roster + shared chronology and must never open duplicate threads.
  const workerChildren = directChildren.filter((child) => !teamIdSet.has(child.id));
  const ledgerLines = useMemo(
    () =>
      model.turns.flatMap((turn) =>
        turn.segments.flatMap((segment) => (segment.kind === "ledger" ? segment.lines : [])),
      ),
    [model],
  );
  const workerLinks = useMemo(
    () => workerThreadLinks(ledgerLines, session.id, index.byId, teamIdSet),
    [ledgerLines, session.id, index.byId, teamIdSet],
  );
  // Protocol-v14 daemons built before workflow child references were added
  // omit `child_session_ids`. Resolve their lightweight workflow anchors once
  // so a rolling client upgrade remains exact instead of guessing from a
  // worker's arbitrary agent type/title. New daemons never take this path.
  const legacyWorkflowRows = useMemo(
    () => (feed.timelineMeta?.workflows ?? [])
      .filter((workflow) => workflow.child_session_ids === undefined)
      .map((workflow) => ({ id: workflow.id, timelineId: workflow.timeline_id })),
    [feed.timelineMeta],
  );
  const legacyWorkflowKey = `${session.id}\u0002${legacyWorkflowRows
    .map((workflow) => `${workflow.id}\u0000${workflow.timelineId ?? ""}`)
    .join("\u0001")}`;
  const [legacyWorkflowIndex, setLegacyWorkflowIndex] = useState<LegacyWorkflowChildIndex | null>(
    null,
  );
  useEffect(() => {
    if (legacyWorkflowRows.length === 0) {
      setLegacyWorkflowIndex(null);
      return;
    }
    let current = true;
    const anchors = legacyWorkflowRows.flatMap((workflow) =>
      workflow.timelineId === null ? [] : [workflow.timelineId]
    );
    void Promise.allSettled(
      anchors.map(async (timelineId) => {
        const row = await client.timelineEntry(session.id, timelineId);
        return linkedChildSessionIds(row);
      }),
    ).then((results) => {
      if (!current) return;
      const ids = new Set(
        results.flatMap((result) => result.status === "fulfilled" ? result.value : []),
      );
      setLegacyWorkflowIndex({
        key: legacyWorkflowKey,
        ids,
        complete: anchors.length === legacyWorkflowRows.length
          && results.every((result) => result.status === "fulfilled"),
      });
    });
    return () => {
      current = false;
    };
  }, [legacyWorkflowKey, session.id]);
  const indexedWorkflowChildren = useMemo(
    () => {
      const ids = new Set(
        (feed.timelineMeta?.workflows ?? []).flatMap(
          (workflow) => workflow.child_session_ids ?? [],
        ),
      );
      const legacyReady = feed.timelineMeta !== null
        && (legacyWorkflowRows.length === 0
          || (legacyWorkflowIndex?.key === legacyWorkflowKey && legacyWorkflowIndex.complete));
      if (legacyWorkflowIndex?.key === legacyWorkflowKey) {
        for (const id of legacyWorkflowIndex.ids) ids.add(id);
      }
      if (!legacyReady) {
        // Until every old-daemon anchor answers, fail closed only for children
        // whose sidecar proves delegation but supplies no ordinary Agent tool
        // owner. Exact ordinary workers and parent-only children still render.
        for (const child of workerChildren) {
          if (
            child.delegation !== null
            && child.delegation.parent_tool_native_id == null
          ) ids.add(child.id);
        }
      }
      return ids;
    }, [
      feed.timelineMeta,
      legacyWorkflowIndex,
      legacyWorkflowKey,
      legacyWorkflowRows.length,
      workerChildren,
    ]
  );
  const floating = standaloneWorkerSessions(
    workerChildren,
    workerLinks,
    indexedWorkflowChildren,
  );
  const [revealedWorker, setRevealedWorker] = useState<{
    readonly childId: string;
    readonly anchorTimelineId: string;
    readonly request: number;
  } | null>(null);
  const revealRequest = useRef(0);
  const revealLinkedWorker = (childId: string) => {
    const anchorTimelineId = workerLinks.anchorRowByChild.get(childId);
    if (anchorTimelineId === undefined) return;
    revealRequest.current += 1;
    setRevealedWorker({ childId, anchorTimelineId, request: revealRequest.current });
  };

  useEffect(() => {
    if (revealedWorker === null) return;
    const frame = window.requestAnimationFrame(() => {
      const element = document.getElementById(messageDomId(session.id, revealedWorker.anchorTimelineId));
      if (element !== null) revealActivityTarget(element);
    });
    return () => window.cancelAnimationFrame(frame);
  }, [revealedWorker, session.id]);

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
      onOpenAttachment={onOpenAttachment}
      onOpenPath={onOpenPath}
      getRuntimeKind={getRuntimeKind}
      homeDirectory={homeDirectory}
    />
  );

  const reportWorkFor = (row: TimelineEntry): TeamReportWorkPresentation | null => {
    if (row.body.type !== "collaboration") return null;
    const event = row.body.event;
    if (event.kind !== "report" || event.source_session_id === null || event.source_response_id === null
        || event.work_start_timeline_id === null || event.work_end_timeline_id === null) return null;
    const memberSession = index.byId.get(event.source_session_id);
    if (memberSession === undefined || !teamIdSet.has(memberSession.id)) return null;
    const participant = participantPresentation(participants, event.sender, event.team);
    return {
      actionCount: null,
      startedAt: null,
      endedAt: null,
      content: <LazyReportWork key={`${row.session_id}:${row.timeline_id}`} report={row} session={memberSession}
        name={event.sender} color={event.color ?? participant?.color ?? null}
        runtime={participant?.runtime ?? runtimeProfile([], memberSession.delegation?.configured_model ?? null,
          teamFeed.responseRuntimesBySession.get(memberSession.id) ?? [])}
        index={index} onReceipt={onReceipt} onOpenAttachment={onOpenAttachment} onOpenPath={onOpenPath}
        getRuntimeKind={getRuntimeKind} homeDirectory={homeDirectory} />,
    };
  };

  const renderSegment = (segment: Segment, embedded = false): ReactNode => {
    const rowId = segment.kind === "ledger" ? segment.rows[0].timeline_id : segment.row.timeline_id;
    const hit = highlightedTimelineId === rowId;
    switch (segment.kind) {
      case "ledger":
        return (
          <Ledger
            key={segment.key}
            embedded={embedded}
            members={segment.members}
            summary={segment.summary}
            anchor={
              !embedded && segment.summary.closedByDefault ? (
                <Anchor
                  at={segment.at}
                  sessionId={segment.rows[0].session_id}
                  timelineId={rowId}
                  onReceipt={onReceipt}
                />
              ) : null
            }
            domIdOf={domIdOf}
            highlightedTimelineId={highlightedTimelineId}
            focusTimelineId={focusTimelineId}
            alive={(line) =>
              recentActivity &&
              line.rows[0].session_id === session.id &&
              feed.watermark !== null && line.rows[0].ordinal > feed.watermark
            }
            hasChildren={(line) =>
              (workerLinks.childrenByRow.get(line.rows[0].timeline_id) ?? []).length > 0
            }
            childFocused={(line) =>
              (focus !== null &&
                (workerLinks.childrenByRow.get(line.rows[0].timeline_id) ?? []).some((child) =>
                  sessionContains(child.id, focus.sessionId, index.byId),
                )) ||
              (revealedWorker !== null &&
                (workerLinks.childrenByRow.get(line.rows[0].timeline_id) ?? []).some(
                  (child) => child.id === revealedWorker.childId,
                ))
            }
            subthreadsFor={(line: LedgerLine) =>
              (workerLinks.childrenByRow.get(line.rows[0].timeline_id) ?? []).map((child) =>
                ({ childSessionId: child.id, node: renderChild(child, true) }),
              )
            }
            workflowAttemptsFor={(line) => workflowAttemptsFor(workflowAttempts, line.rows[0])}
            linkedChildId={(line) =>
              workerLinks.referencedChildIdsByRow.get(line.rows[0].timeline_id)?.[0] ?? null
            }
            onOpenLinkedChild={revealLinkedWorker}
            teamMemberFor={(line) =>
              line.rows
                .map((row) => teamMembers.get(row.timeline_id) ?? null)
                .find((member) => member !== null) ?? null
            }
            onOpenPath={onOpenPath}
            onOpenAttachment={onOpenAttachment}
          />
        );
      case "thinking":
        // A thought alone is a run of one, drawn as its line (Q10 A).
        return (
          <ThoughtLine
            key={segment.key}
            text={segment.text}
            embedded={embedded}
            spanMs={segment.spanMs}
            domId={domIdOf(rowId)}
            timelineId={rowId}
            hit={hit}
            forceOpen={focusTimelineId === rowId}
            anchor={
              <Anchor
                at={segment.at}
                sessionId={segment.row.session_id}
                timelineId={rowId}
                onReceipt={onReceipt}
              />
            }
          />
        );
      case "marker":
        if (segment.notice.type === "permission_requested") return <Marker key={segment.key}
          text={segment.text} tone={segment.tone} domId={domIdOf(rowId)} timelineId={rowId} hit={hit} />;
        return <ContextEvent key={segment.key} segment={segment} domId={domIdOf(rowId)} hit={hit}
          forceOpen={focusTimelineId === rowId} onReceipt={onReceipt} />;
      case "summary":
        return <ContextEvent key={segment.key} segment={segment} domId={domIdOf(rowId)} hit={hit}
          forceOpen={focusTimelineId === rowId} onReceipt={onReceipt} />;
      case "compaction":
        return (
          <Compaction
            key={segment.key}
            text={segment.text}
            domId={domIdOf(rowId)}
            timelineId={rowId}
            hit={segment.rows.some((row) => row.timeline_id === highlightedTimelineId)}
            forceOpen={segment.rows.some((row) => row.timeline_id === focusTimelineId)}
            onReceipt={onReceipt}
          />
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
              sessionId={segment.row.session_id}
              timelineId={rowId}
              onReceipt={onReceipt}
            />
            <AuthoredMessage text={segment.text} attachments={segment.attachments}
              recordedText={segment.row.body.type === "agent" ? segment.row.body.text : null}
              messageKey={`${segment.row.session_id}:${rowId}`} role="agent"
              onReceipt={onReceipt} onOpenAttachment={onOpenAttachment} />
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
      case "collaboration": {
        const sourceMember = teamIdSet.has(segment.row.session_id)
          ? index.byId.get(segment.row.session_id)
          : undefined;
        const inferredRecipient =
          segment.event.channel === "coordinator" && segment.event.recipient === null
            ? (sourceMember?.title ?? sourceMember?.native_session_id ?? null)
            : segment.event.recipient;
        const event =
          inferredRecipient === segment.event.recipient
            ? segment.event
            : { ...segment.event, recipient: inferredRecipient };
        const participant = participantPresentation(
          participants,
          event.sender,
          event.team,
        );
        const recipientParticipant = participantPresentation(
          participants,
          event.recipient,
          event.team,
        );
        const stamp = collaborationStamp(segment.row);
        return (
          <TeamEvent
            key={segment.key}
            event={event}
            at={stamp.at}
            timeBasis={stamp.basis}
            domId={domIdOf(rowId)}
            timelineId={rowId}
            participant={participant}
            recipientParticipant={recipientParticipant}
            hit={hit}
            forceOpen={focusTimelineId === rowId}
            reportWork={reportWorkFor(segment.row)}
            onReceipt={onReceipt}
          />
        );
      }
    }
  };

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

  const renderTools = (items: readonly Extract<ActivityItem, { kind: "tool" }>[], embedded: boolean) => {
    const lines = items.map(item => item.line);
    const rows = lines.flatMap(line => line.rows);
    return renderSegment({ kind: "ledger", key: items[0].key, at: rows[0].at, rows, lines,
      members: lines.map(line => ({ kind: "line", key: line.key, line })), summary: runSummary(lines) }, embedded);
  };
  const renderItem = (item: ActivityItem): ReactNode => {
    if (item.kind === "tool") return renderTools([item], false);
    if (item.kind !== "prompt") return renderSegment(item, true);
    const row = item.prompt.row;
    if (row.body.type === "collaboration") return renderSegment({ kind: "collaboration", key: item.key,
      row, at: row.at, event: row.body.event }, true);
    return <Prompt key={item.key} prompts={[item.prompt]} domIdOf={domIdOf}
      landedTimelineId={highlightedTimelineId} focusTimelineId={focusTimelineId}
      onReceipt={onReceipt} onOpenAttachment={onOpenAttachment} />;
  };
  const renderItems = (items: readonly ActivityItem[]) => {
    const nodes: ReactNode[] = [];
    let tools: Extract<ActivityItem, { kind: "tool" }>[] = [];
    const flush = () => { if (tools.length > 0) nodes.push(renderTools(tools, true)); tools = []; };
    for (const item of items) {
      if (item.kind === "tool") tools.push(item);
      else { flush(); nodes.push(renderItem(item)); }
    }
    flush();
    return nodes;
  };
  const childIsFocused = (item: ActivityItem) => item.kind === "tool" &&
    (workerLinks.childrenByRow.get(item.line.rows[0].timeline_id) ?? []).some(child =>
      (focus !== null && sessionContains(child.id, focus.sessionId, index.byId)) || child.id === revealedWorker?.childId);
  const renderBlock = (block: ReadingBlock): ReactNode => {
    switch (block.kind) {
      case "gap": return <GapMarker count={block.count} onPress={() => onReceipt(
        `${block.count} ${block.count === 1 ? "message is" : "messages are"} in the session but not in this page`)} />;
      case "watermark": return <Watermark text="received since opening" />;
      case "prompt": return <div className="tr-turn tr-reading-prompt">
        <Prompt prompts={block.prompts} domIdOf={domIdOf} landedTimelineId={highlightedTimelineId}
          onReceipt={onReceipt} onOpenAttachment={onOpenAttachment} />
      </div>;
      case "reply": return <div className={block.head === null ? "tr-reading-reply" : "tr-resp tr-reading-reply"}>
        {block.head !== null && <ResponseHead head={block.head} runtimeKind={runtimeKind}
          open={recentActivity && block.head.endedAt === null} />}
        {renderSegment(block.segment)}
      </div>;
      case "request": return renderItem(block.item);
      case "activity": {
        const hit = highlightedTimelineId !== null && block.rows.some(row => row.timeline_id === highlightedTimelineId)
          ? highlightedTimelineId : null;
        const forceOpen = block.items.some(item => childIsFocused(item) ||
          activityRows(item).some(row => row.timeline_id === focusTimelineId));
        return <ActivityGroup items={block.items} rows={block.rows} partial={block.partial} forceOpen={forceOpen}
          highlightedId={hit} alive={row => recentActivity && row.session_id === session.id &&
            feed.watermark !== null && row.ordinal > feed.watermark}>
          {renderItems(block.items)}
        </ActivityGroup>;
      }
    }
  };

  const partialParentHistory = !feed.snapshotReady || feed.earlier.kind !== "none";
  const hasSafeRosterLine = [...teamMembers.values()].some(
    (member) => member.operation === "known",
  );
  const showKnownTeam =
    knownTeamSessions.length > 0 &&
    (teamMembers.size === 0 || (partialParentHistory && !hasSafeRosterLine));
  const empty = model.plan.length === 0 && floating.length === 0 && !showKnownTeam;
  return (
    <ThreadNavigationContext.Provider value={childNavigation}>
    <DisclosureState key={session.id}>
      {head}
      {showKnownTeam && (
        <KnownTeamRoster
          sessions={knownTeamSessions}
          rowsBySession={teamFeed.rowsBySession}
          responseRuntimesBySession={teamFeed.responseRuntimesBySession}
          historyReady={teamFeed.historyReady}
        />
      )}
      {reading.map(({ sitting, blocks }) => (
        <Sitting key={sitting.key} sitting={sitting}>
          {showPills && (
            <DayPill
              sitting={sitting}
              onPress={(event) => onPillPress?.(event.currentTarget, sitting.dayKey)}
            />
          )}
          {blocks.map(block => <Fragment key={block.key}>{drainFloating(block.at)}{renderBlock(block)}</Fragment>)}
        </Sitting>
      ))}
      {drainFloating(null)}
      {empty &&
        feed.snapshotReady &&
        (nested ? (
          <p className="tr-quiet">
            {session.stream_state === "indexed"
              ? "No readable messages in this subthread."
              : "This subthread has not been indexed yet."}
          </p>
        ) : session.stream_state !== "indexed" ? (
          <EmptyState
            title={
              duringBackfill ? "Indexing history" : "History not yet available"
            }
            description="No readable messages have been indexed for this session yet."
          >
            {duringBackfill && <div className="ah-meter">
              <i style={{ width: "8%" }} />
            </div>}
          </EmptyState>
        ) : (
          <EmptyState
            title="Nothing readable yet"
            description="This session has no readable timeline rows in the index."
          />
        ))}
      {session.stream_state === "streaming" && (
        <div className="tr-mark" title="Some source records have not been fully indexed">
          {duringBackfill ? "indexing history" : "history incomplete"}
        </div>
      )}
    </DisclosureState>
    </ThreadNavigationContext.Provider>
  );
}
