// Column ④ — the transcript: a document, not a chat log.
//
// The column is the frame; the document inside it is `components/transcript`.
// What lives here is what belongs to the COLUMN rather than to the reading:
// the head band and its two acts, the scroller and the four things pinned to
// it (the top fade, the day pill's menu, the earlier receipt, the live tail's
// notice), the match stepper the search sheet leaves in the head, and the
// focus landing that brings a search hit home.
//
// Its props are App.tsx's contract and have not moved, except for the two
// facts the record's own drawing needs: the session's runtime (the reply's
// mark and name, T-4) and the viewer's home directory (a path is drawn
// relative to the session's cwd, then home-relative — never the eleven leading
// characters that are the same on every line).
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  SessionRow,
  TimelineMetaArtifact,
  TimelineMetaPlan,
  TimelineMetaTask,
  TimelineMetaWorkflow,
} from "@rosetta/protocol";
import { clockShort } from "../lib/format";
import { desktopHost } from "../lib/desktop-host";
import type { TimelineFeed } from "../lib/hooks";
import { useHeadOfDocument } from "../lib/transcript/head-of-document";
import { useTranscriptTail } from "../lib/transcript/use-transcript-tail";
import type { TimelineLayout } from "../lib/transcript/use-transcript-tail";
import { useTranscriptOpening } from "../lib/transcript/use-transcript-opening";
import { prefersReducedMotion } from "../lib/motion";
import type { PathFact } from "../lib/transcript";
import type { ProjectViewModel } from "../lib/model";
import type { DayEntry } from "../lib/transcript";
import { MatchStepper } from "./search";
import { getSessionTitle } from "./sessions/session-display";
import { DayMenu, markStuckPills } from "./transcript/DayPill";
import { TranscriptDocument } from "./transcript/Document";
import { TranscriptLoading } from "./transcript/TranscriptLoading";
import { useTranscriptModel } from "./transcript/document-model";
import { useFoldPreference } from "./transcript/fold-preference";
import { MessageViewerContext } from "./transcript/MessagePreview";
import { ThreadNavigationContext } from "./transcript/thread-navigation";
import { handleForMessage, messageFileSource } from "./transcript/message-file-source";
import type { FullMessage } from "./transcript/message-file-source";
import { attachmentStatus } from "./transcript/Attachments";
import { AuthoredMessage } from "./transcript/AuthoredMessage";
import { ActivityGlyph } from "./transcript/glyphs";
import { HeadOfDocument, TopFade } from "./transcript/HeadOfDocument";
import { ReceiptPill } from "./transcript/ReceiptPill";
import { TimelineMeta } from "./transcript/TimelineMeta";
import { buildThreadIndex, messageDomId } from "./transcript/thread-index";
import type { ThreadFocus } from "./transcript/thread-index";
import { ColumnHeading, EmptyState } from "./ui";
import { FileViewerDialog } from "./file-viewer";
import { ImageGalleryDialog } from "./file-viewer/ImageGalleryDialog";
import type { FileHandle, FileSource } from "./file-viewer";
import { attachmentFileSource, canOpenAttachment } from "./transcript/attachment-file-source";
import { attachmentGallery, attachmentGallerySource } from "./transcript/attachment-gallery";
import type { AttachmentOpener } from "./transcript/attachment-gallery";
import {
  handleForReadPath,
  transcriptReadFileSource,
} from "./transcript/read-file-source";
import {
  handleForTimelineMetaArtifact,
  handleForTimelineMetaPlan,
  timelineMetaFileSource,
} from "./transcript/timeline-meta-file-source";

import { scrollRowTo } from "./transcript/scroll-to-row";
import { activityCuts } from "./transcript/activity-plan";

export type { ThreadFocus } from "./transcript/thread-index";

/** The receipt lands beneath a pinned day pill, never over it (§08).
 * Measured rather than guessed: the scroller starts 46px down (the head band),
 * a pinned pill occupies 58–80, and the first pill still in the flow starts at
 * 124. 88 is the one band that clears both. */
const RECEIPT_TOP = 88;

interface TranscriptViewerTarget {
  readonly message?: FullMessage;
  readonly handle: FileHandle;
  readonly source: FileSource;
}

interface TimelineMetaFocus {
  readonly timelineId: string;
  readonly label: string;
  readonly kind: "task" | "workflow";
}

/** The stepper's face: the query, where the reader is, how many there are. */
export interface MatchStepperState {
  readonly query: string;
  readonly at: number;
  readonly total: number;
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
  /** The search sheet's tail: this session's matches, walked from the head. */
  readonly stepper: MatchStepperState | null;
  readonly onStepMatch: (delta: 1 | -1) => void;
  readonly onDismissStepper: () => void;
  /** The session's runtime, for the reply's mark and name (T-4). */
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly homeDirectory: string | null;
  readonly onOpenThread?: (sessionId: string) => void;
  readonly onBackToParent?: (() => void) | null;
}

export function TranscriptColumn(props: TranscriptColumnProps) {
  // Selection owns the document's disclosures, overlays and opening state.
  // Reset them in the selection commit, before the previous session can paint.
  return <SessionTranscript key={props.session?.id ?? "no-session"} {...props} />;
}

function SessionTranscript({
  session,
  duringBackfill,
  sessions,
  project,
  feed,
  focus,
  onFocusConsumed,
  onReceipt,
  stepper,
  onStepMatch,
  onDismissStepper,
  getRuntimeKind,
  homeDirectory,
  onOpenThread,
  onBackToParent,
}: TranscriptColumnProps) {
  const threadNavigation = useMemo(() => ({ depth: 0, open: onOpenThread ?? null }), [onOpenThread]);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const focusAfterOpening = useRef(false);
  const [highlightedTimelineId, setHighlightedTimelineId] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ top: number; dayKey: string | null } | null>(null);
  const [viewerTarget, setViewerTarget] = useState<TranscriptViewerTarget | null>(null);
  const [imageGallery, setImageGallery] = useState<ReturnType<typeof attachmentGallery> | null>(null);
  const [metaFocus, setMetaFocus] = useState<TimelineMetaFocus | null>(null);
  const viewerRequest = useRef(0);
  const anchorRequest = useRef(0);
  const focusTries = useRef(0);
  // One exact read per landing target, before any blind walk.
  const anchorTried = useRef<string | null>(null);
  const index = useMemo(() => buildThreadIndex(sessions), [sessions]);
  const fold = useFoldPreference();

  const requestedTimelineId =
    focus !== null && focus.sessionId === session?.id ? focus.timelineId : null;
  const targetTimelineId = requestedTimelineId ?? metaFocus?.timelineId ?? null;
  const targetIsMeta = requestedTimelineId === null && metaFocus !== null;

  const cuts = useMemo(() => activityCuts(feed.rows, feed.watermark), [feed.rows, feed.watermark]);
  const model = useTranscriptModel(feed.rows, session, homeDirectory, Date.now(), { cutBefore: cuts.cutBefore });
  const opening = useTranscriptOpening(feed.snapshotReady, focus !== null || metaFocus !== null);
  const loading = session !== null && opening.visible;
  useEffect(() => {
    if (loading || !focusAfterOpening.current) return;
    focusAfterOpening.current = false;
    scrollRef.current?.focus({ preventScroll: true });
  }, [loading]);

  // §08 — the head of the document: approach fetches and holds, the pull
  // reveals, and the landing keeps the reader exactly where they were.
  const headOfDocument = useHeadOfDocument(scrollRef, feed, {
    enabled: session !== null && !loading,
  });

  const tail = useTranscriptTail(
    scrollRef,
    session?.id ?? null,
    session !== null,
    focus !== null || metaFocus !== null,
    loading,
  );
  const displayedLayout = useRef<TimelineLayout | null>(null);
  const onTimelineLayout = useCallback((layout: TimelineLayout) => {
    displayedLayout.current = layout;
    tail.onLayout(layout);
    opening.onLayout(layout);
  }, [tail.onLayout, opening.onLayout, loading]);

  useEffect(() => () => {
    // A keyed session change unmounts this scope; invalidate any exact reads
    // that could otherwise deliver a receipt after the reader has moved on.
    viewerRequest.current += 1;
    anchorRequest.current += 1;
  }, []);

  const openAttachment: AttachmentOpener = (fact, row) => {
    viewerRequest.current += 1;
    if (!canOpenAttachment(fact)) {
      onReceipt(attachmentStatus(fact));
      return;
    }
    if (viewerTarget?.message === undefined) setViewerTarget(null);
    setImageGallery(attachmentGallery(fact, row));
  };

  const openMessage = useCallback((message: FullMessage) => {
    viewerRequest.current += 1;
    setViewerTarget({ handle: handleForMessage(message),
      source: message.recordedText?.type === "member" ? attachmentFileSource : messageFileSource, message });
  }, []);

  const openReadPath = (path: PathFact, previewBytes?: Uint8Array, name?: string) => {
    const request = (viewerRequest.current += 1);
    void handleForReadPath(path, previewBytes, name)
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
    anchorTried.current = null;
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
      session === null ||
      !feed.snapshotReady ||
      (feed.status !== "live" && feed.status !== "resyncing")
    ) {
      return;
    }
    // A fold draws several rows as one line, so the landing goes to the
    // element that CONTAINS the row rather than failing to find its own.
    const drawnBy = model.representatives.get(targetTimelineId) ?? null;
    if (drawnBy !== null) {
      const displayed = displayedLayout.current?.sessionId === session.id ? displayedLayout.current : null;
      const element = displayed?.elementForAnchor?.(targetTimelineId)
        ?? document.getElementById(messageDomId(session.id, drawnBy));
      const scroller = scrollRef.current;
      if (element && scroller) {
        tail.pause();
        // Scroll the transcript's own scroller, never `scrollIntoView`: that
        // walks every ancestor, and the room's overflow-hidden frame would
        // quietly scroll the search strip out of the window (seen 2026-09-09).
        if (element.getClientRects().length > 0) scrollRowTo(scroller, element, "center");
        setHighlightedTimelineId(element.dataset.timelineId ?? drawnBy);
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
    // timeline_meta resolves exact anchors before setting focus.
    if (targetIsMeta) {
      const kind = metaFocus.kind;
      setMetaFocus(null);
      onReceipt(`That ${kind} message is no longer available`);
      return;
    }
    // A search landing asks for its row exactly, once — the same read the
    // work index uses — and only walks blindly if that row lands but no line
    // draws it yet.
    if (anchorTried.current !== targetTimelineId) {
      anchorTried.current = targetTimelineId;
      const request = (anchorRequest.current += 1);
      void feed.loadAnchor(targetTimelineId)
        .then((loaded) => {
          if (request !== anchorRequest.current || loaded) return;
          onFocusConsumed();
          onReceipt("That message is no longer available");
        })
        .catch((error: unknown) => {
          if (request !== anchorRequest.current) return;
          onFocusConsumed();
          onReceipt(error instanceof Error ? error.message : "Could not load that message");
        });
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
  }, [targetTimelineId, targetIsMeta, metaFocus, session, feed, model, onFocusConsumed, onReceipt]);

  // The pill pins while the reader is inside its day, and returns to its
  // hairline the moment that day scrolls back into view.
  useEffect(() => {
    markStuckPills(scrollRef.current);
  }, [model.plan]);

  const range = (() => {
    if (!session) return "";
    if (!session.activity) return "Activity time unknown";
    return `${clockShort(session.activity.first_at)} → ${clockShort(session.activity.last_at)}`;
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
    tail.pause();
    scroller.scrollTo({
      top: target.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop - 4,
      behavior: prefersReducedMotion() ? "instant" : "smooth",
    });
    const prompt = target.querySelector<HTMLElement>(".tr-prompt");
    if (prompt === null) return;
    prompt.classList.add("is-landed");
    window.setTimeout(() => prompt.classList.remove("is-landed"), 1400);
  };

  return (
    <ThreadNavigationContext.Provider value={threadNavigation}>
    <MessageViewerContext.Provider value={openMessage}>
    <div
      className="ah-col ah-col--measure"
      style={{ position: "relative" }}
      data-fold={fold.folded ? "on" : "off"}
    >
      <header>
        {onBackToParent && <button type="button" className="tr-parent-link" title="Back to parent transcript"
          aria-label="Back to parent transcript" onClick={onBackToParent}>←</button>}
        {session === null ? (
          <div className="ah-colhead">
            <span className="ah-eyebrow">Transcript</span>
          </div>
        ) : (
          <ColumnHeading
            name={getSessionTitle(session)}
            tint={project?.tint ?? null}
            count={loading ? null : feed.rows.length}
            countTitle={`${feed.rows.length} messages loaded`}
          />
        )}
        <span className="ah-headacts">
          {stepper !== null && session !== null ? (
            <MatchStepper
              query={stepper.query}
              at={stepper.at}
              total={stepper.total}
              onStep={onStepMatch}
              onDismiss={onDismissStepper}
            />
          ) : (
            <span className="ah-caption tnum">{range}</span>
          )}
          {session !== null && (
            // §05 — the activity act, beside the range. Folded is the default;
            // the reader who wants the build log gets it in one press, and the
            // preference is per room, not per run.
            <button
              type="button"
              className="ah-iconbtn"
              aria-pressed={fold.folded}
              title={
                fold.folded
                  ? "Activity: conversation — show activity"
                  : "Activity: showing activity — return to conversation"
              }
              onClick={fold.toggle}
            >
              <ActivityGlyph />
            </button>
          )}
        </span>
      </header>
      <div className="tr-stage" data-ready={!loading || undefined}>
      <div className="tr-surface" inert={loading} aria-hidden={loading || undefined}>
      {session !== null && feed.timelineMeta !== null && (
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
        tabIndex={0}
        role="region"
        aria-label="Session messages"
        aria-busy={loading}
        onScroll={() => markStuckPills(scrollRef.current)}
      >
        {/* The top fade is the pinned pill's ground, and it must be the first
            child of the scroller: `useHeadOfDocument` writes `--frost` here. */}
        <TopFade />
        {session === null ? (
          <EmptyState
            title="No session open"
            description="Pick a session — or press ⌘K to search, which is the front door."
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
                onTimelineLayout={onTimelineLayout}
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
                    startedAt={session.activity?.first_at ?? null}
                    finding={targetTimelineId === null ? null : "an anchor"}
                  />
                }
              />
            </div>
          </div>
        )}
      </div>
      </div>
      {loading && <TranscriptLoading
        error={feed.snapshotReady ? null : feed.error ?? null}
        waitingForTeam={opening.waitingForTeam}
        seeking={focus !== null}
        canContinue={feed.snapshotReady && (opening.waitingForTeam || focus !== null)}
        indexing={duringBackfill && session.stream_state !== "indexed"}
        onRetry={() => { focusAfterOpening.current = true; feed.retry(); }}
        onContinue={() => {
          focusAfterOpening.current = true;
          if (focus !== null) onFocusConsumed();
          tail.latest();
          opening.continueReading();
        }}
      />}
      </div>
      {menu !== null && (
        <DayMenu
          days={model.days}
          currentDayKey={menu.dayKey}
          top={menu.top}
          onJump={jumpToDay}
          onDismiss={() => setMenu(null)}
        />
      )}
      <ReceiptPill receipt={headOfDocument.receipt} top={RECEIPT_TOP} />
      {session !== null && (
        <button
          type="button"
          className="ah-newpill"
          data-visible={(!loading && tail.away) || undefined}
          data-unread={tail.unseen > 0 || undefined}
          inert={loading || !tail.away}
          aria-hidden={loading || !tail.away}
          aria-label={tail.unseen > 0
            ? `Scroll to latest · ${tail.unseen} new ${tail.unseen === 1 ? "message" : "messages"}`
            : "Scroll to latest message"}
          onClick={() => {
            // Keep keyboard navigation on the reading surface after the
            // button fades, instead of leaving focus on an inert control.
            scrollRef.current?.focus({ preventScroll: true });
            tail.latest();
          }}
        >
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
            <path d="M8 3v10m-4-4 4 4 4-4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
          <span>{tail.unseen > 0 ? `${tail.unseen} new ${tail.unseen === 1 ? "message" : "messages"}` : "Latest"}</span>
          {tail.unseen > 0 && <i aria-hidden="true" />}
        </button>
      )}
      <span className="ah-tail-announcement" role="status" aria-live="polite" aria-atomic="true">
        {tail.away && tail.unseen > 0
          ? `${tail.unseen} new ${tail.unseen === 1 ? "message" : "messages"}. Scroll to latest to read.`
          : ""}
      </span>
      <FileViewerDialog
        renderMarkdown={viewerTarget?.message === undefined ? undefined : (text) => <AuthoredMessage text={text}
          messageKey={viewerTarget.message!.key} role={viewerTarget.message!.role} preview={false}
          attachments={viewerTarget.message!.attachments ?? []} onOpenAttachment={openAttachment} onReceipt={onReceipt} />}
        handle={viewerTarget?.handle ?? null}
        source={viewerTarget?.source ?? attachmentFileSource}
        tint={project?.tint ?? null}
        homeDirectory={homeDirectory}
        onClose={() => {
          viewerRequest.current += 1;
          setViewerTarget(null);
          setImageGallery(null);
        }}
        onReceipt={onReceipt}
        openInBrowser={desktopHost()?.openInBrowser ?? null}
      />
      {imageGallery && <ImageGalleryDialog {...imageGallery} source={attachmentGallerySource}
        tint={project?.tint ?? null} homeDirectory={homeDirectory}
        onClose={() => setImageGallery(null)} onReceipt={onReceipt}
        openInBrowser={desktopHost()?.openInBrowser ?? null} />}
    </div>
    </MessageViewerContext.Provider>
    </ThreadNavigationContext.Provider>
  );
}
