import type { CSSProperties } from "react";
import type { ProjectBackfill } from "@rosetta/protocol";
import { AgentBadges } from "../../lib/agents";
import { projectBackfillProgress } from "../../lib/ingest";
import { fmtTokens, lastActive, tildePath, tokenText } from "../../lib/format";
import type { ProjectViewModel } from "../../lib/model";
import { ProjectActivitySparkline } from "./ProjectActivitySparkline";

type ProjectListItemStyle = CSSProperties & {
  "--project-color": string;
  "--spend-share": string;
  "--ingest-fill"?: string;
};

function TokenFigure({
  value,
  unknown,
}: {
  readonly value: number;
  /** No figure has landed yet — a dash in the slot, not a claimed zero.
   * Fixed-width either way, so nothing reflows when the number arrives. */
  readonly unknown: boolean;
}) {
  const formattedValue = fmtTokens(value);
  return (
    <span className="ah-project-list-item__token-figure">
      <b>{unknown ? "—" : formattedValue.n}</b>
      <i>{unknown ? "" : formattedValue.u}</i>
    </span>
  );
}

export interface ProjectListItemProps {
  readonly project: ProjectViewModel;
  /** Maximum across the complete project collection, not a filtered subset. */
  readonly maximumLifetimeTokens: number;
  readonly isSelected: boolean;
  /** `null` hides the search hit badge. */
  readonly searchHitCount: number | null;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  /** A backfill is running — projects with unindexed sessions cook. */
  readonly ingestActive: boolean;
  /** This project's row in the daemon's per-project backfill fold; null
   * before the plan settles, and on a daemon too old to carry it. */
  readonly backfill: ProjectBackfill | null;
  readonly onProjectSelect: (projectId: string) => void;
}

export function ProjectListItem({
  project,
  maximumLifetimeTokens,
  isSelected,
  searchHitCount,
  homeDirectory,
  currentTimestamp,
  ingestActive,
  backfill,
  onProjectSelect,
}: ProjectListItemProps) {
  // The cooking card: identity (name, path, agents) renders as itself from
  // the first skim; only the quantitative slots carry the processing state.
  const isCooking = ingestActive && project.readiness !== "ready";
  // The board's fold turns the stove determinate: the accent fills to the
  // project's byte meter instead of breathing. No fold (old daemon, plan
  // not settled) degrades to the indeterminate breathe it always had.
  const ingestFill = isCooking && backfill !== null
    ? projectBackfillProgress(backfill)
    : null;
  const cookingLabel = isCooking
    ? backfill !== null
      ? `figures still cooking · ${backfill.sessions_done.toLocaleString()} of ${backfill.sessions_total.toLocaleString()} sessions` +
        (ingestFill !== null ? ` · ${Math.round(ingestFill * 100)}% read` : "")
      : `figures still cooking · ${project.indexedSessionCount} of ${project.sessionCount} sessions indexed`
    : null;
  const spendShare = maximumLifetimeTokens > 0
    ? (project.lifetimeTokens / maximumLifetimeTokens) * 100
    : 0;
  const lastActiveLabel = project.lastActiveMs === null
    ? null
    : lastActive(project.lastActiveMs, currentTimestamp);
  const workspacePath = project.row.root_paths[0]
    ? tildePath(project.row.root_paths[0], homeDirectory)
    : "No workspace recorded";
  const searchHitLabel = searchHitCount === null
    ? ""
    : ` · ${searchHitCount} ${searchHitCount === 1 ? "search hit" : "search hits"}`;

  return (
    <button
      type="button"
      className={`ah-project-list-item${isSelected ? " is-selected" : ""}`}
      data-motion-key={project.row.id}
      data-cooking={isCooking ? "" : undefined}
      data-fill={ingestFill !== null ? "" : undefined}
      style={{
        "--project-color": project.tint,
        "--spend-share": `${spendShare.toFixed(2)}%`,
        ...(ingestFill !== null
          ? { "--ingest-fill": `${Math.max(2, ingestFill * 100).toFixed(1)}%` }
          : {}),
      } as ProjectListItemStyle}
      aria-current={isSelected ? "true" : undefined}
      aria-label={`${project.row.name} · ${tokenText(
        project.lifetimeTokens,
      )} lifetime tokens${searchHitLabel}${
        lastActiveLabel ? ` · last active ${lastActiveLabel}` : ""
      }${cookingLabel ? ` · ${cookingLabel}` : ""}`}
      onClick={() => onProjectSelect(project.row.id)}
    >
      <i className="ah-project-list-item__accent" />
      <span className="ah-project-list-item__header">
        <strong title={project.row.name}>{project.row.name}</strong>
        {searchHitCount !== null && (
          <span className="ah-project-list-item__search-hits tnum">
            {searchHitCount}
          </span>
        )}
        <AgentBadges shares={project.agents} />
      </span>
      <span className="ah-project-list-item__path">
        <small title={workspacePath}>{workspacePath}</small>
      </span>
      <span className="ah-project-list-item__footer">
        <span
          className="ah-project-list-item__metrics"
          data-cooking={isCooking ? "" : undefined}
          title={cookingLabel ?? undefined}
        >
          <ProjectActivitySparkline values={project.recentTokenBuckets} />
          <TokenFigure
            value={project.lifetimeTokens}
            unknown={isCooking && project.lifetimeTokens === 0}
          />
        </span>
        {lastActiveLabel !== null && (
          <span
            className="ah-last"
            data-live={lastActiveLabel === "now" ? "" : undefined}
            title={`Last active ${lastActiveLabel}`}
          >
            {lastActiveLabel}
          </span>
        )}
      </span>
    </button>
  );
}
