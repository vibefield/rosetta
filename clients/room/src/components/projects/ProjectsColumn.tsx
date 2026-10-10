import { useMemo } from "react";
import type { IngestProgress } from "@rosetta/protocol";
import { isCensusRunning, isIngestActive, projectBackfills } from "../../lib/ingest";
import type { ProjectViewModel } from "../../lib/model";
import { UNASSIGNED_PROJECT_ID } from "../../lib/model";
import { compareActivityRanks } from "../../lib/activity-order";
import type { ActivityOrder } from "../../lib/activity-order";
import { PortfolioSummaryPanel } from "../SummaryPanels";
import { ListSummaryHeader, ListToolbar, SegmentedControl } from "../ui";
import { IngestStatus } from "../IngestStatus";
import type { IngestActivity } from "../../lib/ingest-activity";
import { ProjectList } from "./ProjectList";
import { useHostInputs } from "../../host-inputs";
import { arrangeSections } from "../../lib/sections";
import { FilterChips } from "../FilterChips";

export type ProjectSort = "recent" | "token";

const PROJECT_SORT_OPTIONS = [
  { value: "recent", label: "recent" },
  { value: "token", label: "token" },
] as const;

export interface ProjectsColumnProps {
  readonly projects: readonly ProjectViewModel[];
  readonly activityOrder: ActivityOrder;
  readonly selectedProjectId: string | null;
  readonly sort: ProjectSort;
  readonly onSortChange: (sort: ProjectSort) => void;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  /** The daemon's cold-start block; null on a daemon too old to carry it. */
  readonly ingest: IngestProgress | null;
  readonly activity: IngestActivity | null;
  readonly onProjectSelect: (projectId: string) => void;
}

export function ProjectsColumn({
  projects,
  activityOrder,
  selectedProjectId,
  sort,
  onSortChange,
  homeDirectory,
  currentTimestamp,
  ingest,
  activity,
  onProjectSelect,
}: ProjectsColumnProps) {
  const ingestActive = isIngestActive(ingest);
  // Metadata can arrive progressively; the status panel explains this
  // without covering projects that are already available.
  const censusRunning = isCensusRunning(ingest);
  // The daemon's per-project fold: each cooking card draws its own byte
  // meter from this, so progress reads at the row you actually care about.
  const backfills = useMemo(() => projectBackfills(ingest), [ingest]);
  // "Spend" is estimated USD (§4.1.3); tokens only break ties, so wholly
  // unpriced projects still order among themselves.
  const maximumLifetimeCost = useMemo(
    () => projects.reduce(
      (maximum, project) => Math.max(maximum, project.lifetimeSpend.costMicroUsd ?? 0),
      0,
    ),
    [projects],
  );
  const visibleProjects = useMemo(
    () =>
      [...projects].sort((left, right) =>
        sort === "recent"
          ? compareActivityRanks(
              activityOrder.projects.get(left.row.id === UNASSIGNED_PROJECT_ID ? null : left.row.id)?.rankAt ?? null,
              activityOrder.projects.get(right.row.id === UNASSIGNED_PROJECT_ID ? null : right.row.id)?.rankAt ?? null,
              left.row.id, right.row.id,
            )
          : (right.lifetimeSpend.costMicroUsd ?? 0) - (left.lifetimeSpend.costMicroUsd ?? 0)
            || right.lifetimeTokens - left.lifetimeTokens || left.row.id.localeCompare(right.row.id),
      ),
    [projects, sort, activityOrder],
  );

  // R3: the host's sections over the room's own order.
  const { projectSections, projectFilters } = useHostInputs();
  const sections = useMemo(
    () => (projectSections === undefined ? null : arrangeSections(projectSections, visibleProjects, (p) => p.row.id)),
    [projectSections, visibleProjects],
  );

  return (
    <div className="ah-col">
      {activity !== null ? (
        <ListSummaryHeader label="Projects" count={projects.length} singularNoun="project" />
      ) : (
        <PortfolioSummaryPanel
          projects={projects}
          selectedProjectId={selectedProjectId}
          ingestActive={ingestActive}
          censusRunning={censusRunning}
          onProjectSelect={onProjectSelect}
        />
      )}
      <IngestStatus activity={activity} />
      <ListToolbar>
        {projectFilters !== undefined && projectFilters.length > 0 && <FilterChips filters={projectFilters} />}
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
          orderScope={`projects:${sort}`}
          selectedProjectId={selectedProjectId}
          maximumLifetimeCost={maximumLifetimeCost}
          homeDirectory={homeDirectory}
          currentTimestamp={currentTimestamp}
          ingestActive={ingestActive}
          runtimes={ingest?.runtimes}
          projectBackfills={backfills}
          pending={censusRunning}
          onProjectSelect={onProjectSelect}
          sections={sections}
        />
      </div>
    </div>
  );
}
