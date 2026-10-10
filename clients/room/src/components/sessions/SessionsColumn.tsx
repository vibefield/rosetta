import { useMemo } from "react";
import type { ReactNode } from "react";
import type { IngestProgress, SessionRow, UsageResponse } from "@rosetta/protocol";
import type { RoomAct } from "../../host-inputs";
import { useHostInputs } from "../../host-inputs";
import { arrangeSections } from "../../lib/sections";
import { isCensusRunning, isIngestActive } from "../../lib/ingest";
import type { ProjectViewModel } from "../../lib/model";
import { ProjectSummaryPanel } from "../SummaryPanels";
import { ListSummaryHeader } from "../ui";
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
  /** R5: the host's acts after the head's own, and its card under the head. */
  readonly hostActs?: readonly RoomAct[] | null | undefined;
  readonly hostCard?: ReactNode;
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
  hostActs = null,
  hostCard = null,
}: SessionsColumnProps) {
  const ingestActive = isIngestActive(ingest);
  const censusRunning = isCensusRunning(ingest);
  // R3: the host's sections over this project's sessions, in the room's order.
  const { sessionSections } = useHostInputs();
  const projectId = project?.row.id ?? null;
  const sections = useMemo(() => {
    const given = projectId === null ? null : (sessionSections?.(projectId) ?? null);
    return given === null ? null : arrangeSections(given, sessions, (session) => session.id);
  }, [sessionSections, projectId, sessions]);
  // The head counts what the list holds: with sections, what they draw.
  const visibleSessionCount = sections === null
    ? sessions.length
    : sections.reduce((total, entry) => total + entry.rows.length, 0);

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
          hostActs={hostActs}
        />
      )}
      {project !== null && hostCard !== null && hostCard !== undefined && hostCard !== false && (
        <div className="ah-hostcard">{hostCard}</div>
      )}
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
          sections={sections}
        />
      </div>
    </div>
  );
}
