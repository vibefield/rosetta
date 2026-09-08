import type { ProjectBackfill } from "@rosetta/protocol";
import type { ProjectViewModel } from "../../lib/model";
import { useListMotion } from "../../lib/use-list-motion";
import { EmptyState } from "../ui";
import { ProjectListItem } from "./ProjectListItem";

export interface ProjectListProps {
  /** Projects in display order; sorting and filtering belong to the container. */
  readonly projects: readonly ProjectViewModel[];
  readonly selectedProjectId: string | null;
  readonly maximumLifetimeTokens: number;
  /** `null` denotes browse mode; a map denotes search mode. */
  readonly searchHitCounts: ReadonlyMap<string, number> | null;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  /** A backfill is running: unfinished projects cook rather than lie. */
  readonly ingestActive: boolean;
  /** The daemon's per-project backfill fold, keyed by project id. */
  readonly projectBackfills: ReadonlyMap<string, ProjectBackfill>;
  /** The list itself is still being censused — only the ready may claim
   * emptiness, so the empty state yields to the section's veil. */
  readonly pending: boolean;
  readonly onProjectSelect: (projectId: string) => void;
}

export function ProjectList({
  projects,
  selectedProjectId,
  maximumLifetimeTokens,
  searchHitCounts,
  homeDirectory,
  currentTimestamp,
  ingestActive,
  projectBackfills,
  pending,
  onProjectSelect,
}: ProjectListProps) {
  const isSearching = searchHitCounts !== null;
  const motionRef = useListMotion(JSON.stringify(projects.map((project) => project.row.id)), "projects");

  return (
    <div className="ah-scroll ah-motion-list" ref={motionRef}>
      {projects.map((project) => (
        <ProjectListItem
          key={project.row.id}
          project={project}
          maximumLifetimeTokens={maximumLifetimeTokens}
          isSelected={project.row.id === selectedProjectId}
          searchHitCount={isSearching ? (searchHitCounts.get(project.row.id) ?? 0) : null}
          homeDirectory={homeDirectory}
          currentTimestamp={currentTimestamp}
          ingestActive={ingestActive}
          backfill={projectBackfills.get(project.row.id) ?? null}
          onProjectSelect={onProjectSelect}
        />
      ))}
      {projects.length === 0 && !pending && (
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
