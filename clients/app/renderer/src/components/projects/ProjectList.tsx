import type { ProjectViewModel } from "../../lib/model";
import { EmptyState } from "../ui";
import { ProjectListItem } from "./ProjectListItem";

export interface ProjectListProps {
  /** Projects in display order; sorting and filtering belong to the container. */
  readonly projects: readonly ProjectViewModel[];
  readonly selectedProjectId: string | null;
  readonly maximumTokensLast30Days: number;
  /** `null` denotes browse mode; a map denotes search mode. */
  readonly searchHitCounts: ReadonlyMap<string, number> | null;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  readonly onProjectSelect: (projectId: string) => void;
}

export function ProjectList({
  projects,
  selectedProjectId,
  maximumTokensLast30Days,
  searchHitCounts,
  homeDirectory,
  currentTimestamp,
  onProjectSelect,
}: ProjectListProps) {
  const isSearching = searchHitCounts !== null;

  return (
    <div className="ah-scroll">
      {projects.map((project) => (
        <ProjectListItem
          key={project.row.id}
          project={project}
          maximumTokensLast30Days={maximumTokensLast30Days}
          isSelected={project.row.id === selectedProjectId}
          searchHitCount={isSearching ? (searchHitCounts.get(project.row.id) ?? 0) : null}
          homeDirectory={homeDirectory}
          currentTimestamp={currentTimestamp}
          onProjectSelect={onProjectSelect}
        />
      ))}
      {projects.length === 0 && (
        <EmptyState
          title={isSearching ? "No project matched" : "No projects yet"}
          description={
            isSearching
              ? "No indexed message contains that."
              : "rosettad has not indexed any project yet — leave it serving and this fills in."
          }
        />
      )}
    </div>
  );
}
