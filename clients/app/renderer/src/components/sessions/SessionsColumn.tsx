import type { IngestProgress, SessionRow, UsageResponse } from "@rosetta/protocol";
import { isCensusRunning, isIngestActive } from "../../lib/ingest";
import type { ProjectViewModel } from "../../lib/model";
import { ProjectSummaryPanel } from "../SummaryPanels";
import { ListSummaryHeader, ListToolbar } from "../ui";
import { SessionList } from "./SessionList";
import type { SessionFacts } from "./types";

export interface SessionsColumnProps {
  readonly project: ProjectViewModel | null;
  /** Browse-mode sessions in display order. */
  readonly sessions: readonly SessionRow[];
  readonly factsBySessionId: ReadonlyMap<string, SessionFacts>;
  readonly getRuntimeKind: (runtimeId: string) => string;
  readonly selectedSessionId: string | null;
  readonly usage: UsageResponse | null;
  readonly activityGridStartTimestamp: number;
  readonly currentTimestamp: number;
  /** The daemon's cold-start block; null on a daemon too old to carry it. */
  readonly ingest: IngestProgress | null;
  /** `null` means the browser build has no desktop worktree integration. */
  readonly isWorktreeVisible: boolean | null;
  readonly onToggleWorktree: () => void;
  readonly onSessionSelect: (session: SessionRow) => void;
}

export function SessionsColumn({
  project,
  sessions,
  factsBySessionId,
  getRuntimeKind,
  selectedSessionId,
  usage,
  activityGridStartTimestamp,
  currentTimestamp,
  ingest,
  isWorktreeVisible,
  onToggleWorktree,
  onSessionSelect,
}: SessionsColumnProps) {
  const ingestActive = isIngestActive(ingest);
  const censusRunning = isCensusRunning(ingest);
  const visibleSessionCount = sessions.length;

  return (
    <div className="ah-col">
      {project === null ? (
        <ListSummaryHeader label="Sessions" count={visibleSessionCount} singularNoun="session" />
      ) : (
        <ProjectSummaryPanel
          project={project}
          visibleSessionCount={visibleSessionCount}
          usage={usage}
          activityGridStartTimestamp={activityGridStartTimestamp}
          currentTimestamp={currentTimestamp}
          ingestActive={ingestActive}
          censusRunning={censusRunning}
          isWorktreeVisible={isWorktreeVisible}
          onToggleWorktree={onToggleWorktree}
        />
      )}
      <ListToolbar label="session threads" />
      {censusRunning && <p className="ah-ingest-note">More conversations will appear as discovery finishes.</p>}
      <div className="ah-veilhost">
        <SessionList
          sessions={sessions}
          factsBySessionId={factsBySessionId}
          getRuntimeKind={getRuntimeKind}
          selectedSessionId={selectedSessionId}
          currentTimestamp={currentTimestamp}
          duringBackfill={ingestActive}
          pending={censusRunning}
          emptyState={
            project === null
              ? {
                  title: "No project selected",
                  description: "Pick a project to browse its sessions.",
                }
              : undefined
          }
          onSessionSelect={onSessionSelect}
        />
      </div>
    </div>
  );
}
