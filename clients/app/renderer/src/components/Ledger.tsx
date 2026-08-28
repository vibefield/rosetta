// Column ① — the Ledger. Option A ("ground fill", chosen 2026-08-27): the
// wash IS the comparison, scaled to the busiest project across the whole
// ledger and never to the filtered set. Three encodings, three elements:
// figure = exactly how much · wash/bar = how much comparably · shape = when.
import type { CSSProperties } from "react";
import { AgentBadges } from "../lib/agents";
import { fmtTokens, lastActive, tildePath, tokenText } from "../lib/format";
import type { ProjectVM } from "../lib/model";

function Activity({ vm }: { vm: ProjectVM }) {
  const peak = Math.max(...vm.buckets, 0) || 1;
  return (
    <div className="ah-activity" role="img" aria-label="token activity, last 30 days">
      {vm.buckets.map((v, i) =>
        v === 0 ? (
          <i key={i} data-zero="" style={{ "--h": "1px" } as CSSProperties} />
        ) : (
          <i key={i} style={{ "--h": `${Math.max(12, Math.round((v / peak) * 100))}%` } as CSSProperties} />
        ),
      )}
    </div>
  );
}

function TokenFigure({ n }: { n: number }) {
  const t = fmtTokens(n);
  return (
    <span className="ah-tok">
      <b>{t.n}</b>
      <i>{t.u}</i>
    </span>
  );
}

export function ProjectRowButton({
  vm,
  maxTokens,
  active,
  hits,
  home,
  nowMs,
  onSelect,
}: {
  vm: ProjectVM;
  maxTokens: number;
  active: boolean;
  hits: number | null;
  home: string | null;
  nowMs: number;
  onSelect: (id: string) => void;
}) {
  const share = maxTokens > 0 ? (vm.tokens30 / maxTokens) * 100 : 0;
  const last = vm.lastActiveMs === null ? null : lastActive(vm.lastActiveMs, nowMs);
  const path = tildePath(vm.row.root_paths[0] ?? "", home);
  return (
    <button
      type="button"
      className={`ah-projrow${active ? " is-active" : ""}`}
      style={{ "--tint": vm.tint, "--share": `${share.toFixed(2)}%` } as CSSProperties}
      onClick={() => onSelect(vm.row.id)}
      aria-label={`${vm.row.name} · ${tokenText(vm.tokens30)} tokens over 30 days${
        last ? ` · last active ${last}` : ""
      }`}
    >
      <i className="ah-tint" />
      <span className="ah-projrow__head">
        <strong title={vm.row.name}>{vm.row.name}</strong>
        {hits !== null && <span className="ah-hits tnum">{hits}</span>}
        <AgentBadges shares={vm.agents} />
      </span>
      <span className="ah-projrow__sub">
        <small title={path}>{path}</small>
      </span>
      <span className="ah-projrow__foot">
        <span className="ah-quant">
          <Activity vm={vm} />
          <TokenFigure n={vm.tokens30} />
        </span>
        {last !== null && (
          <span className="ah-last" data-live={last === "now" ? "" : undefined} title={`last active ${last}`}>
            {last}
          </span>
        )}
      </span>
    </button>
  );
}

export type LedgerSort = "spend" | "recent";

export function LedgerColumn({
  vms,
  activeId,
  sort,
  onSort,
  hitCounts,
  home,
  nowMs,
  onSelect,
}: {
  vms: ProjectVM[];
  activeId: string | null;
  sort: LedgerSort;
  onSort: (sort: LedgerSort) => void;
  /** Non-null in search mode: projects with zero hits leave the list. */
  hitCounts: Map<string, number> | null;
  home: string | null;
  nowMs: number;
  onSelect: (id: string) => void;
}) {
  const searching = hitCounts !== null;
  const maxTokens = Math.max(...vms.map((vm) => vm.tokens30), 0);
  const sorted = [...vms].sort((a, b) =>
    sort === "recent" ? (b.lastActiveMs ?? 0) - (a.lastActiveMs ?? 0) : b.tokens30 - a.tokens30,
  );
  const shown = searching ? sorted.filter((vm) => (hitCounts.get(vm.row.id) ?? 0) > 0) : sorted;
  const withHits = searching ? shown.length : null;
  return (
    <div className="ah-col">
      <div className="ah-meta">
        <div className="ah-meta__head">
          <span className="ah-colhead">
            {searching ? (
              <span className="ah-eyebrow">Projects with hits</span>
            ) : (
              <>
                <i style={{ "--tint": "var(--vf-text-tertiary)" } as CSSProperties} />
                <strong>Projects</strong>
                <span className="ah-bubble" title={`${vms.length} projects in this list`}>
                  {vms.length}
                </span>
              </>
            )}
          </span>
          {searching && (
            <span className="ah-caption tnum">
              {withHits} {withHits === 1 ? "project" : "projects"}
            </span>
          )}
        </div>
      </div>
      <div className="ah-listbar">
        <p className="ah-colnote">spend over 30d</p>
        <div className="ah-seg ah-sortseg" role="group" aria-label="Rank projects by">
          <button type="button" className={sort === "spend" ? "is-on" : ""} onClick={() => onSort("spend")}>
            spend
          </button>
          <button type="button" className={sort === "recent" ? "is-on" : ""} onClick={() => onSort("recent")}>
            recent
          </button>
        </div>
      </div>
      <div className="ah-scroll">
        {shown.map((vm) => (
          <ProjectRowButton
            key={vm.row.id}
            vm={vm}
            maxTokens={maxTokens}
            active={vm.row.id === activeId}
            hits={searching ? (hitCounts.get(vm.row.id) ?? 0) : null}
            home={home}
            nowMs={nowMs}
            onSelect={onSelect}
          />
        ))}
        {shown.length === 0 && (
          <div className="ah-empty">
            <strong>{searching ? "No project matched" : "No projects yet"}</strong>
            <p>
              {searching
                ? "No indexed message contains that."
                : "rosettad has not indexed any project yet — leave it serving and this fills in."}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}
