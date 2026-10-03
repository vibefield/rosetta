import type { SessionRow } from "@rosetta/protocol";
import { dayLabel } from "../../lib/format";
import { sessionLastActive } from "../../lib/session-activity";
import { useListMotion } from "../../lib/use-list-motion";
import { activityDay } from "../../lib/activity-order";
import { useDeferredListOrder } from "../../lib/use-deferred-list-order";
import type { ListPosition } from "../../lib/use-deferred-list-order";
import { ListActivityNotice } from "../ListActivityNotice";
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
  } | undefined;
  readonly onSessionSelect: (session: SessionRow) => void;
}

interface SessionDayGroup {
  readonly day: number | null;
  readonly label: string;
  readonly sessions: SessionRow[];
}

function groupSessionsByDay(
  positions: readonly ListPosition[],
  sessions: ReadonlyMap<string, SessionRow>,
  currentTimestamp: number,
): SessionDayGroup[] {
  const groups: SessionDayGroup[] = [];

  for (const { id, day } of positions) {
    const session = sessions.get(id);
    if (!session) continue;
    const label = dayLabel(day, currentTimestamp);
    const currentGroup = groups.at(-1);
    if (currentGroup?.day === day) {
      currentGroup.sessions.push(session);
    } else {
      groups.push({ day, label, sessions: [session] });
    }
  }

  return groups;
}

export function SessionList(props: SessionListProps) {
  const scope = `browse:${props.sessions[0]?.project_id ?? "empty"}`;
  const layout = useDeferredListOrder(props.sessions.map(session => ({
    id: session.id, day: activityDay(sessionLastActive(session)),
  })), scope);
  const days = groupSessionsByDay(layout.items, new Map(props.sessions.map(session => [session.id, session])), props.currentTimestamp);
  const motionRef = useListMotion(JSON.stringify(layout.items), scope, layout.scrollRef);

  return (
    <div className="ah-list-view" {...layout.hostProps}>
      <div className="ah-scroll ah-session-list ah-motion-list" ref={motionRef} onScroll={layout.onScroll}>
        {/* Headers and rows share one key space. Moving from Yesterday to Today
            preserves the actual button, its keyboard focus and its animation. */}
        {days.flatMap((group) => [
          <div key={`day:${group.day}`} className="ah-day">
            {group.sessions.some(session => activityDay(sessionLastActive(session)) !== group.day)
              ? "Activity updated" : group.label}
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
      <ListActivityNotice pending={layout.pending} onShow={layout.apply} noun="sessions" />
    </div>
  );
}
