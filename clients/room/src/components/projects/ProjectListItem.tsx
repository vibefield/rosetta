import type { CSSProperties } from "react";
import type { ProjectBackfill, BackfillState } from "@rosetta/protocol";
import { AgentBadges } from "../../lib/agents";
import { progressBytes } from "../../lib/ingest-activity";
import { projectBackfillProgress } from "../../lib/ingest";
import { lastActive, tildePath, tokenText } from "../../lib/format";
import type { ProjectViewModel } from "../../lib/model";
import { coverage, fmtUsd, spendDescription } from "../../lib/spend";
import type { Spend } from "../../lib/spend";
import { useHostInputs } from "../../host-inputs";
import { HostAct } from "../HostAct";
import { ProjectActivitySparkline } from "./ProjectActivitySparkline";

type ProjectListItemStyle = CSSProperties & {
  "--project-color": string;
  "--spend-share": string;
  "--ingest-fill"?: string;
};

/** The row's "how much": estimated spend (§4.1.3). A partial figure is a
 * lower bound and says so with `+`; usage with no list price at all reads
 * `n/p` rather than a claimed $0. */
function SpendFigure({
  spend,
  unknown,
  description,
}: {
  readonly spend: Spend;
  /** No figure has landed yet — a dash in the slot, not a claimed zero.
   * Fixed-width either way, so nothing reflows when the number arrives. */
  readonly unknown: boolean;
  readonly description: string;
}) {
  const state = coverage(spend);
  const figure = spend.costMicroUsd === null ? null : fmtUsd(spend.costMicroUsd);
  return (
    <span
      className="ah-project-list-item__token-figure"
      data-coverage={state}
      title={unknown ? undefined : description}
    >
      <b>{unknown ? "—" : figure === null ? (state === "unavailable" ? "n/p" : "—") : figure.n}</b>
      <i>{unknown || figure === null ? "" : `${figure.u}${state === "partial" ? "+" : ""}`}</i>
    </span>
  );
}

export interface ProjectListItemProps {
  readonly project: ProjectViewModel;
  /** Maximum lifetime estimated spend (micro-USD) across the complete
   * project collection, not a filtered subset. */
  readonly maximumLifetimeCost: number;
  readonly isSelected: boolean;
  readonly homeDirectory: string | null;
  readonly currentTimestamp: number;
  /** Canonical publication is still pending, so aggregate figures wait. */
  readonly ingestActive: boolean;
  readonly runtimeStates?: readonly BackfillState[] | undefined;
  /** This project's row in the daemon's per-project backfill fold; null
   * before the plan settles, and on a daemon too old to carry it. */
  readonly backfill: ProjectBackfill | null;
  readonly onProjectSelect: (projectId: string) => void;
}

export function ProjectListItem({
  project,
  maximumLifetimeCost,
  isSelected,
  homeDirectory,
  currentTimestamp,
  ingestActive,
  runtimeStates,
  backfill,
  onProjectSelect,
}: ProjectListItemProps) {
  // The cooking card: identity (name, path, agents) renders as itself from
  // the first skim; only the quantitative slots carry the processing state.
  const isCooking = ingestActive || !project.statsReady;
  // Fill the accent from observed bytes when the source plan has a known
  // total. Unknown work keeps a quiet accent and an explicit stage label.
  const ingestFill = isCooking && backfill !== null
    ? projectBackfillProgress(backfill)
    : null;
  const cookingLabel = isCooking
    ? backfill !== null
      ? backfill.sessions_done >= backfill.sessions_total
        ? "Preparing activity summaries"
        : runtimeStates?.includes("streaming")
          ? ingestFill !== null
            ? `Reading conversations · ${Math.floor(ingestFill * 100)}%`
            : `Reading conversations · ${progressBytes(backfill.bytes_streamed)}`
          : runtimeStates?.some((state) => state === "preparing" || state === "applying")
            ? "Organizing conversations" : "Waiting for conversations"
      : project.readiness === "ready" ? "Preparing activity summaries" : "Waiting for conversations"
    : null;
  const lifetimeCost = project.lifetimeSpend.costMicroUsd ?? 0;
  const spendShare = maximumLifetimeCost > 0
    ? (lifetimeCost / maximumLifetimeCost) * 100
    : 0;
  const spendLabel = spendDescription(project.lifetimeSpend, tokenText);
  const lastActiveLabel = project.lastActiveMs === null
    ? null
    : lastActive(project.lastActiveMs, currentTimestamp);
  const workspacePath = project.row.root_paths[0]
    ? tildePath(project.row.root_paths[0], homeDirectory)
    : "No workspace recorded";
  // R2: the host's marks, second line and live count, drawn and never read.
  const decoration = useHostInputs().projectRow?.(project.row.id) ?? null;
  const marks = decoration?.marks ?? null;
  const line = decoration?.line ?? null;
  const live = decoration?.live !== undefined && decoration.live > 0 ? decoration.live : null;
  // H4: the host's act at the row's end (pin, hide), or its rows.
  const act = decoration?.act ?? null;
  const className = `ah-project-list-item${isSelected ? " is-selected" : ""}`;
  const style = {
    "--project-color": project.tint,
    "--spend-share": `${spendShare.toFixed(2)}%`,
    ...(ingestFill !== null
      ? { "--ingest-fill": `${(ingestFill * 100).toFixed(1)}%` }
      : {}),
  } as ProjectListItemStyle;
  const accessibleName = `${project.row.name} · ${isCooking && project.lifetimeTokens === 0
    ? "activity summaries pending"
    : `${spendLabel} · ${tokenText(project.lifetimeTokens)} lifetime tokens`}${
    lastActiveLabel ? ` · last active ${lastActiveLabel}` : ""
  }${live !== null ? ` · ${live} live` : ""}${cookingLabel ? ` · ${cookingLabel}` : ""}`;
  // With an act the row holds two buttons, and a button cannot hold another:
  // the row becomes a box, one button covers it (the selection, named as the
  // row always was), and the act sits above it. The rest is drawn for the eye.
  const shown = act === null ? undefined : true;
  const time = live !== null ? (
    <span className="ah-last" data-live="" title={`${live} live now`} aria-hidden={shown}>
      {live} live
    </span>
  ) : lastActiveLabel !== null && (
    <span
      className="ah-last"
      data-live={lastActiveLabel === "now" ? "" : undefined}
      title={`Last active ${lastActiveLabel}`}
      aria-hidden={shown}
    >
      {lastActiveLabel}
    </span>
  );
  const body = (
    <>
      <i className="ah-project-list-item__accent" />
      <span className="ah-project-list-item__header" aria-hidden={shown}>
        <strong title={project.row.name}>{project.row.name}</strong>
        <AgentBadges shares={project.agents} />
      </span>
      <span className="ah-project-list-item__path" data-marks={marks !== null ? "" : undefined} aria-hidden={shown}>
        {marks !== null && <span className="ah-project-list-item__marks">{marks}</span>}
        <small title={workspacePath}>{workspacePath}</small>
      </span>
      {line !== null && <span className="ah-project-list-item__line" aria-hidden={shown}>{line}</span>}
      {cookingLabel && <span className="ah-project-list-item__progress" aria-hidden={shown}>{cookingLabel}</span>}
      <span className="ah-project-list-item__footer">
        <span
          className="ah-project-list-item__metrics"
          data-cooking={isCooking ? "" : undefined}
          title={cookingLabel ?? undefined}
          aria-hidden={shown}
        >
          <ProjectActivitySparkline values={project.recentTokenBuckets} />
          <SpendFigure
            spend={project.lifetimeSpend}
            unknown={isCooking && project.lifetimeTokens === 0}
            description={`${spendLabel} · ${tokenText(project.lifetimeTokens)} tokens`}
          />
        </span>
        {act === null ? time : (
          <span className="ah-project-list-item__end">
            <HostAct act={act} place="row" />
            {time}
          </span>
        )}
      </span>
    </>
  );

  if (act === null) {
    return (
      <button
        type="button"
        className={className}
        data-motion-key={project.row.id}
        data-cooking={isCooking ? "" : undefined}
        data-fill={ingestFill !== null ? "" : undefined}
        data-line={line !== null ? "" : undefined}
        style={style}
        aria-current={isSelected ? "true" : undefined}
        aria-label={accessibleName}
        onClick={() => onProjectSelect(project.row.id)}
      >
        {body}
      </button>
    );
  }
  return (
    <div
      className={className}
      data-motion-key={project.row.id}
      data-cooking={isCooking ? "" : undefined}
      data-fill={ingestFill !== null ? "" : undefined}
      data-line={line !== null ? "" : undefined}
      data-host-act=""
      style={style}
    >
      <button
        type="button"
        className="ah-project-list-item__hit"
        aria-current={isSelected ? "true" : undefined}
        aria-label={accessibleName}
        onClick={() => onProjectSelect(project.row.id)}
      />
      {body}
    </div>
  );
}
