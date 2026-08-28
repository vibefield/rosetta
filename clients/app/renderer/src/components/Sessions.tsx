// Column ② — sessions, or the search index at session level. One row
// component serves both; search only adds a hit count to the facts line.
// The state is the dot and only the dot (§2.5 meanings, unchanged).
import type { CSSProperties } from "react";
import type { SessionRow, UsageResponse } from "@rosetta/protocol";
import { AgentMark, agentLabel } from "../lib/agents";
import { dayLabel, duration, lastActive, tokenText } from "../lib/format";
import type { ProjectViewModel } from "../lib/model";
import { ProjectSummaryPanel } from "./SummaryPanels";

export interface SessionFacts {
  tokens: number;
  messages: number;
}

function dotTone(s: SessionRow): { tone: string | null; live: boolean; word: string } {
  switch (s.stream_state) {
    case "streaming":
      return { tone: "green", live: true, word: "live" };
    case "pending":
      return { tone: "orange", live: false, word: "not yet indexed" };
    default:
      return { tone: null, live: false, word: "done" };
  }
}

export function sessionTitle(s: SessionRow): string {
  return s.title ?? s.summary ?? s.native_session_id;
}

export function SessionRowButton({
  session,
  facts,
  kind,
  active,
  hits,
  nowMs,
  onSelect,
}: {
  session: SessionRow;
  facts: SessionFacts | null;
  kind: string;
  active: boolean;
  hits: number | null;
  nowMs: number;
  onSelect: (session: SessionRow) => void;
}) {
  const { tone, live, word } = dotTone(session);
  const title = sessionTitle(session);
  const last = lastActive(session.updated_at, nowMs);
  return (
    <button
      type="button"
      className={`ah-sessrow${active ? " is-active is-focused" : ""}`}
      data-role={session.role}
      onClick={() => onSelect(session)}
      aria-label={`${title} · ${word} · last active ${last}`}
    >
      <i
        className="ah-dot"
        data-tone={tone ?? undefined}
        data-live={live ? "" : undefined}
        title={word}
      />
      <span className="ah-sessrow__l1">
        <strong title={title}>{title}</strong>
        <AgentMark kind={kind} note={agentLabel(kind)} />
      </span>
      <span className="ah-sessmeta">
        {session.role !== "main" && (
          <>
            <span>{session.role}</span>
            <span>·</span>
          </>
        )}
        {hits !== null && (
          <>
            <span>
              {hits} {hits === 1 ? "hit" : "hits"}
            </span>
            <span>·</span>
          </>
        )}
        {facts !== null && (
          <>
            <span>{facts.messages} msgs</span>
            <span>·</span>
          </>
        )}
        <span>{duration(session.started_at, session.updated_at)}</span>
        {facts !== null && facts.tokens > 0 && (
          <>
            <span>·</span>
            <span>{tokenText(facts.tokens)}</span>
          </>
        )}
      </span>
      <span className="ah-sessrow__l3">
        <span className="ah-branch" title={`native session ${session.native_session_id}`}>
          <span>{session.native_session_id.slice(0, 18)}</span>
        </span>
        <span className="ah-last" data-live={last === "now" ? "" : undefined}>
          {last}
        </span>
      </span>
    </button>
  );
}

export function SessionsColumn({
  project,
  sessions,
  facts,
  kindOf,
  activeId,
  usage,
  sessionScope,
  onSessionScope,
  activityGridStartTimestamp,
  currentTimestamp,
  isWorktreeVisible,
  onToggleWorktree,
  onSelect,
  searchGroups,
}: {
  project: ProjectViewModel | null;
  /** Browse mode: the selected project's root sessions, newest first. */
  sessions: SessionRow[];
  facts: Map<string, SessionFacts>;
  kindOf: (runtimeId: string) => string;
  activeId: string | null;
  /** The selected project's usage decomposition, for the summary panel. */
  usage: UsageResponse | null;
  sessionScope: "roots" | "all";
  onSessionScope: (scope: "roots" | "all") => void;
  activityGridStartTimestamp: number;
  currentTimestamp: number;
  isWorktreeVisible: boolean | null;
  onToggleWorktree: () => void;
  onSelect: (session: SessionRow) => void;
  /** Search mode: sessions-with-hits under project eyebrows. */
  searchGroups:
    | { project: ProjectViewModel; sessions: { session: SessionRow; hits: number }[] }[]
    | null;
}) {
  const searching = searchGroups !== null;
  const total = searching ? searchGroups.reduce((n, g) => n + g.sessions.length, 0) : sessions.length;
  return (
    <div className="ah-col">
      {searching || project === null ? (
        // Search has no project to name, so the head says what the column now
        // holds instead: same band, same height, different content (H-2).
        <div className="ah-meta">
          <div className="ah-meta__head">
            <span className="ah-eyebrow">{searching ? "Sessions with hits" : "Sessions"}</span>
            <span className="ah-caption tnum">
              {total} {total === 1 ? "session" : "sessions"}
            </span>
          </div>
        </div>
      ) : (
        <ProjectSummaryPanel
          project={project}
          visibleSessionCount={total}
          usage={usage}
          activityGridStartTimestamp={activityGridStartTimestamp}
          currentTimestamp={currentTimestamp}
          isWorktreeVisible={isWorktreeVisible}
          onToggleWorktree={onToggleWorktree}
        />
      )}
      <div className="ah-listbar">
        <p className="ah-colnote">{searching ? "ranked by hits" : "session threads"}</p>
        {!searching && (
          <div className="ah-seg ah-sortseg" role="group" aria-label="Show session threads">
            <button
              type="button"
              className={sessionScope === "roots" ? "is-on" : ""}
              onClick={() => onSessionScope("roots")}
            >
              roots
            </button>
            <button
              type="button"
              className={sessionScope === "all" ? "is-on" : ""}
              onClick={() => onSessionScope("all")}
            >
              all
            </button>
          </div>
        )}
      </div>
      <div className="ah-scroll">
        {searching
          ? searchGroups.map((group) => (
              <div key={group.project.row.id}>
                <div className="ah-group" style={{ "--tint": group.project.tint } as CSSProperties}>
                  <i />
                  {group.project.row.name}
                  <span className="ah-hitcount">
                    {group.sessions.reduce((n, s) => n + s.hits, 0)}
                  </span>
                </div>
                {group.sessions.map(({ session, hits }) => (
                  <SessionRowButton
                    key={session.id}
                    session={session}
                    facts={facts.get(session.id) ?? null}
                    kind={kindOf(session.runtime_id)}
                    active={session.id === activeId}
                    hits={hits}
                    nowMs={currentTimestamp}
                    onSelect={onSelect}
                  />
                ))}
              </div>
            ))
          : (() => {
              let day: string | null = null;
              const out: React.ReactNode[] = [];
              for (const session of sessions) {
                const label = dayLabel(session.updated_at, currentTimestamp);
                if (label !== day) {
                  day = label;
                  out.push(
                    <div className="ah-day" key={`day-${label}`}>
                      {label}
                    </div>,
                  );
                }
                out.push(
                  <SessionRowButton
                    key={session.id}
                    session={session}
                    facts={facts.get(session.id) ?? null}
                    kind={kindOf(session.runtime_id)}
                    active={session.id === activeId}
                    hits={null}
                    nowMs={currentTimestamp}
                    onSelect={onSelect}
                  />,
                );
              }
              return out;
            })()}
        {total === 0 && (
          <div className="ah-empty">
            <strong>{searching ? "Nothing matched" : "No sessions here yet"}</strong>
            <p>
              {searching
                ? "No indexed message contains that."
                : "Sessions appear as rosettad reads this project's agent stores."}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
