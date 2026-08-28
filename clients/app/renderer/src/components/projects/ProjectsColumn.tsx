import { useMemo } from "react";
import type { ProjectViewModel } from "../../lib/model";
import { PortfolioSummaryPanel } from "../SummaryPanels";
import { ListSummaryHeader, ListToolbar, SegmentedControl } from "../ui";
import { ProjectList } from "./ProjectList";

export type ProjectSort = "spend" | "recent";

const PROJECT_SORT_OPTIONS = [
  { value: "spend", label: "spend" },
  { value: "recent", label: "recent" },
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
  onProjectSelect,
}: ProjectsColumnProps) {
  const isSearching = searchHitCounts !== null;
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
          onProjectSelect={onProjectSelect}
        />
      )}
      <ListToolbar label="spend over 30d">
        <SegmentedControl
          value={sort}
          options={PROJECT_SORT_OPTIONS}
          accessibleLabel="Rank projects by"
          className="ah-sortseg"
          onValueChange={onSortChange}
        />
      </ListToolbar>
      <ProjectList
        projects={visibleProjects}
        selectedProjectId={selectedProjectId}
        maximumTokensLast30Days={maximumTokensLast30Days}
        searchHitCounts={searchHitCounts}
        homeDirectory={homeDirectory}
        currentTimestamp={currentTimestamp}
        onProjectSelect={onProjectSelect}
      />
    </div>
  );
}
