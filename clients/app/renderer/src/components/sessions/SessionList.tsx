import { Fragment, useMemo } from "react";
import type { SessionRow } from "@rosetta/protocol";
import { dayLabel } from "../../lib/format";
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

function BrowseSessionItems({
  sessions,
  common,
}: {
  readonly sessions: readonly SessionRow[];
  readonly common: SessionListCommonProps;
}) {
  const groups = useMemo(
    () => groupSessionsByDay(sessions, common.currentTimestamp),
    [common.currentTimestamp, sessions],
  );

  return groups.map((group) => (
    <Fragment key={group.label}>
      <div className="ah-day">{group.label}</div>
      {group.sessions.map((session) => (
        <SessionItem
          key={session.id}
          session={session}
          searchHitCount={null}
          common={common}
        />
      ))}
    </Fragment>
  ));
}

function SearchSessionItems({
  groups,
  common,
}: {
  readonly groups: readonly ProjectSearchResultGroup[];
  readonly common: SessionListCommonProps;
}) {
  return groups.map((group) => {
    return (
      <div key={group.project.row.id}>
        <ResultGroupHeader
          label={group.project.row.name}
          count={group.hitCount}
          countLabel={`${group.hitCount} ${group.hitCount === 1 ? "hit" : "hits"}`}
          tint={group.project.tint}
        />
        {group.sessions.map((result) => (
          <SessionItem
            key={result.session.id}
            session={result.session}
            searchHitCount={result.hits.length}
            common={common}
          />
        ))}
      </div>
    );
  });
}

export function SessionList(props: SessionListProps) {
  const common: SessionListCommonProps = props;
  const totalSessionCount = props.mode === "browse"
    ? props.sessions.length
    : props.groups.reduce((total, group) => total + group.sessions.length, 0);

  return (
    <div className="ah-scroll ah-session-list">
      {props.mode === "search" ? (
        <SearchSessionItems groups={props.groups} common={common} />
      ) : (
        <BrowseSessionItems sessions={props.sessions} common={common} />
      )}
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
