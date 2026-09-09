import type { SessionRow } from "@rosetta/protocol";
import { AgentMark } from "../../lib/agents";
import { duration, lastActive, tokenText } from "../../lib/format";
import { isSessionCooking } from "../../lib/ingest";
import { getSessionStatus, getSessionTitle } from "./session-display";
import type { SessionFacts } from "./types";

export interface SessionListItemProps {
  readonly session: SessionRow;
  readonly facts: SessionFacts | null;
  readonly runtimeKind: string;
  readonly isSelected: boolean;
  readonly currentTimestamp: number;
  /** A backfill is running — an unindexed session's dot says "indexing". */
  readonly duringBackfill: boolean;
  readonly onSessionSelect: (session: SessionRow) => void;
}

export function SessionListItem({
  session,
  facts,
  runtimeKind,
  isSelected,
  currentTimestamp,
  duringBackfill,
  onSessionSelect,
}: SessionListItemProps) {
  const status = getSessionStatus(session, duringBackfill);
  // The cooking card: identity is real from the skim; the figure slots are
  // what is still unknown, and they alone carry the processing state. A
  // facts row whose figures have not landed — a cooking session still
  // counting from zero — is not a claim: the shimmer keeps the slot.
  const cookingState = isSessionCooking(session);
  const landedFacts = facts !== null && (facts.messages > 0 || !cookingState) ? facts : null;
  const isCooking = cookingState && landedFacts === null;
  const title = getSessionTitle(session);
  const lastActiveLabel = lastActive(session.updated_at, currentTimestamp);
  const roleLabel = session.role === "main" ? null : session.role;
  const accessibleMetadata = [
    roleLabel === null ? null : `${roleLabel} session`,
    landedFacts === null ? null : `${landedFacts.messages} messages`,
    landedFacts !== null && landedFacts.tokens > 0
      ? `${tokenText(landedFacts.tokens)} tokens`
      : null,
  ].filter((value): value is string => value !== null);

  return (
    <button
      type="button"
      className={`ah-session-list-item${isSelected ? " is-selected" : ""}`}
      data-motion-key={session.id}
      data-role={session.role}
      data-cooking={isCooking ? "" : undefined}
      aria-current={isSelected ? "true" : undefined}
      aria-label={[
        title,
        status.label,
        ...accessibleMetadata,
        `last active ${lastActiveLabel}`,
      ].join(" · ")}
      onClick={() => onSessionSelect(session)}
    >
      <i
        className="ah-session-list-item__status"
        data-tone={status.tone ?? undefined}
        data-live={status.isLive ? "" : undefined}
        title={status.label}
        aria-hidden="true"
      />
      <span className="ah-session-list-item__header">
        <strong title={title}>{title}</strong>
        <AgentMark kind={runtimeKind} />
      </span>
      <span className="ah-session-list-item__metadata">
        {roleLabel !== null && (
          <>
            <span>{roleLabel}</span>
            <span>·</span>
          </>
        )}
        {landedFacts !== null && (
          <>
            <span>{landedFacts.messages} msgs</span>
            <span>·</span>
          </>
        )}
        {/* the figure slots cook while the daemon reads this transcript —
            a shimmer where the numbers will land, never a claimed zero */}
        {isCooking && (
          <>
            <span className="ah-cook" aria-hidden="true" />
            <span>·</span>
          </>
        )}
        <span>{duration(session.started_at, session.updated_at)}</span>
        {landedFacts !== null && landedFacts.tokens > 0 && (
          <>
            <span>·</span>
            <span>{tokenText(landedFacts.tokens)}</span>
          </>
        )}
      </span>
      <span className="ah-session-list-item__footer">
        <span
          className="ah-session-list-item__native-id"
          title={`Native session ${session.native_session_id}`}
        >
          <span>{session.native_session_id.slice(0, 18)}</span>
        </span>
        <span className="ah-last" data-live={lastActiveLabel === "now" ? "" : undefined}>
          {lastActiveLabel}
        </span>
      </span>
    </button>
  );
}
