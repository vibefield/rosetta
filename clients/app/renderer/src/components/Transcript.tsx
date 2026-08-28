// Column ④ — the transcript: a document, not a chat log. Prose owns the
// measure (H-1); a contiguous run of tool calls is ONE object that opens;
// the watermark marks where live began in YOUR reading and never chases the
// tail; arrival is a notice, not a stolen scroll.
import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { MessageRow, SearchHit, SessionRow } from "@rosetta/protocol";
import { clock, clockShort, tokenText } from "../lib/format";
import type { TimelineFeed } from "../lib/hooks";
import { ChevronGlyph } from "../lib/icons";
import type { DocBlock, ProjectViewModel } from "../lib/model";
import { documentBlocks, parseSnippet, verbSummary } from "../lib/model";
import { getSessionTitle } from "./sessions/session-display";

function fmtDur(ms: number | null): string {
  if (ms === null || ms <= 0) return "";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
}

function WorkBlock({ block }: { block: Extract<DocBlock, { type: "work" }> }) {
  const [open, setOpen] = useState(false);
  const { parts, failed } = verbSummary(block.calls);
  return (
    <div className="ah-work" data-open={open}>
      <button
        type="button"
        className="ah-work__head"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
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
          {block.calls.map((call) => (
            <div className="ah-rung" key={call.row.timeline_id} data-failed={call.failed || undefined}>
              <span className="ah-rung__verb">{call.verb}</span>
              <span className="ah-rung__target" title={call.target}>
                {call.target}
              </span>
              <span className="ah-rung__result">{call.result}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function roleName(row: MessageRow): string {
  switch (row.kind) {
    case "user":
      return "You";
    case "agent":
      return row.model ?? "Agent";
    case "thinking":
      return "Reasoning";
    case "summary":
      return "Summary";
    case "system":
      return row.subtype ?? "System";
    default:
      return row.model ?? "Agent";
  }
}

export function TranscriptColumn({
  session,
  project,
  feed,
  focusId,
  onFocusConsumed,
  onReceipt,
  searchHits,
  searchCounts,
  onOpenHit,
}: {
  session: SessionRow | null;
  project: ProjectViewModel | null;
  feed: TimelineFeed;
  /** A timeline_id to land on and ring once (search → continuation). */
  focusId: string | null;
  onFocusConsumed: () => void;
  onReceipt: (text: string) => void;
  /** Search mode: grouped hits replace the document. */
  searchHits:
    | { project: ProjectViewModel; session: SessionRow; hits: SearchHit[] }[]
    | null;
  searchCounts: { matches: number; sessions: number; projects: number } | null;
  onOpenHit: (session: SessionRow, hit: SearchHit) => void;
}) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const seenMax = useRef<number>(-1);
  const [unseen, setUnseen] = useState(0);
  const [rung, setRung] = useState<string | null>(null);
  const focusTries = useRef(0);

  const searching = searchHits !== null;
  const blocks = useMemo(() => documentBlocks(feed.rows), [feed.rows]);
  const maxOrdinal = feed.rows.length ? feed.rows[feed.rows.length - 1].ordinal : -1;
  const live = session?.stream_state === "streaming";

  const atFoot = () => {
    const el = scrollRef.current;
    if (!el) return true;
    return el.scrollTop + el.clientHeight >= el.scrollHeight - 32;
  };

  // Arrivals: at the foot they simply appear (and the foot holds); scrolled
  // up, a pill states the count and waits. Never a stolen viewport.
  const wasAtFoot = useRef(true);
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    if (wasAtFoot.current) {
      el.scrollTop = el.scrollHeight;
      seenMax.current = maxOrdinal;
      setUnseen(0);
    } else if (feed.watermark !== null) {
      const fresh = feed.rows.filter(
        (r) => r.ordinal > feed.watermark! && r.ordinal > seenMax.current,
      ).length;
      if (fresh > 0) setUnseen((u) => u + fresh);
      seenMax.current = maxOrdinal;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [maxOrdinal]);

  // New session: forget the old scroll bookkeeping.
  useEffect(() => {
    seenMax.current = -1;
    wasAtFoot.current = true;
    setUnseen(0);
    focusTries.current = 0;
  }, [session?.id]);

  // Landing a search hit: scroll to it once its row exists; page back a
  // bounded number of times if it does not.
  useEffect(() => {
    if (focusId === null || searching) return;
    const present = feed.rows.some((r) => r.timeline_id === focusId);
    if (present) {
      const el = document.getElementById(`msg-${focusId}`);
      if (el) {
        wasAtFoot.current = false;
        el.scrollIntoView({ block: "center" });
        setRung(focusId);
        onFocusConsumed();
        window.setTimeout(() => setRung(null), 1500);
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
  }, [focusId, feed, searching, onFocusConsumed, onReceipt]);

  const range = (() => {
    if (!session) return "";
    const from = clockShort(session.started_at);
    return live ? `${from} → now` : `${from} → ${clockShort(session.updated_at)}`;
  })();

  return (
    <div className="ah-col ah-col--measure" style={{ position: "relative" }}>
      <header>
        <div className="ah-colhead" style={project ? ({ "--tint": project.tint } as CSSProperties) : undefined}>
          {searching ? (
            <span className="ah-eyebrow">Matches</span>
          ) : session ? (
            <>
              <i />
              <strong title={getSessionTitle(session)}>{getSessionTitle(session)}</strong>
              <span className="ah-bubble" title={`${feed.rows.length} messages loaded`}>
                {feed.rows.length}
              </span>
            </>
          ) : (
            <span className="ah-eyebrow">Transcript</span>
          )}
        </div>
        <span className="ah-headacts">
          <span className="ah-caption tnum">
            {searching && searchCounts
              ? `${searchCounts.matches} ${searchCounts.matches === 1 ? "match" : "matches"} · ${searchCounts.sessions} ${searchCounts.sessions === 1 ? "session" : "sessions"} · ${searchCounts.projects} ${searchCounts.projects === 1 ? "project" : "projects"}`
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
            {searchHits.map((group) => (
              <div key={group.session.id}>
                <div className="ah-group" style={{ "--tint": group.project.tint } as CSSProperties}>
                  <i />
                  {group.project.row.name} · {getSessionTitle(group.session)}
                  <span className="ah-hitcount">{group.hits.length}</span>
                </div>
                {group.hits.map((hit) => (
                  <button
                    type="button"
                    className="ah-hit"
                    key={hit.timeline_id}
                    onClick={() => onOpenHit(group.session, hit)}
                  >
                    <p>
                      {parseSnippet(hit.snippet).map((part, i) =>
                        part.mark ? <mark key={i}>{part.text}</mark> : <span key={i}>{part.text}</span>,
                      )}
                    </p>
                  </button>
                ))}
              </div>
            ))}
            {searchHits.length === 0 && (
              <div className="ah-empty">
                <strong>Nothing matched</strong>
                <p>No indexed message contains that.</p>
              </div>
            )}
          </div>
        ) : session === null ? (
          <div className="ah-empty">
            <strong>No session open</strong>
            <p>Pick a session — or search, which is the front door.</p>
          </div>
        ) : (
          <div className="ah-transcript">
            {feed.canLoadEarlier && (
              <button type="button" className="ah-loadmore" onClick={feed.loadEarlier}>
                load earlier
              </button>
            )}
            {blocks.map((block) => {
              if (block.type === "work") {
                const after =
                  feed.watermark !== null && block.calls[0].row.ordinal > feed.watermark;
                return (
                  <div key={block.key} data-after-watermark={after || undefined}>
                    <WorkBlock block={block} />
                  </div>
                );
              }
              const row = block.row;
              const marked =
                feed.watermark !== null &&
                row.ordinal > feed.watermark &&
                blocks.find((b) => b.type !== "work" && b.row.ordinal > feed.watermark!) === block;
              return (
                <div key={row.timeline_id}>
                  {marked && <div className="ah-mark ah-watermark">live from here</div>}
                  <div
                    className="ah-tmsg"
                    id={`msg-${row.timeline_id}`}
                    data-hit={rung === row.timeline_id || undefined}
                    data-kind={row.kind}
                  >
                    <div className="ah-tmsg__head">
                      {roleName(row)}{" "}
                      {row.ts !== null && (
                        <time
                          title="Copy an anchor to this message"
                          onClick={() => {
                            navigator.clipboard
                              .writeText(`${row.session_id}#${row.timeline_id}`)
                              .then(() => onReceipt(`Anchor copied · ${clock(row.ts!)}`))
                              .catch(() => onReceipt("The clipboard refused the anchor"));
                          }}
                        >
                          {clock(row.ts)}
                        </time>
                      )}
                      {row.usage && (
                        <span className="ah-caption tnum" style={{ marginLeft: "auto" }}>
                          {tokenText(
                            (row.usage.input ?? 0) +
                              (row.usage.output ?? 0) +
                              (row.usage.thinking ?? 0),
                          )}
                        </span>
                      )}
                    </div>
                    {(row.content ?? "").split("\n\n").map((para, i) =>
                      para.trim() === "" ? null : <p key={i}>{para}</p>,
                    )}
                  </div>
                </div>
              );
            })}
            {blocks.length === 0 && feed.status !== "connecting" && (
              <div className="ah-empty">
                <strong>Nothing readable yet</strong>
                <p>This session has no readable timeline rows in the index.</p>
              </div>
            )}
            {live && (
              <div className="ah-mark ah-watermark" title="this session is still running">
                live
              </div>
            )}
          </div>
        )}
      </div>
      {unseen > 0 && (
        <button
          type="button"
          className="ah-newpill"
          onClick={() => {
            const el = scrollRef.current;
            if (el) el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
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
