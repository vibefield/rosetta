// Column ④ — the transcript: a document, not a chat log. Child sessions are
// document structure, not sibling navigation: a proven Delegate link places a
// lazy subthread beneath its tool call; a parent-only link is interleaved by
// session start time without pretending that timestamp proximity proves cause.
import { useEffect, useMemo, useRef, useState } from "react";
import type { SearchHit, SessionRow, TimelineEntry } from "@rosetta/protocol";
import { clock, clockShort, tokenText } from "../lib/format";
import { useTimeline } from "../lib/hooks";
import type { TimelineFeed } from "../lib/hooks";
import { ChevronGlyph } from "../lib/icons";
import type { DocBlock, ProjectViewModel } from "../lib/model";
import {
  attachmentLabel,
  citationLabel,
  documentBlocks,
  parseSnippet,
  timelineAttachments,
  timelineCitations,
  timelineText,
  verbSummary,
} from "../lib/model";
import type { SearchResultsViewModel } from "../lib/search-results";
import { parseMarkdown } from "./file-viewer/markdown";
import { MarkdownBlocks } from "./file-viewer/markdown/render";
import { getSessionTitle } from "./sessions/session-display";
import { ColumnHeading, EmptyState, ResultGroupHeader } from "./ui";

export interface ThreadFocus {
  readonly sessionId: string;
  readonly timelineId: string;
}

interface ThreadIndex {
  readonly byId: ReadonlyMap<string, SessionRow>;
  readonly childrenByParent: ReadonlyMap<string, readonly SessionRow[]>;
}

function buildThreadIndex(sessions: readonly SessionRow[]): ThreadIndex {
  const byId = new Map(sessions.map((session) => [session.id, session]));
  const childrenByParent = new Map<string, SessionRow[]>();
  for (const session of sessions) {
    if (session.parent_session_id === null) continue;
    const children = childrenByParent.get(session.parent_session_id);
    if (children) children.push(session);
    else childrenByParent.set(session.parent_session_id, [session]);
  }
  for (const children of childrenByParent.values()) {
    children.sort(
      (left, right) =>
        left.started_at - right.started_at || left.id.localeCompare(right.id),
    );
  }
  return { byId, childrenByParent };
}

function sessionContains(
  ancestorId: string,
  candidateId: string,
  sessions: ReadonlyMap<string, SessionRow>,
): boolean {
  let current: string | null = candidateId;
  const visited = new Set<string>();
  while (current !== null && !visited.has(current)) {
    if (current === ancestorId) return true;
    visited.add(current);
    current = sessions.get(current)?.parent_session_id ?? null;
  }
  return false;
}

function messageDomId(sessionId: string, timelineId: string): string {
  return `msg-${sessionId}-${timelineId}`;
}

function docBlockKey(block: DocBlock): string {
  return block.type === "work" ? block.key : block.row.timeline_id;
}

function docBlockStartTime(block: DocBlock): number | null {
  if (block.type !== "work") return block.row.at;
  const timestamps = block.calls
    .map((call) => call.row.at)
    .filter((timestamp): timestamp is number => timestamp !== null);
  return timestamps.length === 0 ? null : Math.min(...timestamps);
}

/**
 * Place a child before the first document block known to start after it. Rows
 * with no timestamp retain their canonical ordinal position; they are not
 * moved around on the strength of missing evidence.
 */
function timestampInsertionIndex(blocks: readonly DocBlock[], startedAt: number): number {
  const index = blocks.findIndex((block) => {
    const blockStartedAt = docBlockStartTime(block);
    return blockStartedAt !== null && blockStartedAt > startedAt;
  });
  return index === -1 ? blocks.length : index;
}

function linkedChildSessionIds(row: TimelineEntry): readonly string[] {
  if (row.body.type !== "tool_use") return [];
  const action = row.body.tool.action;
  switch (action.type) {
    case "delegate":
      return action.child_session_id === null ? [] : [action.child_session_id];
    case "collaborate":
      return action.linked_session_id === null ? [] : [action.linked_session_id];
    default:
      return [];
  }
}

function fmtDur(ms: number | null): string {
  if (ms === null || ms <= 0) return "";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
}

interface WorkBlockProps {
  readonly duringBackfill: boolean;
  readonly block: Extract<DocBlock, { type: "work" }>;
  readonly session: SessionRow;
  readonly childrenByTimelineId: ReadonlyMap<string, readonly SessionRow[]>;
  readonly index: ThreadIndex;
  readonly focus: ThreadFocus | null;
  readonly highlightedTimelineId: string | null;
  readonly onFocusConsumed: () => void;
  readonly onReceipt: (text: string) => void;
}

function WorkBlock({
  duringBackfill,
  block,
  session,
  childrenByTimelineId,
  index,
  focus,
  highlightedTimelineId,
  onFocusConsumed,
  onReceipt,
}: WorkBlockProps) {
  const focusedCall = block.calls.some(
    (call) =>
      focus?.sessionId === session.id && focus.timelineId === call.row.timeline_id,
  );
  const focusedChild = block.calls.some((call) =>
    (childrenByTimelineId.get(call.row.timeline_id) ?? []).some(
      (child) =>
        focus !== null && sessionContains(child.id, focus.sessionId, index.byId),
    ),
  );
  const forceOpen = focusedCall || focusedChild;
  const [open, setOpen] = useState(forceOpen);
  useEffect(() => {
    if (forceOpen) setOpen(true);
  }, [forceOpen]);

  const { parts, failed } = verbSummary(block.calls);
  return (
    <div className="ah-work" data-open={open}>
      <button
        type="button"
        className="ah-work__head"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="ah-work__summary">
          {parts.join(" · ")}
          {failed > 0 && (
            <>
              {" · "}
              <b>{failed} failed</b>
            </>
          )}
        </span>
        <span className="ah-work__time">{fmtDur(block.durMs)}</span>
        <span className="ah-work__chev">
          <ChevronGlyph />
        </span>
      </button>
      {open && (
        <div className="ah-work__ladder" style={{ display: "block" }}>
          {block.calls.map((call) => {
            const children = childrenByTimelineId.get(call.row.timeline_id) ?? [];
            return (
              <div className="ah-rungwrap" key={call.row.timeline_id}>
                <div
                  className="ah-rung"
                  id={messageDomId(session.id, call.row.timeline_id)}
                  data-failed={call.failed || undefined}
                  data-hit={highlightedTimelineId === call.row.timeline_id || undefined}
                >
                  <span className="ah-rung__verb">{call.verb}</span>
                  <span className="ah-rung__target" title={call.target}>
                    {call.target}
                  </span>
                  <span className="ah-rung__result">{call.result}</span>
                </div>
                {children.map((child) => (
                  <Subthread
                    key={child.id}
                    session={child}
                    index={index}
                    focus={focus}
                    anchored
                    duringBackfill={duringBackfill}
                    onFocusConsumed={onFocusConsumed}
                    onReceipt={onReceipt}
                  />
                ))}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function roleName(row: TimelineEntry): string {
  switch (row.body.type) {
    case "user":
      return "You";
    case "agent":
      return row.model ?? "Agent";
    case "thinking":
      return "Reasoning";
    case "summary":
      return `${row.body.purpose[0].toUpperCase()}${row.body.purpose.slice(1)} summary`;
    case "notice":
      return "System";
    case "tool_use":
      return row.model ?? "Agent";
  }
}

function reportedTokenTotal(row: TimelineEntry): number | null {
  const reported = [row.usage.input, row.usage.output, row.usage.thinking];
  if (reported.every((value) => value === null)) return null;
  return reported.reduce<number>((total, value) => total + (value ?? 0), 0);
}

function MarkdownMessage({ text }: { readonly text: string }) {
  const blocks = useMemo(() => parseMarkdown(text), [text]);
  return (
    <div className="ah-tmsg__markdown ah-markdown ah-syntax">
      <MarkdownBlocks blocks={blocks} />
    </div>
  );
}

interface ThreadDocumentProps {
  readonly session: SessionRow;
  /** The cold backfill is running: a streaming session is being INDEXED by
   * the daemon, not run by an agent — the watermark must not claim life. */
  readonly duringBackfill: boolean;
  readonly feed: TimelineFeed;
  readonly index: ThreadIndex;
  readonly focus: ThreadFocus | null;
  readonly highlightedTimelineId: string | null;
  readonly onFocusConsumed: () => void;
  readonly onReceipt: (text: string) => void;
  readonly nested?: boolean;
}

function ThreadDocument({
  session,
  duringBackfill,
  feed,
  index,
  focus,
  highlightedTimelineId,
  onFocusConsumed,
  onReceipt,
  nested = false,
}: ThreadDocumentProps) {
  const blocks = useMemo(() => documentBlocks(feed.rows), [feed.rows]);
  const directChildren = index.childrenByParent.get(session.id) ?? [];
  const placement = useMemo(() => {
    const childById = new Map(directChildren.map((child) => [child.id, child]));
    const claimed = new Set<string>();
    const byTimelineId = new Map<string, SessionRow[]>();
    for (const block of blocks) {
      if (block.type !== "work") continue;
      for (const call of block.calls) {
        for (const childId of linkedChildSessionIds(call.row)) {
          const child = childById.get(childId);
          if (!child || claimed.has(child.id)) continue;
          claimed.add(child.id);
          const children = byTimelineId.get(call.row.timeline_id);
          if (children) children.push(child);
          else byTimelineId.set(call.row.timeline_id, [child]);
        }
      }
    }

    const byBlockIndex = new Map<number, SessionRow[]>();
    for (const child of directChildren) {
      if (claimed.has(child.id)) continue;
      const index = timestampInsertionIndex(blocks, child.started_at);
      const children = byBlockIndex.get(index);
      if (children) children.push(child);
      else byBlockIndex.set(index, [child]);
    }
    return {
      byTimelineId,
      byBlockIndex,
    };
  }, [blocks, directChildren]);

  const firstAfterWatermark =
    feed.watermark === null
      ? null
      : blocks.find((block) =>
          block.type === "work"
            ? block.calls[0].row.ordinal > feed.watermark!
            : block.row.ordinal > feed.watermark!,
        );
  const firstAfterWatermarkKey = firstAfterWatermark
    ? docBlockKey(firstAfterWatermark)
    : null;

  const timestampedSubthreads = (blockIndex: number) =>
    (placement.byBlockIndex.get(blockIndex) ?? []).map((child) => (
      <Subthread
        key={child.id}
        session={child}
        index={index}
        focus={focus}
        anchored={false}
        duringBackfill={duringBackfill}
        onFocusConsumed={onFocusConsumed}
        onReceipt={onReceipt}
      />
    ));

  return (
    <>
      {feed.canLoadEarlier && (
        <button type="button" className="ah-loadmore" onClick={feed.loadEarlier}>
          load earlier
        </button>
      )}
      {timestampedSubthreads(0)}
      {blocks.map((block, blockIndex) => {
        const marked = docBlockKey(block) === firstAfterWatermarkKey;
        if (block.type === "work") {
          return (
            <div key={block.key} data-after-watermark={marked || undefined}>
              {marked && <div className="ah-mark ah-watermark">live from here</div>}
              <WorkBlock
                block={block}
                session={session}
                duringBackfill={duringBackfill}
                childrenByTimelineId={placement.byTimelineId}
                index={index}
                focus={focus}
                highlightedTimelineId={highlightedTimelineId}
                onFocusConsumed={onFocusConsumed}
                onReceipt={onReceipt}
              />
              {timestampedSubthreads(blockIndex + 1)}
            </div>
          );
        }
        const row = block.row;
        const text = timelineText(row) ?? "";
        const attachments = timelineAttachments(row);
        const citations = timelineCitations(row);
        const tokenTotal = reportedTokenTotal(row);
        return (
          <div key={row.timeline_id}>
            {marked && <div className="ah-mark ah-watermark">live from here</div>}
            <div
              className="ah-tmsg"
              id={messageDomId(session.id, row.timeline_id)}
              data-hit={highlightedTimelineId === row.timeline_id || undefined}
              data-kind={row.body.type}
            >
              <div className="ah-tmsg__head">
                {roleName(row)}{" "}
                {row.at !== null && (
                  <time
                    title="Copy an anchor to this message"
                    onClick={() => {
                      navigator.clipboard
                        .writeText(`${row.session_id}#${row.timeline_id}`)
                        .then(() => onReceipt(`Anchor copied · ${clock(row.at!)}`))
                        .catch(() => onReceipt("The clipboard refused the anchor"));
                    }}
                  >
                    {clock(row.at)}
                  </time>
                )}
                {tokenTotal !== null && (
                  <span className="ah-caption tnum" style={{ marginLeft: "auto" }}>
                    {tokenText(tokenTotal)}
                  </span>
                )}
              </div>
              {row.body.type === "user" || row.body.type === "agent" ? (
                <MarkdownMessage text={text} />
              ) : (
                text.split("\n\n").map((paragraph, index) =>
                  paragraph.trim() === "" ? null : <p key={index}>{paragraph}</p>,
                )
              )}
              {attachments.map((attachment, index) => (
                <p className="ah-caption" key={`attachment-${index}`}>
                  {attachmentLabel(attachment)}
                </p>
              ))}
              {citations.map((citation, index) => (
                <p className="ah-caption" key={`citation-${index}`}>
                  Source · {citationLabel(citation)}
                </p>
              ))}
            </div>
            {timestampedSubthreads(blockIndex + 1)}
          </div>
        );
      })}
      {blocks.length === 0 && directChildren.length === 0 && feed.status !== "connecting" &&
        (nested ? (
          <p className="ah-subthread__empty">
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
        <div
          className="ah-mark ah-watermark"
          title={
            duringBackfill
              ? "rosettad is reading this transcript"
              : "this session is still running"
          }
        >
          {duringBackfill ? "indexing" : "live"}
        </div>
      )}
    </>
  );
}

interface SubthreadProps {
  readonly session: SessionRow;
  readonly duringBackfill: boolean;
  readonly index: ThreadIndex;
  readonly focus: ThreadFocus | null;
  readonly anchored: boolean;
  readonly onFocusConsumed: () => void;
  readonly onReceipt: (text: string) => void;
}

function subthreadTitle(session: SessionRow): string {
  if (session.title !== null) return session.title;
  if (session.summary !== null) return session.summary;
  const native = session.native_session_id.split("/").at(-1) ?? session.native_session_id;
  return `Subagent ${native.length > 12 ? native.slice(-12) : native}`;
}

function Subthread({
  session,
  duringBackfill,
  index,
  focus,
  anchored,
  onFocusConsumed,
  onReceipt,
}: SubthreadProps) {
  const onFocusPath =
    focus !== null && sessionContains(session.id, focus.sessionId, index.byId);
  const [open, setOpen] = useState(onFocusPath);
  const effectiveOpen = open || onFocusPath;
  const feed = useTimeline(effectiveOpen ? session.id : null);
  const [highlightedTimelineId, setHighlightedTimelineId] = useState<string | null>(null);
  const focusTries = useRef(0);
  const targetTimelineId = focus?.sessionId === session.id ? focus.timelineId : null;

  useEffect(() => {
    if (onFocusPath) setOpen(true);
  }, [onFocusPath]);

  useEffect(() => {
    focusTries.current = 0;
    setHighlightedTimelineId(null);
  }, [targetTimelineId]);

  useEffect(() => {
    if (targetTimelineId === null || feed.status === "connecting") return;
    const present = feed.rows.some((row) => row.timeline_id === targetTimelineId);
    if (present) {
      const element = document.getElementById(messageDomId(session.id, targetTimelineId));
      if (element) {
        element.scrollIntoView({ block: "center" });
        setHighlightedTimelineId(targetTimelineId);
        onFocusConsumed();
        window.setTimeout(() => setHighlightedTimelineId(null), 1500);
      }
      return;
    }
    if (feed.canLoadEarlier && focusTries.current < 8) {
      focusTries.current += 1;
      feed.loadEarlier();
    } else if (!feed.canLoadEarlier) {
      onFocusConsumed();
      onReceipt("That subthread message is older than the loaded history");
    }
  }, [targetTimelineId, session.id, feed, onFocusConsumed, onReceipt]);

  return (
    <section className="ah-subthread" data-anchored={anchored || undefined} data-open={effectiveOpen}>
      <button
        type="button"
        className="ah-subthread__head"
        aria-expanded={effectiveOpen}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="ah-subthread__rail" />
        <span className="ah-subthread__title">{subthreadTitle(session)}</span>
        <span className="ah-subthread__meta">
          {effectiveOpen && feed.rows.length > 0
            ? `${feed.rows.length} messages`
            : session.stream_state === "indexed"
              ? "subthread"
              : session.stream_state}
        </span>
        <span className="ah-subthread__chev">
          <ChevronGlyph />
        </span>
      </button>
      {effectiveOpen && (
        <div className="ah-subthread__body">
          <ThreadDocument
            session={session}
            duringBackfill={duringBackfill}
            feed={feed}
            index={index}
            focus={focus}
            highlightedTimelineId={highlightedTimelineId}
            onFocusConsumed={onFocusConsumed}
            onReceipt={onReceipt}
            nested
          />
        </div>
      )}
    </section>
  );
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
}: TranscriptColumnProps) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const seenMax = useRef<number>(-1);
  const [unseen, setUnseen] = useState(0);
  const [highlightedTimelineId, setHighlightedTimelineId] = useState<string | null>(null);
  const focusTries = useRef(0);
  const index = useMemo(() => buildThreadIndex(sessions), [sessions]);

  const searching = searchResults !== null;
  const maxOrdinal = feed.rows.length ? feed.rows[feed.rows.length - 1].ordinal : -1;
  const live = session?.stream_state === "streaming";
  const targetTimelineId =
    focus !== null && focus.sessionId === session?.id ? focus.timelineId : null;

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
    focusTries.current = 0;
  }, [session?.id]);

  useEffect(() => {
    focusTries.current = 0;
    setHighlightedTimelineId(null);
  }, [targetTimelineId]);

  useEffect(() => {
    if (targetTimelineId === null || searching || feed.status === "connecting" || session === null) {
      return;
    }
    const present = feed.rows.some((row) => row.timeline_id === targetTimelineId);
    if (present) {
      const element = document.getElementById(messageDomId(session.id, targetTimelineId));
      if (element) {
        wasAtFoot.current = false;
        element.scrollIntoView({ block: "center" });
        setHighlightedTimelineId(targetTimelineId);
        onFocusConsumed();
        window.setTimeout(() => setHighlightedTimelineId(null), 1500);
      }
      return;
    }
    if (feed.canLoadEarlier && focusTries.current < 8) {
      focusTries.current += 1;
      feed.loadEarlier();
    } else if (!feed.canLoadEarlier) {
      onFocusConsumed();
      onReceipt("That message is older than the loaded history");
    }
  }, [targetTimelineId, session, feed, searching, onFocusConsumed, onReceipt]);

  const range = (() => {
    if (!session) return "";
    const from = clockShort(session.started_at);
    return live ? `${from} → now` : `${from} → ${clockShort(session.updated_at)}`;
  })();

  return (
    <div className="ah-col ah-col--measure" style={{ position: "relative" }}>
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
        </span>
      </header>
      <div
        className="ah-scroll"
        data-flush=""
        ref={scrollRef}
        onScroll={() => {
          wasAtFoot.current = atFoot();
          if (wasAtFoot.current && unseen > 0) setUnseen(0);
        }}
      >
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
                        {parseSnippet(hit.snippet).map((part, index) =>
                          part.mark ? (
                            <mark key={index}>{part.text}</mark>
                          ) : (
                            <span key={index}>{part.text}</span>
                          ),
                        )}
                      </p>
                    </button>
                  ))}
                </div>
              )),
            )}
            {searchResults.counts.matches === 0 && (
              <EmptyState
                title="Nothing matched"
                description="No indexed message contains that."
              />
            )}
          </div>
        ) : session === null ? (
          <EmptyState
            title="No session open"
            description="Pick a session — or search, which is the front door."
          />
        ) : (
          <div className="ah-transcript">
            <ThreadDocument
              session={session}
              duringBackfill={duringBackfill}
              feed={feed}
              index={index}
              focus={focus}
              highlightedTimelineId={highlightedTimelineId}
              onFocusConsumed={onFocusConsumed}
              onReceipt={onReceipt}
            />
          </div>
        )}
      </div>
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
    </div>
  );
}
