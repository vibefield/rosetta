import type { ProjectBackfill, RuntimeBackfill } from "@rosetta/protocol";
import type { ProjectViewModel } from "../../lib/model";
import { useListMotion } from "../../lib/use-list-motion";
import { useDeferredListOrder } from "../../lib/use-deferred-list-order";
import { ListActivityNotice } from "../ListActivityNotice";
import { EmptyState } from "../ui";
import { ProjectListItem } from "./ProjectListItem";

export interface ProjectListProps {
  /** Projects in display order; sorting and filtering belong to the container. */
  readonly projects: readonly ProjectViewModel[];
  /** A deliberate sort choice establishes a fresh view. */
  readonly orderScope?: string;
  readonly selectedProjectId: string | null;
  readonly maximumLifetimeCost: number;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  /** A backfill is running: unfinished projects cook rather than lie. */
  readonly ingestActive: boolean;
  readonly runtimes?: readonly RuntimeBackfill[] | undefined;
  /** The daemon's per-project backfill fold, keyed by project id. */
  readonly projectBackfills: ReadonlyMap<string, ProjectBackfill>;
  /** The list itself is still being censused — only the ready may claim
   * emptiness, so the empty state yields to the section's veil. */
  readonly pending: boolean;
  readonly onProjectSelect: (projectId: string) => void;
}

export function ProjectList({
  projects,
  orderScope = "projects",
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
  const layout = useDeferredListOrder(projects.map(project => ({ id: project.row.id, day: null })), orderScope);
  const motionRef = useListMotion(JSON.stringify(layout.items), orderScope, layout.scrollRef);
  const byId = new Map(projects.map(project => [project.row.id, project]));
  const visible = layout.items.flatMap(item => byId.get(item.id) ?? []);

  return (
    <div className="ah-list-view" {...layout.hostProps}>
      <div className="ah-scroll ah-motion-list" ref={motionRef} onScroll={layout.onScroll}>
        {visible.map((project) => (
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
      <ListActivityNotice pending={layout.pending} onShow={layout.apply} noun="projects" />
    </div>
  );
}
