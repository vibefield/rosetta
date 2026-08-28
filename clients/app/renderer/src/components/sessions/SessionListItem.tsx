import type { SessionRow } from "@rosetta/protocol";
import { AgentMark } from "../../lib/agents";
import { duration, lastActive, tokenText } from "../../lib/format";
import { getSessionStatus, getSessionTitle } from "./session-display";
import type { SessionFacts } from "./types";

export interface SessionListItemProps {
  readonly session: SessionRow;
  readonly facts: SessionFacts | null;
  readonly runtimeKind: string;
  readonly isSelected: boolean;
  /** `null` hides the search hit count. */
  readonly searchHitCount: number | null;
  readonly currentTimestamp: number;
  readonly onSelect: (session: SessionRow) => void;
}

export function SessionListItem({
  session,
  facts,
  runtimeKind,
  isSelected,
  searchHitCount,
  currentTimestamp,
  onSelect,
}: SessionListItemProps) {
  const status = getSessionStatus(session);
  const title = getSessionTitle(session);
  const lastActiveLabel = lastActive(session.updated_at, currentTimestamp);
  const roleLabel = session.role === "main" ? null : session.role;

  return (
    <button
      type="button"
      className={`ah-sessrow${isSelected ? " is-active is-focused" : ""}`}
      data-role={session.role}
      aria-current={isSelected ? "true" : undefined}
      aria-label={`${title} · ${status.label} · last active ${lastActiveLabel}`}
      onClick={() => onSelect(session)}
    >
      <i
        className="ah-dot"
        data-tone={status.tone ?? undefined}
        data-live={status.isLive ? "" : undefined}
        title={status.label}
        aria-hidden="true"
      />
      <span className="ah-sessrow__l1">
        <strong title={title}>{title}</strong>
        <AgentMark kind={runtimeKind} />
      </span>
      <span className="ah-sessmeta">
        {roleLabel !== null && (
          <>
            <span>{roleLabel}</span>
            <span>·</span>
          </>
        )}
        {searchHitCount !== null && (
          <>
            <span>
              {searchHitCount} {searchHitCount === 1 ? "hit" : "hits"}
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
        <span className="ah-branch" title={`Native session ${session.native_session_id}`}>
          <span>{session.native_session_id.slice(0, 18)}</span>
        </span>
        <span className="ah-last" data-live={lastActiveLabel === "now" ? "" : undefined}>
          {lastActiveLabel}
        </span>
      </span>
    </button>
  );
}
