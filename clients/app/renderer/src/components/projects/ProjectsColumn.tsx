import { useMemo } from "react";
import type { IngestProgress } from "@rosetta/protocol";
import { censusProgress, isCensusRunning, isIngestActive, projectBackfills } from "../../lib/ingest";
import type { ProjectViewModel } from "../../lib/model";
import { PortfolioSummaryPanel } from "../SummaryPanels";
import { ListSummaryHeader, ListToolbar, SegmentedControl, Veil } from "../ui";
import { ProjectList } from "./ProjectList";

export type ProjectSort = "recent" | "token";

const PROJECT_SORT_OPTIONS = [
  { value: "recent", label: "recent" },
  { value: "token", label: "token" },
] as const;

export interface ProjectsColumnProps {
  readonly projects: readonly ProjectViewModel[];
  readonly selectedProjectId: string | null;
  readonly sort: ProjectSort;
  readonly onSortChange: (sort: ProjectSort) => void;
  /** Non-null in search mode; projects with zero hits are filtered out. */
  readonly searchHitCounts: ReadonlyMap<string, number> | null;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  /** The daemon's cold-start block; null on a daemon too old to carry it. */
  readonly ingest: IngestProgress | null;
  readonly onProjectSelect: (projectId: string) => void;
}

export function ProjectsColumn({
  projects,
  selectedProjectId,
  sort,
  onSortChange,
  searchHitCounts,
  homeDirectory,
  currentTimestamp,
  ingest,
  onProjectSelect,
}: ProjectsColumnProps) {
  const isSearching = searchHitCounts !== null;
  const ingestActive = isIngestActive(ingest);
  // While the census (skim) runs the LIST ITSELF is incomplete — the veil
  // is down and rows keep materializing beneath it. Once every runtime is
  // streaming, the list is whole and only the figures still cook.
  const censusRunning = isCensusRunning(ingest);
  // The daemon's per-project fold: each cooking card draws its own byte
  // meter from this, so progress reads at the row you actually care about.
  const backfills = useMemo(() => projectBackfills(ingest), [ingest]);
  const sessionsFound = useMemo(
    () => projects.reduce((total, project) => total + project.sessionCount, 0),
    [projects],
  );
  const maximumTokensLast30Days = useMemo(
    () => projects.reduce(
      (maximum, project) => Math.max(maximum, project.tokensLast30Days),
      0,
    ),
    [projects],
  );
  const visibleProjects = useMemo(() => {
    const sortedProjects = [...projects].sort((left, right) =>
      sort === "recent"
        ? (right.lastActiveMs ?? 0) - (left.lastActiveMs ?? 0)
        : right.tokensLast30Days - left.tokensLast30Days,
    );

    return searchHitCounts === null
      ? sortedProjects
      : sortedProjects.filter(
          (project) => (searchHitCounts.get(project.row.id) ?? 0) > 0,
        );
  }, [projects, searchHitCounts, sort]);

  return (
    <div className="ah-col">
      {isSearching ? (
        <ListSummaryHeader
          label="Projects with hits"
          count={visibleProjects.length}
          singularNoun="project"
        />
      ) : (
        <PortfolioSummaryPanel
          projects={projects}
          selectedProjectId={selectedProjectId}
          ingestActive={ingestActive}
          censusRunning={censusRunning}
          onProjectSelect={onProjectSelect}
        />
      )}
      <ListToolbar label="tokens over 30d">
        <SegmentedControl
          value={sort}
          options={PROJECT_SORT_OPTIONS}
          accessibleLabel="Rank projects by"
          className="ah-sortseg"
          onValueChange={onSortChange}
        />
      </ListToolbar>
      <div className="ah-veilhost">
        <ProjectList
          projects={visibleProjects}
          selectedProjectId={selectedProjectId}
          maximumTokensLast30Days={maximumTokensLast30Days}
          searchHitCounts={searchHitCounts}
          homeDirectory={homeDirectory}
          currentTimestamp={currentTimestamp}
          ingestActive={ingestActive}
          projectBackfills={backfills}
          pending={censusRunning}
          onProjectSelect={onProjectSelect}
        />
        {censusRunning && !isSearching && (
          <Veil
            label={`taking the census · ${sessionsFound.toLocaleString()} sessions found`}
            progress={censusProgress(ingest)}
          />
        )}
      </div>
    </div>
  );
}
