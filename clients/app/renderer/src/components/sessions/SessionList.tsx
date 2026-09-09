import type { SessionRow } from "@rosetta/protocol";
import { dayLabel } from "../../lib/format";
import { useListMotion } from "../../lib/use-list-motion";
import { EmptyState } from "../ui";
import { SessionListItem } from "./SessionListItem";
import type { SessionFacts } from "./types";

export interface SessionListProps {
  /** Sessions in display order, normally newest first. */
  readonly sessions: readonly SessionRow[];
  readonly factsBySessionId: ReadonlyMap<string, SessionFacts>;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly selectedSessionId: string | null;
  readonly currentTimestamp: number;
  /** A backfill is running — unindexed sessions cook rather than lie. */
  readonly duringBackfill: boolean;
  /** The census is still naming sessions: the empty state yields to the
   * section's veil, because only the ready may claim emptiness. */
  readonly pending: boolean;
  readonly emptyState?: {
    readonly title: string;
    readonly description: string;
  };
  readonly onSessionSelect: (session: SessionRow) => void;
}

interface SessionDayGroup {
  readonly label: string;
  readonly sessions: SessionRow[];
}

function groupSessionsByDay(
  sessions: readonly SessionRow[],
  currentTimestamp: number,
): SessionDayGroup[] {
  const groups: SessionDayGroup[] = [];

  for (const session of sessions) {
    const label = dayLabel(session.updated_at, currentTimestamp);
    const currentGroup = groups.at(-1);
    if (currentGroup?.label === label) {
      currentGroup.sessions.push(session);
    } else {
      groups.push({ label, sessions: [session] });
    }
  }

  return groups;
}

export function SessionList(props: SessionListProps) {
  const days = groupSessionsByDay(props.sessions, props.currentTimestamp);
  const order = days.map((day) => [day.label, ...day.sessions.map((session) => session.id)]);
  const scope = `browse:${props.sessions[0]?.project_id ?? "empty"}`;
  const motionRef = useListMotion(JSON.stringify(order), scope);

  return (
    <div className="ah-scroll ah-session-list ah-motion-list" ref={motionRef}>
      {/* Headers and rows share one key space. Moving from Yesterday to Today
          preserves the actual button, its keyboard focus and its animation. */}
      {days.flatMap((group) => [
        <div key={`day:${group.label}`} className="ah-day">
          {group.label}
        </div>,
        ...group.sessions.map((session) => (
          <SessionListItem
            key={`session:${session.id}`}
            session={session}
            facts={props.factsBySessionId.get(session.id) ?? null}
            runtimeKind={props.getRuntimeKind(session.runtime_id)}
            isSelected={session.id === props.selectedSessionId}
            currentTimestamp={props.currentTimestamp}
            duringBackfill={props.duringBackfill}
            onSessionSelect={props.onSessionSelect}
          />
        )),
      ])}
      {props.sessions.length === 0 && !props.pending && (
        <EmptyState
          title={props.emptyState?.title ?? "No sessions here yet"}
          description={
            props.emptyState?.description ??
            "Sessions appear as rosettad reads this project's agent stores."
          }
        />
      )}
    </div>
  );
}
