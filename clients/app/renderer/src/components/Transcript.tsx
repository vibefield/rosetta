// Column ④ — the transcript: a document, not a chat log.
//
// The column is the frame; the document inside it is `components/transcript`.
// What lives here is what belongs to the COLUMN rather than to the reading:
// the head band and its two acts, the scroller and the four things pinned to
// it (the top fade, the day pill's menu, the earlier receipt, the live tail's
// notice), the search-results view that replaces the document, and the focus
// landing that brings a search hit home.
//
// Its props are App.tsx's contract and have not moved, except for the two
// facts the record's own drawing needs: the session's runtime (the reply's
// mark and name, T-4) and the viewer's home directory (a path is drawn
// relative to the session's cwd, then home-relative — never the eleven leading
// characters that are the same on every line).
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  SearchHit,
  SessionRow,
  TimelineMetaArtifact,
  TimelineMetaPlan,
  TimelineMetaTask,
  TimelineMetaWorkflow,
} from "@rosetta/protocol";
import { clockShort } from "../lib/format";
import type { TimelineFeed } from "../lib/hooks";
import { useHeadOfDocument } from "../lib/transcript/head-of-document";
import type { AttachmentFact, PathFact } from "../lib/transcript";
import type { ProjectViewModel } from "../lib/model";
import { parseSnippet } from "../lib/model";
import type { SearchResultsViewModel } from "../lib/search-results";
import type { DayEntry } from "../lib/transcript";
import { getSessionTitle } from "./sessions/session-display";
import { DayMenu, markStuckPills } from "./transcript/DayPill";
import { TranscriptDocument } from "./transcript/Document";
import { useTranscriptModel } from "./transcript/document-model";
import { useFoldPreference } from "./transcript/fold-preference";
import { ActivityGlyph } from "./transcript/glyphs";
import { HeadOfDocument, TopFade } from "./transcript/HeadOfDocument";
import { ReceiptPill } from "./transcript/ReceiptPill";
import { TimelineMeta } from "./transcript/TimelineMeta";
import { buildThreadIndex, messageDomId } from "./transcript/thread-index";
import type { ThreadFocus } from "./transcript/thread-index";
import { ColumnHeading, EmptyState, ResultGroupHeader } from "./ui";
import { FileViewerDialog } from "./file-viewer";
import type { FileHandle, FileSource } from "./file-viewer";
import {
  attachmentFileSource,
  handleForAttachment,
} from "./transcript/attachment-file-source";
import {
  handleForReadPath,
  transcriptReadFileSource,
} from "./transcript/read-file-source";
import {
  handleForTimelineMetaArtifact,
  handleForTimelineMetaPlan,
  timelineMetaFileSource,
} from "./transcript/timeline-meta-file-source";

export type { ThreadFocus } from "./transcript/thread-index";

/** The receipt lands beneath a pinned day pill, never over it (§08).
 * Measured rather than guessed: the scroller starts 46px down (the head band),
 * a pinned pill occupies 58–80, and the first pill still in the flow starts at
 * 124. 88 is the one band that clears both. */
const RECEIPT_TOP = 88;

interface TranscriptViewerTarget {
  readonly handle: FileHandle;
  readonly source: FileSource;
}

interface TimelineMetaFocus {
  readonly timelineId: string;
  readonly label: string;
  readonly kind: "task" | "workflow";
}

export interface TranscriptColumnProps {
  readonly session: SessionRow | null;
  /** The cold backfill is running — threaded to every watermark. */
  readonly duringBackfill: boolean;
  /** Full metadata topology. Only roots are navigable, but descendants are
   * needed here to build the embedded thread tree. */
  readonly sessions: readonly SessionRow[];
  readonly project: ProjectViewModel | null;
  readonly feed: TimelineFeed;
  readonly focus: ThreadFocus | null;
  readonly onFocusConsumed: () => void;
  readonly onReceipt: (text: string) => void;
  readonly searchResults: SearchResultsViewModel | null;
  readonly onOpenHit: (session: SessionRow, hit: SearchHit) => void;
  /** The session's runtime, for the reply's mark and name (T-4). */
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly homeDirectory: string | null;
}

export function TranscriptColumn({
  session,
  duringBackfill,
  sessions,
  project,
  feed,
  focus,
  onFocusConsumed,
  onReceipt,
  searchResults,
  onOpenHit,
  getRuntimeKind,
  homeDirectory,
}: TranscriptColumnProps) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const seenMax = useRef<number>(-1);
  const [unseen, setUnseen] = useState(0);
  const [highlightedTimelineId, setHighlightedTimelineId] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ top: number; dayKey: string | null } | null>(null);
  const [viewerTarget, setViewerTarget] = useState<TranscriptViewerTarget | null>(null);
  const [metaFocus, setMetaFocus] = useState<TimelineMetaFocus | null>(null);
  const viewerRequest = useRef(0);
  const anchorRequest = useRef(0);
  const focusTries = useRef(0);
  const index = useMemo(() => buildThreadIndex(sessions), [sessions]);
  const fold = useFoldPreference();

  const searching = searchResults !== null;
  const maxOrdinal = feed.rows.length ? feed.rows[feed.rows.length - 1].ordinal : -1;
  const live = session?.stream_state === "streaming";
  const requestedTimelineId =
    focus !== null && focus.sessionId === session?.id ? focus.timelineId : null;
  const targetTimelineId = requestedTimelineId ?? metaFocus?.timelineId ?? null;
  const targetIsMeta = requestedTimelineId === null && metaFocus !== null;

  const model = useTranscriptModel(feed.rows, session, homeDirectory, Date.now());

  // §08 — the head of the document: approach fetches and holds, the pull
  // reveals, and the landing keeps the reader exactly where they were.
  const headOfDocument = useHeadOfDocument(scrollRef, feed, {
    enabled: !searching && session !== null,
  });

  const atFoot = () => {
    const element = scrollRef.current;
    if (!element) return true;
    return element.scrollTop + element.clientHeight >= element.scrollHeight - 32;
  };

  const wasAtFoot = useRef(true);
  useEffect(() => {
    const element = scrollRef.current;
    if (!element) return;
    if (wasAtFoot.current) {
      element.scrollTop = element.scrollHeight;
      seenMax.current = maxOrdinal;
      setUnseen(0);
    } else if (feed.watermark !== null) {
      const fresh = feed.rows.filter(
        (row) => row.ordinal > feed.watermark! && row.ordinal > seenMax.current,
      ).length;
      if (fresh > 0) setUnseen((value) => value + fresh);
      seenMax.current = maxOrdinal;
    }
    // Only a new tail row should trigger live-follow behavior. Loading older
    // pages changes `feed.rows` without changing the tail and must not jump.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [maxOrdinal]);

  useEffect(() => {
    seenMax.current = -1;
    wasAtFoot.current = true;
    setUnseen(0);
    setHighlightedTimelineId(null);
    setMenu(null);
    viewerRequest.current += 1;
    anchorRequest.current += 1;
    setViewerTarget(null);
    setMetaFocus(null);
    focusTries.current = 0;
  }, [session?.id]);

  const openAttachment = (fact: AttachmentFact) => {
    viewerRequest.current += 1;
    const handle = handleForAttachment(fact);
    if (handle === null) {
      onReceipt("That image was not stored");
      return;
    }
    setViewerTarget({ handle, source: attachmentFileSource });
  };

  const openReadPath = (path: PathFact) => {
    const request = (viewerRequest.current += 1);
    void handleForReadPath(path)
      .then((handle) => {
        if (request !== viewerRequest.current) return;
        if (handle === null) {
          onReceipt("That row did not record an openable file");
          return;
        }
        setViewerTarget({ handle, source: transcriptReadFileSource });
      })
      .catch((error: unknown) => {
        if (request !== viewerRequest.current) return;
        onReceipt(error instanceof Error ? error.message : "Could not open that recorded file");
      });
  };

  const openMetaFile = (
    load: () => Promise<FileHandle>,
    unavailable: string,
  ) => {
    const request = (viewerRequest.current += 1);
    void load()
      .then((handle) => {
        if (request !== viewerRequest.current) return;
        setViewerTarget({ handle, source: timelineMetaFileSource });
      })
      .catch((error: unknown) => {
        if (request !== viewerRequest.current) return;
        onReceipt(error instanceof Error ? error.message : unavailable);
      });
  };

  const openMetaPlan = (plan: TimelineMetaPlan) => {
    if (session === null) return;
    openMetaFile(
      () => handleForTimelineMetaPlan(session.id, plan),
      "Could not open that recorded plan",
    );
  };

  const openMetaArtifact = (artifact: TimelineMetaArtifact) => {
    if (session === null) return;
    openMetaFile(
      () => handleForTimelineMetaArtifact(session.id, artifact),
      "Could not open that recorded artifact",
    );
  };

  const openMetaTimeline = (
    timelineId: string | null,
    label: string,
    kind: TimelineMetaFocus["kind"],
  ) => {
    if (timelineId === null) {
      onReceipt(`That ${kind} has no recorded message anchor`);
      return;
    }
    const land = () => {
      focusTries.current = 0;
      setMetaFocus({ timelineId, label, kind });
    };
    if (model.representatives.has(timelineId)) {
      land();
      return;
    }
    const request = (anchorRequest.current += 1);
    void feed.loadAnchor(timelineId)
      .then((loaded) => {
        if (request !== anchorRequest.current || !loaded) return;
        land();
      })
      .catch((error: unknown) => {
        if (request !== anchorRequest.current) return;
        onReceipt(error instanceof Error ? error.message : `Could not load that ${kind} message`);
      });
  };

  const openMetaTask = (task: TimelineMetaTask) => {
    openMetaTimeline(task.timeline_id, task.subject, "task");
  };

  const openMetaWorkflow = (workflow: TimelineMetaWorkflow) => {
    openMetaTimeline(workflow.timeline_id, workflow.name, "workflow");
  };

  useEffect(() => {
    focusTries.current = 0;
    // Only a NEW target clears the ring. Consuming the focus sets the target
    // to null, and clearing on that would wipe the ring in the same frame it
    // was drawn — the landing would scroll and then show nothing.
    if (targetTimelineId !== null) setHighlightedTimelineId(null);
  }, [targetTimelineId]);

  useEffect(() => {
    // Wait for the subscription to have DELIVERED, not merely opened: a feed
    // that has not had its first snapshot reports nothing above it, and acting
    // on that would tell the reader a row is older than the loaded history
    // one frame before the history exists.
    if (
      targetTimelineId === null ||
      searching ||
      session === null ||
      (feed.status !== "live" && feed.status !== "resyncing")
    ) {
      return;
    }
    // A fold draws several rows as one line, so the landing goes to the
    // element that CONTAINS the row rather than failing to find its own.
    const drawnBy = model.representatives.get(targetTimelineId) ?? null;
    if (drawnBy !== null) {
      const element = document.getElementById(messageDomId(session.id, drawnBy));
      if (element) {
        wasAtFoot.current = false;
        element.scrollIntoView({ block: "center" });
        setHighlightedTimelineId(drawnBy);
        if (targetIsMeta) {
          onReceipt(`Landed on the ${metaFocus.kind} message · ${metaFocus.label}`);
          setMetaFocus(null);
        } else {
          onFocusConsumed();
        }
        window.setTimeout(() => setHighlightedTimelineId(null), 1500);
      }
      return;
    }
    // timeline_meta resolves exact anchors before setting focus. The bounded
    // blind walk remains only for external search landings.
    if (targetIsMeta) {
      const kind = metaFocus.kind;
      setMetaFocus(null);
      onReceipt(`That ${kind} message is no longer available`);
      return;
    }
    if (feed.earlier.kind !== "none" && focusTries.current < 8) {
      focusTries.current += 1;
      feed.loadEarlier();
    } else if (feed.earlier.kind === "none") {
      if (targetIsMeta) setMetaFocus(null);
      else onFocusConsumed();
      onReceipt("That message is older than the loaded history");
    }
  }, [targetTimelineId, targetIsMeta, metaFocus, session, feed, model, searching, onFocusConsumed, onReceipt]);

  // The pill pins while the reader is inside its day, and returns to its
  // hairline the moment that day scrolls back into view.
  useEffect(() => {
    markStuckPills(scrollRef.current);
  }, [model.plan]);

  const range = (() => {
    if (!session) return "";
    const from = clockShort(session.started_at);
    return live ? `${from} → now` : `${from} → ${clockShort(session.updated_at)}`;
  })();

  const jumpToDay = (day: DayEntry) => {
    setMenu(null);
    const scroller = scrollRef.current;
    if (scroller === null) return;
    const target = scroller.querySelector<HTMLElement>(
      `[data-sitting-key="${CSS.escape(day.firstSittingKey)}"]`,
    );
    if (target === null) return;
    // Section-relative offsets do not survive a sticky pill; the rects do.
    wasAtFoot.current = false;
    scroller.scrollTo({
      top: target.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop - 4,
      behavior: "smooth",
    });
    const prompt = target.querySelector<HTMLElement>(".tr-prompt");
    if (prompt === null) return;
    prompt.classList.add("is-landed");
    window.setTimeout(() => prompt.classList.remove("is-landed"), 1400);
  };

  return (
    <div
      className="ah-col ah-col--measure"
      style={{ position: "relative" }}
      data-fold={fold.folded ? "on" : "off"}
    >
      <header>
        {searching || session === null ? (
          <div className="ah-colhead">
            <span className="ah-eyebrow">{searching ? "Matches" : "Transcript"}</span>
          </div>
        ) : (
          <ColumnHeading
            name={getSessionTitle(session)}
            tint={project?.tint ?? null}
            count={feed.rows.length}
            countTitle={`${feed.rows.length} messages loaded`}
          />
        )}
        <span className="ah-headacts">
          <span className="ah-caption tnum">
            {searchResults
              ? `${searchResults.counts.matches} ${searchResults.counts.matches === 1 ? "match" : "matches"} · ${searchResults.counts.sessions} ${searchResults.counts.sessions === 1 ? "session" : "sessions"} · ${searchResults.counts.projects} ${searchResults.counts.projects === 1 ? "project" : "projects"}`
              : range}
          </span>
          {!searching && session !== null && (
            // §05 — the activity act, beside the range. Folded is the default;
            // the reader who wants the build log gets it in one press, and the
            // preference is per room, not per run.
            <button
              type="button"
              className="ah-iconbtn"
              aria-pressed={fold.folded}
              title={
                fold.folded
                  ? "Activity: folded — press to open every run"
                  : "Activity: open — press to fold every run"
              }
              onClick={fold.toggle}
            >
              <ActivityGlyph />
            </button>
          )}
        </span>
      </header>
      {!searching && session !== null && feed.timelineMeta !== null && (
        <TimelineMeta
          meta={feed.timelineMeta}
          onOpenTask={openMetaTask}
          onOpenWorkflow={openMetaWorkflow}
          onOpenPlan={openMetaPlan}
          onOpenArtifact={openMetaArtifact}
        />
      )}
      <div
        className="ah-scroll tr-scroll"
        data-flush=""
        ref={scrollRef}
        onScroll={() => {
          wasAtFoot.current = atFoot();
          if (wasAtFoot.current && unseen > 0) setUnseen(0);
          markStuckPills(scrollRef.current);
        }}
      >
        {/* The top fade is the pinned pill's ground, and it must be the first
            child of the scroller: `useHeadOfDocument` writes `--frost` here. */}
        <TopFade />
        {searching ? (
          <div className="ah-hitlist">
            {searchResults.groups.flatMap((group) =>
              group.sessions.map((result) => (
                <div key={result.session.id}>
                  <ResultGroupHeader
                    label={`${group.project.row.name} · ${getSessionTitle(result.session)}`}
                    count={result.hits.length}
                    countLabel={`${result.hits.length} ${result.hits.length === 1 ? "hit" : "hits"}`}
                    tint={group.project.tint}
                  />
                  {result.hits.map((hit) => (
                    <button
                      type="button"
                      className="ah-hit"
                      key={`${hit.session_id}:${hit.timeline_id}`}
                      onClick={() => onOpenHit(result.session, hit)}
                    >
                      <p>
                        {parseSnippet(hit.snippet).map((part, at) =>
                          part.mark ? <mark key={at}>{part.text}</mark> : <span key={at}>{part.text}</span>,
                        )}
                      </p>
                    </button>
                  ))}
                </div>
              )),
            )}
            {searchResults.counts.matches === 0 && (
              <EmptyState title="Nothing matched" description="No indexed message contains that." />
            )}
          </div>
        ) : session === null ? (
          <EmptyState
            title="No session open"
            description="Pick a session — or search, which is the front door."
          />
        ) : (
          <div className="tr-col">
            <div className="tr">
              <TranscriptDocument
                session={session}
                model={model}
                feed={feed}
                duringBackfill={duringBackfill}
                index={index}
                focus={focus}
                highlightedTimelineId={highlightedTimelineId}
                onFocusConsumed={onFocusConsumed}
                onReceipt={onReceipt}
                onOpenAttachment={openAttachment}
                onOpenPath={openReadPath}
                getRuntimeKind={getRuntimeKind}
                homeDirectory={homeDirectory}
                onPillPress={(element: HTMLElement, dayKey: string | null) => {
                  const frame = element.closest(".ah-col");
                  if (frame === null) return;
                  const top =
                    element.getBoundingClientRect().bottom - frame.getBoundingClientRect().top + 6;
                  setMenu((open) => (open === null ? { top, dayKey } : null));
                }}
                head={
                  <HeadOfDocument
                    feed={feed}
                    pull={headOfDocument.pull}
                    startedAt={session.started_at}
                    finding={targetTimelineId === null ? null : "an anchor"}
                  />
                }
              />
            </div>
          </div>
        )}
      </div>
      {menu !== null && !searching && (
        <DayMenu
          days={model.days}
          currentDayKey={menu.dayKey}
          top={menu.top}
          onJump={jumpToDay}
          onDismiss={() => setMenu(null)}
        />
      )}
      <ReceiptPill receipt={headOfDocument.receipt} top={RECEIPT_TOP} />
      {unseen > 0 && (
        <button
          type="button"
          className="ah-newpill"
          onClick={() => {
            const element = scrollRef.current;
            if (element) element.scrollTo({ top: element.scrollHeight, behavior: "smooth" });
            setUnseen(0);
          }}
        >
          <i />
          <span>
            {unseen} new {unseen === 1 ? "message" : "messages"}
          </span>
        </button>
      )}
      <FileViewerDialog
        handle={viewerTarget?.handle ?? null}
        source={viewerTarget?.source ?? attachmentFileSource}
        tint={project?.tint ?? null}
        homeDirectory={homeDirectory}
        onClose={() => {
          viewerRequest.current += 1;
          setViewerTarget(null);
        }}
        onReceipt={onReceipt}
      />
    </div>
  );
}
