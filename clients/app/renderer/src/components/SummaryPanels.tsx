// Project and portfolio summary panels decompose the ledger's totals at two
// levels. The project panel heads column ②; the portfolio panel heads column
// ①. Every chart derives from the same series as the ledger, so their figures
// cannot disagree. When collapsed, each panel becomes its column header.
import { useMemo, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import type { UsageResponse } from "@rosetta/protocol";
import { CalendarHeatmap, Streamgraph, Treemap } from "./charts";
import type { TreemapData } from "./charts";
import { SegmentedControl } from "./ui";
import {
  calculateActivityStats,
  createCalendarHeatmapData,
  createModelUsageStreamgraphData,
} from "../lib/chart-data";
import { fmtTokens, tokenText } from "../lib/format";
import { GitGlyph, StatsGlyph, TreeGlyph } from "../lib/icons";
import type { ProjectViewModel } from "../lib/model";

type TintStyle = CSSProperties & { "--tint": string };
type ProjectChart = "activity" | "models";

const PROJECT_CHART_OPTIONS = [
  { value: "activity", label: "Overview" },
  { value: "models", label: "Models" },
] as const;

/* ── four chips, not four cards ─────────────────────────────────────────── */

function TokenSummaryValue({ value }: { value: number }) {
  const formattedValue = fmtTokens(value);
  return (
    <b>
      {formattedValue.n}
      <small>{formattedValue.u}</small>
    </b>
  );
}

interface SummaryStat {
  label: string;
  value: ReactNode;
}

function SummaryStats({ stats }: { stats: readonly SummaryStat[] }) {
  return (
    <div className="ah-stats">
      {stats.map((stat) => (
        <div className="ah-stat" key={stat.label}>
          {stat.value}
          <span>{stat.label}</span>
        </div>
      ))}
    </div>
  );
}

/* ── project summary panel (column ②'s header) ──────────────────────────── */

function formatHourLabel(hour: number): string {
  if (hour === 0) return "12 AM";
  if (hour < 12) return `${hour} AM`;
  if (hour === 12) return "12 PM";
  return `${hour - 12} PM`;
}

export interface ProjectSummaryPanelProps {
  readonly project: ProjectViewModel;
  readonly visibleSessionCount: number;
  readonly usage: UsageResponse | null;
  readonly activityGridStartTimestamp: number;
  readonly currentTimestamp: number;
  /** `null` means the browser build has no desktop worktree integration. */
  readonly isWorktreeVisible: boolean | null;
  readonly onToggleWorktree: () => void;
}

export function ProjectSummaryPanel({
  project,
  visibleSessionCount,
  usage,
  activityGridStartTimestamp,
  currentTimestamp,
  isWorktreeVisible,
  onToggleWorktree,
}: ProjectSummaryPanelProps) {
  const [isExpanded, setIsExpanded] = useState(true);
  const [activeChart, setActiveChart] = useState<ProjectChart>(() =>
    new URLSearchParams(window.location.search).get("tab") === "models" ? "models" : "activity",
  );
  const activityStats = useMemo(
    () => calculateActivityStats(project.dailyTokens, project.currentDayIndex),
    [project.currentDayIndex, project.dailyTokens],
  );
  const heatmapData = useMemo(
    () => createCalendarHeatmapData(project.dailyTokens, activityGridStartTimestamp),
    [activityGridStartTimestamp, project.dailyTokens],
  );
  const modelUsageData = useMemo(
    () =>
      usage
        ? createModelUsageStreamgraphData(usage, currentTimestamp)
        : { domain: [], series: [] },
    [currentTimestamp, usage],
  );
  const peakHour =
    usage && usage.hours.length > 0
      ? usage.hours.reduce((busiest, hour) =>
          hour.tokens > busiest.tokens ? hour : busiest,
        ).hour
      : null;
  const primaryModelLabel = modelUsageData.series[0]?.label ?? null;
  // The repo act opens a REMOTE. A git_repo that is a local path is a fact
  // about discovery, not an address anyone can visit — no button for it.
  const repositoryUrl =
    project.row.git_repo !== null && !project.row.git_repo.startsWith("/")
      ? project.row.git_repo
      : null;

  return (
    <div className="ah-meta" style={{ "--tint": project.tint } as TintStyle}>
      <div className="ah-meta__head">
        <span className="ah-colhead">
          <i />
          <strong title={project.row.name}>{project.row.name}</strong>
          <span
            className="ah-bubble"
            title={`${visibleSessionCount} sessions in this list`}
          >
            {visibleSessionCount}
          </span>
        </span>
        <span className="ah-meta__acts">
          {/* the two toggles sit together and the one act sits last, so a
              project with no remote never moves a control that has one */}
          {isWorktreeVisible !== null && (
            <button
              type="button"
              className="ah-iconbtn"
              aria-pressed={isWorktreeVisible}
              title={isWorktreeVisible ? "Hide the worktree" : "Show the worktree"}
              aria-label="Toggle the project worktree"
              onClick={onToggleWorktree}
            >
              <TreeGlyph />
            </button>
          )}
          <button
            type="button"
            className="ah-iconbtn"
            aria-pressed={isExpanded}
            title={isExpanded ? "Hide the figures" : "Show the figures"}
            aria-label="Toggle the figures"
            onClick={() => setIsExpanded((current) => !current)}
          >
            <StatsGlyph />
          </button>
          {/* absent, not disabled: a project without a known remote has no
              repo we can claim to open */}
          {repositoryUrl !== null && (
            <button
              type="button"
              className="ah-iconbtn"
              title={`Open ${repositoryUrl}`}
              aria-label="Open the git repository"
              onClick={() =>
                window.open(
                  repositoryUrl.includes("://")
                    ? repositoryUrl
                    : `https://${repositoryUrl}`,
                )
              }
            >
              <GitGlyph />
            </button>
          )}
        </span>
      </div>
      {isExpanded && (
        <div className="ah-meta__body">
          <SummaryStats
            stats={[
              { value: <b>{project.rootSessionCount}</b>, label: "sessions" },
              { value: <b>{project.lifetimeMessages.toLocaleString()}</b>, label: "messages" },
              {
                value: <TokenSummaryValue value={project.tokensLast53Weeks} />,
                label: "tokens",
              },
              { value: <b>{activityStats.activeDayCount}</b>, label: "active days" },
            ]}
          />
          <div className="ah-chartcard">
            <SegmentedControl
              value={activeChart}
              options={PROJECT_CHART_OPTIONS}
              accessibleLabel="Project chart"
              className="ah-tabseg"
              onValueChange={setActiveChart}
            />
            <div className="ah-tabbody">
              {activeChart === "models" ? (
                <Streamgraph
                  data={modelUsageData}
                  title="Tokens per day · 30 days"
                  note="no zero · thickness is the total"
                  ariaLabel="Tokens per day by model over 30 days, shown as a wiggle-minimised streamgraph"
                  emptyMessage="No model-attributed usage in this window"
                  formatValue={tokenText}
                />
              ) : (
                <>
                  <CalendarHeatmap
                    data={heatmapData}
                    color={project.tint}
                    ariaLabel={`Daily token activity for ${project.row.name} over 53 weeks`}
                    summary={`${activityStats.activeDayCount} active days · 53 weeks`}
                  />
                  <p className="ah-meta__facts">
                    <b>{activityStats.currentStreakLength}d</b> streak ·{" "}
                    <b>{activityStats.longestStreakLength}d</b> best
                    {peakHour !== null && (
                      <>
                        {" "}
                        · peak <b>{formatHourLabel(peakHour)}</b>
                      </>
                    )}
                    {primaryModelLabel !== null && (
                      <>
                        {" "}
                        · mostly <b>{primaryModelLabel}</b>
                      </>
                    )}
                  </p>
                </>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/* ── portfolio summary panel (column ①'s header, one level up) ──────────── */

export interface PortfolioSummaryPanelProps {
  readonly projects: readonly ProjectViewModel[];
  readonly selectedProjectId: string | null;
  readonly onProjectSelect: (projectId: string) => void;
}

export function PortfolioSummaryPanel({
  projects,
  selectedProjectId,
  onProjectSelect,
}: PortfolioSummaryPanelProps) {
  const [isExpanded, setIsExpanded] = useState(true);
  const summary = useMemo(() => {
    const activeDayIndices = new Set<number>();
    for (const project of projects) {
      project.dailyTokens.forEach((value, dayIndex) => {
        if (value > 0) activeDayIndices.add(dayIndex);
      });
    }

    const treemapData: TreemapData = {
      items: projects.map((project) => ({
        id: project.row.id,
        label: project.row.name,
        value: project.tokensLast30Days,
        color: project.tint,
      })),
    };

    return {
      sessionCount: projects.reduce(
        (total, project) => total + project.rootSessionCount,
        0,
      ),
      messageCount: projects.reduce(
        (total, project) => total + project.lifetimeMessages,
        0,
      ),
      tokenCount: projects.reduce(
        (total, project) => total + project.tokensLast53Weeks,
        0,
      ),
      activeDayCount: activeDayIndices.size,
      treemapData,
    };
  }, [projects]);

  return (
    <div className="ah-meta">
      <div className="ah-meta__head">
        <span className="ah-colhead">
          {/* the marker keeps the component's shape and spends no hue: this
              head names a set, and hue in this room names one project */}
          <i style={{ "--tint": "var(--vf-text-tertiary)" } as TintStyle} />
          <strong>Projects</strong>
          <span className="ah-bubble" title={`${projects.length} projects in this list`}>
            {projects.length}
          </span>
        </span>
        <span className="ah-meta__acts">
          <button
            type="button"
            className="ah-iconbtn"
            aria-pressed={isExpanded}
            title={isExpanded ? "Hide the figures" : "Show the figures"}
            aria-label="Toggle the figures"
            onClick={() => setIsExpanded((current) => !current)}
          >
            <StatsGlyph />
          </button>
        </span>
      </div>
      {isExpanded && (
        <div className="ah-meta__body">
          <SummaryStats
            stats={[
              { value: <b>{summary.sessionCount}</b>, label: "sessions" },
              { value: <b>{summary.messageCount.toLocaleString()}</b>, label: "messages" },
              { value: <TokenSummaryValue value={summary.tokenCount} />, label: "tokens" },
              { value: <b>{summary.activeDayCount}</b>, label: "active days" },
            ]}
          />
          <div className="ah-chartcard">
            <Treemap
              data={summary.treemapData}
              title="Spend · 30 days"
              note="area is the share"
              ariaLabel="Spend by project, where area represents share"
              emptyMessage="No project spend in this window"
              itemNoun={{ singular: "project", plural: "projects" }}
              selectedItemId={selectedProjectId}
              onItemSelect={(item) => onProjectSelect(item.id)}
              formatValue={tokenText}
              getItemAccessibleLabel={(item, context) =>
                `${item.label} · ${tokenText(item.value)} over 30 days · ${(
                  context.share * 100
                ).toFixed(1)}% of all spend${
                  context.isMinimumSized ? " · shown at the minimum visible size" : ""
                }`
              }
            />
          </div>
        </div>
      )}
    </div>
  );
}
