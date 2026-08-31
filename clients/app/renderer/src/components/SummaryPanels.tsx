// Project and portfolio summary panels decompose the ledger's totals at two
// levels. The project panel heads column ②; the portfolio panel heads column
// ①. Every chart derives from the same series as the ledger, so their figures
// cannot disagree. When collapsed, each panel becomes its column header.
import { useMemo, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import type { UsageResponse } from "@rosetta/protocol";
import { CalendarHeatmap, Streamgraph, Treemap } from "./charts";
import type { TreemapData } from "./charts";
import { ColumnHeading, SegmentedControl, Veil } from "./ui";
import { sessionCountLabel } from "../lib/ingest";
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
const MAX_TREEMAP_PROJECT_COUNT = 10;
// Matches the project chart's 25px selector track, 9px gap, and 106px chart body.
const SUMMARY_CHART_CONTENT_HEIGHT = 140;

const PROJECT_CHART_OPTIONS = [
  { value: "activity", label: "Overview" },
  { value: "models", label: "Models" },
] as const;

/* ── four chips, not four cards ─────────────────────────────────────────── */

function TokenSummaryValue({ value, unknown = false }: { value: number; unknown?: boolean }) {
  if (unknown) return <b>—</b>;
  const formattedValue = fmtTokens(value);
  return (
    <b>
      {formattedValue.n}
      <small>{formattedValue.u}</small>
    </b>
  );
}

/** A veiled figure that has not landed yet is a dash, never a claimed zero
 * (the design record's rule for every cooking slot). */
function CountFigure({ value, veiled }: { readonly value: number; readonly veiled: boolean }) {
  return <b>{veiled && value === 0 ? "—" : value.toLocaleString()}</b>;
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
  /** A backfill is running: figures for an unfinished project stay veiled.
   * Once the phase is live a veil never descends again. */
  readonly ingestActive: boolean;
  /** The census itself is still running: the counts below this panel are
   * incomplete, so its veil defers to the column's census veil instead of
   * stacking a near-duplicate counting label on top of it. */
  readonly censusRunning: boolean;
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
  ingestActive,
  censusRunning,
  isWorktreeVisible,
  onToggleWorktree,
}: ProjectSummaryPanelProps) {
  // Veiled until every one of THIS project's sessions is indexed — beneath
  // the frost the charts keep drawing the partial sums as they grow.
  const isVeiled = ingestActive && project.readiness !== "ready";
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
        <ColumnHeading
          name={project.row.name}
          tint={project.tint}
          count={visibleSessionCount}
          countTitle={`${visibleSessionCount} sessions in this list`}
        />
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
        <div className="ah-meta__body ah-veilhost">
          {isVeiled && (
            <Veil
              label={
                censusRunning
                  ? "waiting on the census"
                  : `counting · ${sessionCountLabel(
                      project.indexedSessionCount,
                      project.sessionCount,
                    )}`
              }
              progress={
                !censusRunning && project.sessionCount > 0
                  ? project.indexedSessionCount / project.sessionCount
                  : null
              }
            />
          )}
          <SummaryStats
            stats={[
              { value: <b>{project.rootSessionCount}</b>, label: "sessions" },
              {
                value: <CountFigure value={project.lifetimeMessages} veiled={isVeiled} />,
                label: "messages",
              },
              {
                value: (
                  <TokenSummaryValue
                    value={project.tokensLast53Weeks}
                    unknown={isVeiled && project.tokensLast53Weeks === 0}
                  />
                ),
                label: "tokens",
              },
              {
                value: <CountFigure value={activityStats.activeDayCount} veiled={isVeiled} />,
                label: "active days",
              },
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
                    summary={`${
                      isVeiled && activityStats.activeDayCount === 0
                        ? "—"
                        : activityStats.activeDayCount
                    } active days · 53 weeks`}
                  />
                  <p className="ah-meta__facts">
                    <b>
                      {isVeiled && activityStats.longestStreakLength === 0
                        ? "—"
                        : `${activityStats.currentStreakLength}d`}
                    </b>{" "}
                    streak ·{" "}
                    <b>
                      {isVeiled && activityStats.longestStreakLength === 0
                        ? "—"
                        : `${activityStats.longestStreakLength}d`}
                    </b>{" "}
                    best
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
  /** A backfill is running: the portfolio's totals are still counting, so
   * the whole panel stays veiled. It unveils last — the finale. */
  readonly ingestActive: boolean;
  /** The census is still naming sessions: defer to the column's census veil
   * rather than stacking a second counting label above it. */
  readonly censusRunning: boolean;
  readonly onProjectSelect: (projectId: string) => void;
}

export function PortfolioSummaryPanel({
  projects,
  selectedProjectId,
  ingestActive,
  censusRunning,
  onProjectSelect,
}: PortfolioSummaryPanelProps) {
  const [isExpanded, setIsExpanded] = useState(true);
  const readinessCounts = useMemo(
    () =>
      projects.reduce(
        (sum, project) => ({
          indexed: sum.indexed + project.indexedSessionCount,
          total: sum.total + project.sessionCount,
        }),
        { indexed: 0, total: 0 },
      ),
    [projects],
  );
  const summary = useMemo(() => {
    const activeDayIndices = new Set<number>();
    for (const project of projects) {
      project.dailyTokens.forEach((value, dayIndex) => {
        if (value > 0) activeDayIndices.add(dayIndex);
      });
    }

    const treemapProjects = [...projects]
      .sort(
        (left, right) =>
          right.tokensLast30Days - left.tokensLast30Days ||
          left.row.name.localeCompare(right.row.name) ||
          left.row.id.localeCompare(right.row.id),
      )
      .slice(0, MAX_TREEMAP_PROJECT_COUNT);
    const treemapData: TreemapData = {
      items: treemapProjects.map((project) => ({
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
        {/* The marker keeps the component's shape and spends no hue: this
            head names a SET, and hue in this room names one project. */}
        <ColumnHeading
          name="Projects"
          tint="var(--vf-text-tertiary)"
          title={null}
          count={projects.length}
          countTitle={`${projects.length} projects in this list`}
        />
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
        <div className="ah-meta__body ah-veilhost">
          {ingestActive && (
            <Veil
              label={
                censusRunning
                  ? "waiting on the census"
                  : `preparing · ${sessionCountLabel(
                      readinessCounts.indexed,
                      readinessCounts.total,
                    )}`
              }
              progress={
                !censusRunning && readinessCounts.total > 0
                  ? readinessCounts.indexed / readinessCounts.total
                  : null
              }
            />
          )}
          <SummaryStats
            stats={[
              { value: <b>{summary.sessionCount}</b>, label: "sessions" },
              {
                value: <CountFigure value={summary.messageCount} veiled={ingestActive} />,
                label: "messages",
              },
              {
                value: (
                  <TokenSummaryValue
                    value={summary.tokenCount}
                    unknown={ingestActive && summary.tokenCount === 0}
                  />
                ),
                label: "tokens",
              },
              {
                value: <CountFigure value={summary.activeDayCount} veiled={ingestActive} />,
                label: "active days",
              },
            ]}
          />
          <div className="ah-chartcard">
            <Treemap
              data={summary.treemapData}
              ariaLabel="Top projects by 30-day spend, where area represents share among displayed projects"
              emptyMessage="No project spend in this window"
              showFooter={false}
              height={SUMMARY_CHART_CONTENT_HEIGHT}
              selectedItemId={selectedProjectId}
              onItemSelect={(item) => onProjectSelect(item.id)}
              formatValue={tokenText}
              getItemAccessibleLabel={(item, context) =>
                `${item.label} · ${tokenText(item.value)} over 30 days · ${(
                  context.share * 100
                ).toFixed(1)}% of displayed spend${
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
