import { useMemo } from "react";
import type { IngestProgress } from "@vibefield/rosetta-protocol";
import { isCensusRunning, isIngestActive, projectBackfills } from "../../lib/ingest";
import type { ProjectViewModel } from "../../lib/model";
import { UNASSIGNED_PROJECT_ID } from "../../lib/model";
import { compareProjectOrder } from "../../lib/activity-order";
import type { ActivityOrder } from "../../lib/activity-order";
import { PortfolioSummaryPanel } from "../SummaryPanels";
import { ListSummaryHeader, ListToolbar, SegmentedControl } from "../ui";
import type { SegmentedControlOption } from "../ui/SegmentedControl";
import { IngestStatus, IngestStatusFold } from "../IngestStatus";
import type { IngestActivity } from "../../lib/ingest-activity";
import type { ConversationReadiness } from "../../lib/conversation-readiness";
import { ProjectList } from "./ProjectList";
import { useHostInputs } from "../../host-inputs";
import { arrangeSections } from "../../lib/sections";
import { FilterChips } from "../FilterChips";

export type ProjectSort = "recent" | "token";

/** Spend figures load only after publication, so until then a token sort
 * would rank every project at zero, by id: it waits, and says why. */
const TOKEN_SORT_WAITS = "Spend ranks arrive after the import";
const projectSortOptions = (importing: boolean): readonly SegmentedControlOption<ProjectSort>[] => [
  { value: "recent", label: "recent" },
  importing
    ? { value: "token", label: "token", disabled: true, title: TOKEN_SORT_WAITS, accessibleLabel: `token: ${TOKEN_SORT_WAITS}` }
    : { value: "token", label: "token" },
];

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
  /** The room's fold of readable conversations, from the rows it draws. */
  readonly conversations?: ConversationReadiness | undefined;
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
  conversations,
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
  // The import's order holds until publication: the token sort waits, and
  // a token choice reads as recent meanwhile.
  const importing = activityOrder.bootstrapping;
  const effectiveSort: ProjectSort = importing ? "recent" : sort;
  const visibleProjects = useMemo(
    () =>
      [...projects].sort((left, right) =>
        effectiveSort === "recent"
          ? compareProjectOrder(
              activityOrder,
              left.row.id === UNASSIGNED_PROJECT_ID ? null : left.row.id,
              right.row.id === UNASSIGNED_PROJECT_ID ? null : right.row.id,
              left.row.id, right.row.id,
            )
          : (right.lifetimeSpend.costMicroUsd ?? 0) - (left.lifetimeSpend.costMicroUsd ?? 0)
            || right.lifetimeTokens - left.lifetimeTokens || left.row.id.localeCompare(right.row.id),
      ),
    [projects, effectiveSort, activityOrder],
  );

  // R3: the host's sections over the room's own order.
  const { projectSections, projectFilters } = useHostInputs();
  const sections = useMemo(
    () => (projectSections === undefined ? null : arrangeSections(projectSections, visibleProjects, (p) => p.row.id)),
    [projectSections, visibleProjects],
  );

  // The portfolio panel shows once figures land, whatever search is doing,
  // and the import or search card sits under its head; so the end folds the
  // card away under a head that is already there. Before any figure there
  // is nothing to chart: the bare count heads the column.
  const figuresLanded = useMemo(() => projects.some((project) => project.statsReady), [projects]);
  return (
    <div className="ah-col">
      {activity === null || figuresLanded ? (
        <PortfolioSummaryPanel
          projects={projects}
          selectedProjectId={selectedProjectId}
          ingestActive={ingestActive}
          censusRunning={censusRunning}
          onProjectSelect={onProjectSelect}
        />
      ) : (
        <ListSummaryHeader label="Projects" count={projects.length} singularNoun="project" />
      )}
      <IngestStatusFold activity={activity}>
        <IngestStatus activity={activity} />
      </IngestStatusFold>
      <ListToolbar>
        {projectFilters !== undefined && projectFilters.length > 0 && <FilterChips filters={projectFilters} />}
        <SegmentedControl
          value={effectiveSort}
          options={projectSortOptions(importing)}
          accessibleLabel="Rank projects by"
          className="ah-sortseg"
          onValueChange={onSortChange}
        />
      </ListToolbar>
      <div className="ah-veilhost">
        <ProjectList
          projects={visibleProjects}
          orderScope={`projects:${effectiveSort}`}
          selectedProjectId={selectedProjectId}
          maximumLifetimeCost={maximumLifetimeCost}
          homeDirectory={homeDirectory}
          currentTimestamp={currentTimestamp}
          ingestActive={ingestActive}
          importing={importing}
          projectBackfills={backfills}
          projectConversations={conversations?.byProject}
          pending={censusRunning}
          onProjectSelect={onProjectSelect}
          sections={sections}
        />
      </div>
    </div>
  );
}
