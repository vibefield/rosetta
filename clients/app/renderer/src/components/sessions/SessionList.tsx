import type { SessionRow } from "@rosetta/protocol";
import { dayLabel } from "../../lib/format";
import { useListMotion } from "../../lib/use-list-motion";
import type { ProjectSearchResultGroup } from "../../lib/search-results";
import { EmptyState, ResultGroupHeader } from "../ui";
import { SessionListItem } from "./SessionListItem";
import type { SessionFacts } from "./types";

interface SessionListCommonProps {
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

interface BrowseSessionListProps extends SessionListCommonProps {
  readonly mode: "browse";
  /** Sessions in display order, normally newest first. */
  readonly sessions: readonly SessionRow[];
}

interface SearchSessionListProps extends SessionListCommonProps {
  readonly mode: "search";
  readonly groups: readonly ProjectSearchResultGroup[];
}

export type SessionListProps = BrowseSessionListProps | SearchSessionListProps;

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

function SessionItem({
  session,
  searchHitCount,
  common,
}: {
  readonly session: SessionRow;
  readonly searchHitCount: number | null;
  readonly common: SessionListCommonProps;
}) {
  return (
    <SessionListItem
      session={session}
      facts={common.factsBySessionId.get(session.id) ?? null}
      runtimeKind={common.getRuntimeKind(session.runtime_id)}
      isSelected={session.id === common.selectedSessionId}
      searchHitCount={searchHitCount}
      currentTimestamp={common.currentTimestamp}
      duringBackfill={common.duringBackfill}
      onSessionSelect={common.onSessionSelect}
    />
  );
}

export function SessionList(props: SessionListProps) {
  const common: SessionListCommonProps = props;
  const days =
    props.mode === "browse" ? groupSessionsByDay(props.sessions, props.currentTimestamp) : [];
  const order =
    props.mode === "browse"
      ? days.map((day) => [day.label, ...day.sessions.map((session) => session.id)])
      : props.groups.map((group) => [
          group.project.row.id,
          ...group.sessions.map((result) => result.session.id),
        ]);
  const scope =
    props.mode === "browse" ? `browse:${props.sessions[0]?.project_id ?? "empty"}` : "search";
  const motionRef = useListMotion(JSON.stringify(order), scope);
  const totalSessionCount =
    props.mode === "browse"
      ? props.sessions.length
      : props.groups.reduce((total, group) => total + group.sessions.length, 0);

  return (
    <div className="ah-scroll ah-session-list ah-motion-list" ref={motionRef}>
      {/* Headers and rows share one key space. Moving from Yesterday to Today
          preserves the actual button, its keyboard focus and its animation. */}
      {props.mode === "search"
        ? props.groups.flatMap((group) => [
            <ResultGroupHeader
              key={`project:${group.project.row.id}`}
              label={group.project.row.name}
              count={group.hitCount}
              countLabel={`${group.hitCount} ${group.hitCount === 1 ? "hit" : "hits"}`}
              tint={group.project.tint}
            />,
            ...group.sessions.map((result) => (
              <SessionItem
                key={`session:${result.session.id}`}
                session={result.session}
                searchHitCount={result.hits.length}
                common={common}
              />
            )),
          ])
        : days.flatMap((group) => [
            <div key={`day:${group.label}`} className="ah-day">
              {group.label}
            </div>,
            ...group.sessions.map((session) => (
              <SessionItem
                key={`session:${session.id}`}
                session={session}
                searchHitCount={null}
                common={common}
              />
            )),
          ])}
      {totalSessionCount === 0 && !props.pending && (
        <EmptyState
          title={
            props.emptyState?.title ??
            (props.mode === "search" ? "Nothing matched" : "No sessions here yet")
          }
          description={
            props.emptyState?.description ??
            (props.mode === "search"
              ? "No indexed message contains that."
              : "Sessions appear as rosettad reads this project's agent stores.")
          }
        />
      )}
    </div>
  );
}
