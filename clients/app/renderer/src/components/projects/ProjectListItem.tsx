import type { CSSProperties } from "react";
import { AgentBadges } from "../../lib/agents";
import { fmtTokens, lastActive, tildePath, tokenText } from "../../lib/format";
import type { ProjectViewModel } from "../../lib/model";
import { ProjectActivitySparkline } from "./ProjectActivitySparkline";

type ProjectListItemStyle = CSSProperties & {
  "--project-color": string;
  "--spend-share": string;
};

function TokenFigure({ value }: { readonly value: number }) {
  const formattedValue = fmtTokens(value);
  return (
    <span className="ah-project-list-item__token-figure">
      <b>{formattedValue.n}</b>
      <i>{formattedValue.u}</i>
    </span>
  );
}

export interface ProjectListItemProps {
  readonly project: ProjectViewModel;
  /** Maximum across the complete project collection, not a filtered subset. */
  readonly maximumTokensLast30Days: number;
  readonly isSelected: boolean;
  /** `null` hides the search hit badge. */
  readonly searchHitCount: number | null;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  readonly onProjectSelect: (projectId: string) => void;
}

export function ProjectListItem({
  project,
  maximumTokensLast30Days,
  isSelected,
  searchHitCount,
  homeDirectory,
  currentTimestamp,
  onProjectSelect,
}: ProjectListItemProps) {
  const spendShare = maximumTokensLast30Days > 0
    ? (project.tokensLast30Days / maximumTokensLast30Days) * 100
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
      style={{
        "--project-color": project.tint,
        "--spend-share": `${spendShare.toFixed(2)}%`,
      } as ProjectListItemStyle}
      aria-current={isSelected ? "true" : undefined}
      aria-label={`${project.row.name} · ${tokenText(
        project.tokensLast30Days,
      )} tokens over 30 days${searchHitLabel}${
        lastActiveLabel ? ` · last active ${lastActiveLabel}` : ""
      }`}
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
        <span className="ah-project-list-item__metrics">
          <ProjectActivitySparkline values={project.recentTokenBuckets} />
          <TokenFigure value={project.tokensLast30Days} />
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
