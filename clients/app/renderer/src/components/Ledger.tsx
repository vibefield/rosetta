// Column ① — the Ledger. Option A ("ground fill", chosen 2026-08-27): the
// wash IS the comparison, scaled to the busiest project across the whole
// ledger and never to the filtered set. Three encodings, three elements:
// figure = exactly how much · wash/bar = how much comparably · shape = when.
import type { CSSProperties } from "react";
import { AgentBadges } from "../lib/agents";
import { fmtTokens, lastActive, tildePath, tokenText } from "../lib/format";
import type { ProjectViewModel } from "../lib/model";
import { PortfolioSummaryPanel } from "./SummaryPanels";

type ActivityBarStyle = CSSProperties & { "--h": string };
type ProjectRowStyle = CSSProperties & { "--tint": string; "--share": string };

function RecentTokenActivity({ values }: { values: readonly number[] }) {
  const peak = Math.max(...values, 0) || 1;
  return (
    <div className="ah-activity" role="img" aria-label="token activity, last 30 days">
      {values.map((value, bucketIndex) =>
        value === 0 ? (
          <i
            key={bucketIndex}
            data-zero=""
            style={{ "--h": "1px" } as ActivityBarStyle}
          />
        ) : (
          <i
            key={bucketIndex}
            style={
              {
                "--h": `${Math.max(12, Math.round((value / peak) * 100))}%`,
              } as ActivityBarStyle
            }
          />
        ),
      )}
    </div>
  );
}

function TokenFigure({ value }: { value: number }) {
  const formattedValue = fmtTokens(value);
  return (
    <span className="ah-tok">
      <b>{formattedValue.n}</b>
      <i>{formattedValue.u}</i>
    </span>
  );
}

export interface ProjectRowButtonProps {
  readonly project: ProjectViewModel;
  readonly maximumTokensLast30Days: number;
  readonly isSelected: boolean;
  readonly searchHitCount: number | null;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  readonly onProjectSelect: (projectId: string) => void;
}

export function ProjectRowButton({
  project,
  maximumTokensLast30Days,
  isSelected,
  searchHitCount,
  homeDirectory,
  currentTimestamp,
  onProjectSelect,
}: ProjectRowButtonProps) {
  const spendShare =
    maximumTokensLast30Days > 0
      ? (project.tokensLast30Days / maximumTokensLast30Days) * 100
      : 0;
  const lastActiveLabel =
    project.lastActiveMs === null
      ? null
      : lastActive(project.lastActiveMs, currentTimestamp);
  const workspacePath = project.row.root_paths[0]
    ? tildePath(project.row.root_paths[0], homeDirectory)
    : "No workspace recorded";
  return (
    <button
      type="button"
      className={`ah-projrow${isSelected ? " is-active" : ""}`}
      style={
        {
          "--tint": project.tint,
          "--share": `${spendShare.toFixed(2)}%`,
        } as ProjectRowStyle
      }
      onClick={() => onProjectSelect(project.row.id)}
      aria-label={`${project.row.name} · ${tokenText(
        project.tokensLast30Days,
      )} tokens over 30 days${
        lastActiveLabel ? ` · last active ${lastActiveLabel}` : ""
      }`}
    >
      <i className="ah-tint" />
      <span className="ah-projrow__head">
        <strong title={project.row.name}>{project.row.name}</strong>
        {searchHitCount !== null && <span className="ah-hits tnum">{searchHitCount}</span>}
        <AgentBadges shares={project.agents} />
      </span>
      <span className="ah-projrow__sub">
        <small title={workspacePath}>{workspacePath}</small>
      </span>
      <span className="ah-projrow__foot">
        <span className="ah-quant">
          <RecentTokenActivity values={project.recentTokenBuckets} />
          <TokenFigure value={project.tokensLast30Days} />
        </span>
        {lastActiveLabel !== null && (
          <span
            className="ah-last"
            data-live={lastActiveLabel === "now" ? "" : undefined}
            title={`last active ${lastActiveLabel}`}
          >
            {lastActiveLabel}
          </span>
        )}
      </span>
    </button>
  );
}

export type LedgerSort = "spend" | "recent";

export interface LedgerColumnProps {
  readonly projects: readonly ProjectViewModel[];
  readonly selectedProjectId: string | null;
  readonly sort: LedgerSort;
  readonly onSortChange: (sort: LedgerSort) => void;
  /** Non-null in search mode: projects with zero hits leave the list. */
  readonly searchHitCounts: ReadonlyMap<string, number> | null;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  readonly onProjectSelect: (projectId: string) => void;
}

export function LedgerColumn({
  projects,
  selectedProjectId,
  sort,
  onSortChange,
  searchHitCounts,
  homeDirectory,
  currentTimestamp,
  onProjectSelect,
}: LedgerColumnProps) {
  const isSearching = searchHitCounts !== null;
  const maximumTokensLast30Days = projects.reduce(
    (maximum, project) => Math.max(maximum, project.tokensLast30Days),
    0,
  );
  const sortedProjects = [...projects].sort((left, right) =>
    sort === "recent"
      ? (right.lastActiveMs ?? 0) - (left.lastActiveMs ?? 0)
      : right.tokensLast30Days - left.tokensLast30Days,
  );
  const visibleProjects = isSearching
    ? sortedProjects.filter(
        (project) => (searchHitCounts.get(project.row.id) ?? 0) > 0,
      )
    : sortedProjects;
  const projectCountWithHits = isSearching ? visibleProjects.length : null;
  return (
    <div className="ah-col">
      {isSearching ? (
        // In search the block goes, exactly as ②'s does, and the head becomes
        // an eyebrow: a portfolio treemap above a list filtered to three would
        // be answering a question nobody asked.
        <div className="ah-meta">
          <div className="ah-meta__head">
            <span className="ah-eyebrow">Projects with hits</span>
            <span className="ah-caption tnum">
              {projectCountWithHits} {projectCountWithHits === 1 ? "project" : "projects"}
            </span>
          </div>
        </div>
      ) : (
        <PortfolioSummaryPanel
          projects={projects}
          selectedProjectId={selectedProjectId}
          onProjectSelect={onProjectSelect}
        />
      )}
      <div className="ah-listbar">
        <p className="ah-colnote">spend over 30d</p>
        <div className="ah-seg ah-sortseg" role="group" aria-label="Rank projects by">
          <button
            type="button"
            className={sort === "spend" ? "is-on" : ""}
            onClick={() => onSortChange("spend")}
          >
            spend
          </button>
          <button
            type="button"
            className={sort === "recent" ? "is-on" : ""}
            onClick={() => onSortChange("recent")}
          >
            recent
          </button>
        </div>
      </div>
      <div className="ah-scroll">
        {visibleProjects.map((project) => (
          <ProjectRowButton
            key={project.row.id}
            project={project}
            maximumTokensLast30Days={maximumTokensLast30Days}
            isSelected={project.row.id === selectedProjectId}
            searchHitCount={
              isSearching ? (searchHitCounts.get(project.row.id) ?? 0) : null
            }
            homeDirectory={homeDirectory}
            currentTimestamp={currentTimestamp}
            onProjectSelect={onProjectSelect}
          />
        ))}
        {visibleProjects.length === 0 && (
          <div className="ah-empty">
            <strong>{isSearching ? "No project matched" : "No projects yet"}</strong>
            <p>
              {isSearching
                ? "No indexed message contains that."
                : "rosettad has not indexed any project yet — leave it serving and this fills in."}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
