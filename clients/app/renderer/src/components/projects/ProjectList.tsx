import type { ProjectBackfill, RuntimeBackfill } from "@rosetta/protocol";
import type { ProjectViewModel } from "../../lib/model";
import { useListMotion } from "../../lib/use-list-motion";
import { EmptyState } from "../ui";
import { ProjectListItem } from "./ProjectListItem";

export interface ProjectListProps {
  /** Projects in display order; sorting and filtering belong to the container. */
  readonly projects: readonly ProjectViewModel[];
  readonly selectedProjectId: string | null;
  readonly maximumLifetimeCost: number;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  /** A backfill is running: unfinished projects cook rather than lie. */
  readonly ingestActive: boolean;
  readonly runtimes?: readonly RuntimeBackfill[];
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
  maximumLifetimeCost,
  homeDirectory,
  currentTimestamp,
  ingestActive,
  runtimes,
  projectBackfills,
  pending,
  onProjectSelect,
}: ProjectListProps) {
  const motionRef = useListMotion(JSON.stringify(projects.map((project) => project.row.id)), "projects");

  return (
    <div className="ah-scroll ah-motion-list" ref={motionRef}>
      {projects.map((project) => (
        <ProjectListItem
          key={project.row.id}
          project={project}
          maximumLifetimeCost={maximumLifetimeCost}
          isSelected={project.row.id === selectedProjectId}
          homeDirectory={homeDirectory}
          currentTimestamp={currentTimestamp}
          ingestActive={ingestActive}
          runtimeStates={runtimes?.filter((runtime) => project.agents.some((agent) => agent.kind === runtime.kind)).map((runtime) => runtime.state)}
          backfill={projectBackfills.get(project.row.id) ?? null}
          onProjectSelect={onProjectSelect}
        />
      ))}
      {projects.length === 0 && !pending && (
        <EmptyState
          title="No projects yet"
          description="rosettad has not indexed any project yet — leave it serving and this fills in."
        />
      )}
    </div>
  );
}
