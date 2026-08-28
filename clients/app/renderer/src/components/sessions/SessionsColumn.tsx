import type { SessionRow, UsageResponse } from "@rosetta/protocol";
import type { ProjectViewModel } from "../../lib/model";
import type { ProjectSearchResultGroup } from "../../lib/search-results";
import { ProjectSummaryPanel } from "../SummaryPanels";
import { ListSummaryHeader, ListToolbar, SegmentedControl } from "../ui";
import { SessionList } from "./SessionList";
import type { SessionFacts, SessionScope } from "./types";

const SESSION_SCOPE_OPTIONS = [
  { value: "roots", label: "roots" },
  { value: "all", label: "all" },
] as const;

export interface SessionsColumnProps {
  readonly project: ProjectViewModel | null;
  /** Browse-mode sessions in display order. */
  readonly sessions: readonly SessionRow[];
  readonly factsBySessionId: ReadonlyMap<string, SessionFacts>;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly selectedSessionId: string | null;
  readonly usage: UsageResponse | null;
  readonly sessionScope: SessionScope;
  readonly onSessionScopeChange: (scope: SessionScope) => void;
  readonly activityGridStartTimestamp: number;
  readonly currentTimestamp: number;
  /** `null` means the browser build has no desktop worktree integration. */
  readonly isWorktreeVisible: boolean | null;
  readonly onToggleWorktree: () => void;
  readonly onSessionSelect: (session: SessionRow) => void;
  /** Non-null selects search mode and replaces the browse-mode sessions. */
  readonly searchGroups: readonly ProjectSearchResultGroup[] | null;
}

export function SessionsColumn({
  project,
  sessions,
  factsBySessionId,
  getRuntimeKind,
  selectedSessionId,
  usage,
  sessionScope,
  onSessionScopeChange,
  activityGridStartTimestamp,
  currentTimestamp,
  isWorktreeVisible,
  onToggleWorktree,
  onSessionSelect,
  searchGroups,
}: SessionsColumnProps) {
  const isSearching = searchGroups !== null;
  const visibleSessionCount = isSearching
    ? searchGroups.reduce((total, group) => total + group.sessions.length, 0)
    : sessions.length;

  return (
    <div className="ah-col">
      {isSearching || project === null ? (
        <ListSummaryHeader
          label={isSearching ? "Sessions with hits" : "Sessions"}
          count={visibleSessionCount}
          singularNoun="session"
        />
      ) : (
        <ProjectSummaryPanel
          project={project}
          visibleSessionCount={visibleSessionCount}
          usage={usage}
          activityGridStartTimestamp={activityGridStartTimestamp}
          currentTimestamp={currentTimestamp}
          isWorktreeVisible={isWorktreeVisible}
          onToggleWorktree={onToggleWorktree}
        />
      )}
      <ListToolbar label={isSearching ? "ranked by hits" : "session threads"}>
        {!isSearching && (
          <SegmentedControl
            value={sessionScope}
            options={SESSION_SCOPE_OPTIONS}
            accessibleLabel="Show session threads"
            className="ah-sortseg"
            onValueChange={onSessionScopeChange}
          />
        )}
      </ListToolbar>
      {searchGroups === null ? (
        <SessionList
          mode="browse"
          sessions={sessions}
          factsBySessionId={factsBySessionId}
          getRuntimeKind={getRuntimeKind}
          selectedSessionId={selectedSessionId}
          currentTimestamp={currentTimestamp}
          onSessionSelect={onSessionSelect}
        />
      ) : (
        <SessionList
          mode="search"
          groups={searchGroups}
          factsBySessionId={factsBySessionId}
          getRuntimeKind={getRuntimeKind}
          selectedSessionId={selectedSessionId}
          currentTimestamp={currentTimestamp}
          onSessionSelect={onSessionSelect}
        />
      )}
    </div>
  );
}
