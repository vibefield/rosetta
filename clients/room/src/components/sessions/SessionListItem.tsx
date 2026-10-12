import type { SessionRow } from "@vibefield/rosetta-protocol";
import { AgentMark } from "../../lib/agents";
import { duration, lastActive, tokenText } from "../../lib/format";
import { isSessionCooking } from "../../lib/ingest";
import { sessionLastActive } from "../../lib/session-activity";
import { spendDescription, spendText } from "../../lib/spend";
import { getSessionStatus, getSessionTitle } from "./session-display";
import type { SessionFacts } from "./types";
import { useHostInputs } from "../../host-inputs";
import { HostAct } from "../HostAct";

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
  const lastActiveLabel = lastActive(sessionLastActive(session), currentTimestamp);
  const roleLabel = session.role === "main" ? null : session.role;
  const accessibleMetadata = [
    roleLabel === null ? null : `${roleLabel} session`,
    landedFacts === null ? null : `${landedFacts.messages} messages`,
    landedFacts !== null && landedFacts.tokens > 0
      ? `${tokenText(landedFacts.tokens)} tokens`
      : null,
    landedFacts !== null && landedFacts.tokens > 0
      ? spendDescription(landedFacts.spend, tokenText)
      : null,
  ].filter((value): value is string => value !== null);
  const spendFigure = landedFacts !== null && landedFacts.tokens > 0
    ? spendText(landedFacts.spend)
    : null;
  // R4: the host's id slot, and its act or the reason at the row's end.
  const decoration = useHostInputs().sessionRow?.(session) ?? null;
  const hostId = decoration?.id ?? null;
  const act = decoration?.act ?? null;
  const reason = act === null ? (decoration?.reason ?? null) : null;
  const className = `ah-session-list-item${isSelected ? " is-selected" : ""}`;
  const accessibleName = [
    title,
    status.label,
    ...accessibleMetadata,
    `last active ${lastActiveLabel}`,
  ].join(" · ");
  // With an act the row holds two buttons, and a button cannot hold another:
  // the row becomes a box, one button covers it (the selection, named as the
  // row always was), and the act sits above it. The rest is drawn for the eye.
  const shown = act === null ? undefined : true;
  const last = (
    <span className="ah-last" data-live={lastActiveLabel === "now" ? "" : undefined} aria-hidden={shown}>
      {lastActiveLabel}
    </span>
  );
  const idSlot = hostId === null ? (
    <span
      className="ah-session-list-item__native-id"
      title={`Native session ${session.native_session_id}`}
    >
      <span>{session.native_session_id.slice(0, 18)}</span>
    </span>
  ) : (
    <span className="ah-session-list-item__native-id" data-host="" title={decoration?.idTitle} aria-hidden={shown}>
      {hostId}
    </span>
  );
  const body = (
    <>
      <i
        className="ah-session-list-item__status"
        data-tone={status.tone ?? undefined}
        data-live={status.isLive ? "" : undefined}
        title={status.label}
        aria-hidden="true"
      />
      <span className="ah-session-list-item__header" aria-hidden={shown}>
        <strong title={title}>{title}</strong>
        <AgentMark kind={runtimeKind} />
      </span>
      <span className="ah-session-list-item__metadata" aria-hidden={shown}>
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
        <span title="Span of recorded conversation activity">
          {session.activity ? duration(session.activity.first_at, session.activity.last_at) : "—"}
        </span>
        {landedFacts !== null && landedFacts.tokens > 0 && (
          <>
            <span>·</span>
            <span>{tokenText(landedFacts.tokens)}</span>
          </>
        )}
        {landedFacts !== null && spendFigure !== null && (
          <>
            <span>·</span>
            <span title={spendDescription(landedFacts.spend, tokenText)}>{spendFigure}</span>
          </>
        )}
      </span>
      <span className="ah-session-list-item__footer">
        {idSlot}
        {act === null && reason === null ? last : (
          <span className="ah-session-list-item__end">
            {act !== null ? <HostAct act={act} place="row" /> : (
              <span className="ah-session-list-item__reason">{reason}</span>
            )}
            {last}
          </span>
        )}
      </span>
    </>
  );

  if (act === null) {
    return (
      <button
        type="button"
        className={className}
        data-motion-key={session.id}
        data-role={session.role}
        data-cooking={isCooking ? "" : undefined}
        aria-current={isSelected ? "true" : undefined}
        aria-label={accessibleName}
        onClick={() => onSessionSelect(session)}
      >
        {body}
      </button>
    );
  }
  return (
    <div
      className={className}
      data-motion-key={session.id}
      data-role={session.role}
      data-cooking={isCooking ? "" : undefined}
      data-host-act=""
    >
      <button
        type="button"
        className="ah-session-list-item__hit"
        aria-current={isSelected ? "true" : undefined}
        aria-label={accessibleName}
        onClick={() => onSessionSelect(session)}
      />
      {body}
    </div>
  );
}
