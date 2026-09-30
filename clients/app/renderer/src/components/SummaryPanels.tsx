// Project and portfolio summary panels decompose the ledger's totals at two
// levels. The project panel heads column ②; the portfolio panel heads column
// ①. Every chart derives from the same series as the ledger, so their figures
// cannot disagree. When collapsed, each panel becomes its column header.
import { useEffect, useMemo, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import type { UsageResponse } from "@rosetta/protocol";
import { ActivityChartDialog, CalendarHeatmap, Streamgraph, Treemap } from "./charts";
import type { TreemapData } from "./charts";
import { ColumnHeading, SegmentedControl, Veil } from "./ui";
import {
  calculateActivityStats,
  createCalendarHeatmapData,
  createModelUsageStreamgraphData,
} from "../lib/chart-data";
import { fmtTokens, tokenText } from "../lib/format";
import { FullscreenGlyph, GitHubGlyph, StatsGlyph, TreeGlyph } from "../lib/icons";
import { getCalendarDayKey } from "../lib/model";
import type { ProjectViewModel } from "../lib/model";
import { NO_SPEND, addSpend, coverage, fmtUsd, spendDescription, usdText } from "../lib/spend";
import type { Spend } from "../lib/spend";

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

/** List-price spend (§4.1.3): a partial figure is a lower bound marked `+`;
 * usage the catalog cannot price reads `n/p`, never $0. */
function SpendSummaryValue({ spend, unknown = false }: { spend: Spend; unknown?: boolean }) {
  if (unknown) return <b>—</b>;
  const state = coverage(spend);
  if (spend.costMicroUsd === null) return <b>{state === "unavailable" ? "n/p" : "—"}</b>;
  const figure = fmtUsd(spend.costMicroUsd);
  return (
    <b>
      {figure.n}
      <small>{`${figure.u}${state === "partial" ? "+" : ""}`}</small>
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
  title?: string;
}

function SummaryStats({ stats }: { stats: readonly SummaryStat[] }) {
  return (
    <div className="ah-stats">
      {stats.map((stat) => (
        <div className="ah-stat" key={stat.label} title={stat.title}>
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
  // Aggregate figures become available after canonical publication and
  // their own requests finish, independently of transcript readiness.
  const isVeiled = ingestActive || !project.statsReady;
  const [isExpanded, setIsExpanded] = useState(true);
  const [isActivityOpen, setIsActivityOpen] = useState(false);
  useEffect(() => setIsActivityOpen(false), [project.row.id]);
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
              <GitHubGlyph />
            </button>
          )}
        </span>
      </div>
      {isExpanded && (
        <div className="ah-meta__body ah-veilhost">
          {isVeiled && (
            <Veil
              label={ingestActive
                ? censusRunning ? "Discovering conversations…" : "Preparing activity summaries…"
                : "Loading activity summaries…"}
              progress={null}
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
                // The day count stays in the calendar's caption below.
                value: (
                  <SpendSummaryValue
                    spend={project.spendLast53Weeks}
                    unknown={isVeiled && project.tokensLast53Weeks === 0}
                  />
                ),
                label: "estimated cost",
                title: `${spendDescription(project.spendLast53Weeks, tokenText)} · 53 weeks`,
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
                  <div className="ah-chart-launcher">
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
                    <button type="button" className="ah-chart-launcher__button"
                      aria-label={`Expand activity chart for ${project.row.name}`}
                      aria-haspopup="dialog" disabled={isVeiled}
                      onClick={() => setIsActivityOpen(true)}>
                      <span className="ah-chart-launcher__hint"><FullscreenGlyph /> Expand</span>
                    </button>
                  </div>
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
      {isActivityOpen && <ActivityChartDialog name={project.row.name} color={project.tint}
        data={heatmapData} today={getCalendarDayKey(currentTimestamp)}
        onClose={() => setIsActivityOpen(false)} />}
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
  const summariesPending = ingestActive || projects.some((project) => !project.statsReady);
  const summary = useMemo(() => {
    // Match the project list's lifetime spend ranking, tokens breaking ties.
    const treemapProjects = [...projects]
      .filter((project) => (project.lifetimeSpend.costMicroUsd ?? 0) > 0)
      .sort((left, right) =>
        (right.lifetimeSpend.costMicroUsd ?? 0) - (left.lifetimeSpend.costMicroUsd ?? 0)
          || right.lifetimeTokens - left.lifetimeTokens,
      )
      .slice(0, MAX_TREEMAP_PROJECT_COUNT);
    const treemapData: TreemapData = {
      items: treemapProjects.map((project) => ({
        id: project.row.id,
        label: project.row.name,
        value: project.lifetimeSpend.costMicroUsd ?? 0,
        color: project.tint,
        // A lower bound when part of the project's usage has no list price or
        // no recorded output (§4.1.3: partial coverage is always visible).
        partial: coverage(project.lifetimeSpend) === "partial",
      })),
    };
    // Projects the treemap cannot size: usage, none of it priced.
    const unpricedOnlyProjects = projects.filter(
      (project) => coverage(project.lifetimeSpend) === "unavailable",
    ).length;

    return {
      unpricedOnlyProjects,
      sessionCount: projects.reduce(
        (total, project) => total + project.rootSessionCount,
        0,
      ),
      messageCount: projects.reduce(
        (total, project) => total + project.lifetimeMessages,
        0,
      ),
      tokenCount: projects.reduce(
        (total, project) => total + project.lifetimeTokens,
        0,
      ),
      spend: projects.reduce<Spend>(
        (total, project) => addSpend(total, project.lifetimeSpend),
        NO_SPEND,
      ),
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
          {summariesPending && (
            <Veil
              label={ingestActive
                ? censusRunning ? "Discovering conversations…" : "Preparing activity summaries…"
                : "Loading activity summaries…"}
              progress={null}
            />
          )}
          <SummaryStats
            stats={[
              { value: <b>{summary.sessionCount}</b>, label: "sessions" },
              {
                value: <CountFigure value={summary.messageCount} veiled={summariesPending} />,
                label: "messages",
              },
              {
                value: (
                  <TokenSummaryValue
                    value={summary.tokenCount}
                    unknown={summariesPending && summary.tokenCount === 0}
                  />
                ),
                label: "tokens",
              },
              {
                value: (
                  <SpendSummaryValue
                    spend={summary.spend}
                    unknown={summariesPending && summary.tokenCount === 0}
                  />
                ),
                label: "estimated cost",
                title: `${spendDescription(summary.spend, tokenText)} · all time`,
              },
            ]}
          />
          <div className="ah-chartcard">
            <Treemap
              data={summary.treemapData}
              ariaLabel="Top projects by lifetime estimated spend, where area represents share among displayed projects"
              emptyMessage="No priced project usage"
              showFooter={false}
              height={SUMMARY_CHART_CONTENT_HEIGHT}
              minimumVisibleShare={0}
              selectedItemId={selectedProjectId}
              onItemSelect={(item) => onProjectSelect(item.id)}
              formatValue={usdText}
              getItemAccessibleLabel={(item, context) =>
                `${item.label} · ${usdText(item.value)}${item.partial ? "+" : ""} estimated cost${
                  item.partial ? " (a lower bound)" : ""
                } · ${(context.share * 100).toFixed(1)}% of displayed lifetime spend`
              }
            />
            {summary.unpricedOnlyProjects > 0 && (
              <p className="ah-treemap__empty">
                {summary.unpricedOnlyProjects === 1
                  ? "1 project has usage with no list price and is not shown"
                  : `${summary.unpricedOnlyProjects} projects have usage with no list price and are not shown`}
              </p>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
