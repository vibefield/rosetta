// The project meta block (column ②'s header) and the portfolio block
// (column ①'s, the same block one level up): what the ledger row's one
// figure decomposes into. Three acts, four figures, two views — and all
// charts derive from the same series the ledger draws, so they cannot
// disagree. Collapsed, each block is just its head: the column header the
// other columns already have.
import { useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import type { UsageResponse } from "@rosetta/protocol";
import { fmtTokens, tokenText } from "../lib/format";
import { GitGlyph, StatsGlyph } from "../lib/icons";
import { HEAT_WEEKS, dayKey, heatStats } from "../lib/model";
import type { ProjectVM } from "../lib/model";
import { streamPaths } from "../lib/stream";
import { TREE_H, TREE_W, treemapTiles } from "../lib/treemap";

const DAY_MS = 86_400_000;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/* ── four chips, not four cards ─────────────────────────────────────────── */

function TokenChipValue({ n }: { n: number }) {
  const t = fmtTokens(n);
  return (
    <b>
      {t.n}
      <small>{t.u}</small>
    </b>
  );
}

function StatChips({ chips }: { chips: { value: ReactNode; label: string }[] }) {
  return (
    <div className="ah-stats">
      {chips.map((chip) => (
        <div className="ah-stat" key={chip.label}>
          {chip.value}
          <span>{chip.label}</span>
        </div>
      ))}
    </div>
  );
}

/* ── the year ribbon ────────────────────────────────────────────────────── */

function heatLevel(v: number, peak: number): number {
  if (v <= 0) return 0;
  const r = v / peak;
  return r > 0.66 ? 4 : r > 0.4 ? 3 : r > 0.18 ? 2 : 1;
}

/** Month labels sit over the column their month opens in. */
function MonthRow({ gridStartMs }: { gridStartMs: number }) {
  const labels: { at: number; text: string }[] = [];
  let last = -1;
  let lastAt = -3;
  for (let w = 0; w < HEAT_WEEKS; w += 1) {
    const d = new Date(gridStartMs + w * 7 * DAY_MS + DAY_MS / 2);
    if (d.getMonth() !== last && w < HEAT_WEEKS - 1) {
      last = d.getMonth();
      if (w - lastAt >= 4) {
        lastAt = w;
        labels.push({ at: (w / HEAT_WEEKS) * 100, text: MONTHS[last] });
      }
    }
  }
  return (
    <div className="ah-months" aria-hidden="true">
      {labels.map((l) => (
        <span key={l.at} style={{ left: `${l.at.toFixed(2)}%` }}>
          {l.text}
        </span>
      ))}
    </div>
  );
}

function Ribbon({ vm }: { vm: ProjectVM }) {
  const peak = Math.max(...vm.yearCells, 0) || 1;
  const cols = [];
  for (let w = 0; w < HEAT_WEEKS; w += 1) {
    const cells = [];
    for (let d = 0; d < 7; d += 1) {
      const v = vm.yearCells[w * 7 + d] ?? -1;
      cells.push(
        v < 0 ? (
          // not yet, which is not the same as empty
          <i key={d} data-l="0" style={{ visibility: "hidden" }} />
        ) : (
          <i key={d} data-l={heatLevel(v, peak)} style={{ "--l": heatLevel(v, peak) } as CSSProperties} />
        ),
      );
    }
    cols.push(
      <span className="ah-heat__col" key={w}>
        {cells}
      </span>,
    );
  }
  return (
    <div className="ah-heat" role="img" aria-label="daily token activity, 53 weeks">
      {cols}
    </div>
  );
}

/* ── the models streamgraph ─────────────────────────────────────────────── */

/** §2.6 accent, assigned stably by model id — never the project tint. */
function modelTint(id: string): string {
  let h = 2166136261;
  for (let i = 0; i < id.length; i += 1) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return `var(--vf-accent-${(((h >>> 0) % 8) + 1).toFixed(0)})`;
}

function modelLabel(id: string): string {
  return id.startsWith("claude-") ? id.slice(7) : id;
}

interface ModelBand {
  id: string;
  label: string;
  tint: string;
  series: number[];
  total: number;
}

/** Per-model daily layers over the last 30 days. Beyond three models the
 * rest fold into `other` — bands sum to the day's total either way, so the
 * legend's figures add to exactly the ledger's. */
function modelBands(usage: UsageResponse, dayKeys: string[]): ModelBand[] {
  const byModel = new Map<string, Map<string, number>>();
  const want = new Set(dayKeys);
  for (const row of usage.models) {
    if (!want.has(row.day)) continue;
    let days = byModel.get(row.model);
    if (!days) {
      days = new Map();
      byModel.set(row.model, days);
    }
    days.set(row.day, (days.get(row.day) ?? 0) + row.tokens);
  }
  const all = [...byModel.entries()]
    .map(([id, days]) => {
      const series = dayKeys.map((k) => days.get(k) ?? 0);
      return { id, series, total: series.reduce((t, v) => t + v, 0) };
    })
    .filter((band) => band.total > 0)
    .sort((a, b) => b.total - a.total);
  const top = all.slice(0, 3).map((band) => ({
    ...band,
    label: modelLabel(band.id),
    tint: modelTint(band.id),
  }));
  const rest = all.slice(3);
  if (rest.length > 0) {
    const series = dayKeys.map((_, j) => rest.reduce((t, band) => t + band.series[j], 0));
    top.push({
      id: "other",
      label: `other ×${rest.length}`,
      tint: "var(--vf-text-faint)",
      series,
      total: series.reduce((t, v) => t + v, 0),
    });
  }
  return top;
}

function ModelsTab({ bands, nowMs }: { bands: ModelBand[]; nowMs: number }) {
  const W = 276;
  const H = 60;
  const paths = streamPaths(bands.map((b) => b.series), W, H);
  const dates = [0, 10, 20, 29].map((k) => {
    const d = new Date(nowMs - (29 - k) * DAY_MS);
    return `${MONTHS[d.getMonth()]} ${d.getDate()}`;
  });
  return (
    <>
      <div className="ah-chart__head">
        <p className="ah-chart__title">Tokens per day · 30 days</p>
        {/* a chart may trade a quantity away, but it may not keep the axis
            that implies it still has it */}
        <span className="ah-chart__note">no zero · thickness is the total</span>
      </div>
      <div className="ah-chart">
        {paths.length === 0 ? (
          <p className="ah-chart__note">no model-attributed usage in this window</p>
        ) : (
          <svg
            viewBox={`0 0 ${W} ${H}`}
            preserveAspectRatio="none"
            role="img"
            aria-label="tokens per day by model, 30 days, wiggle-minimised stream"
          >
            {paths.map((d, i) => (
              <path key={bands[i].id} d={d} fill={bands[i].tint} opacity="0.72" />
            ))}
          </svg>
        )}
      </div>
      <div className="ah-dates">
        {dates.map((d) => (
          <span key={d}>{d}</span>
        ))}
      </div>
      <div className="ah-legend">
        {bands.map((band) => (
          <span key={band.id} style={{ "--m": band.tint } as CSSProperties}>
            <i />
            {band.label} <b>{tokenText(Math.round(band.total))}</b>
          </span>
        ))}
      </div>
    </>
  );
}

/* ── the project meta block (column ②'s header) ─────────────────────────── */

function hourLabel(hour: number): string {
  if (hour === 0) return "12 AM";
  if (hour < 12) return `${hour} AM`;
  if (hour === 12) return "12 PM";
  return `${hour - 12} PM`;
}

export function ProjectMetaBlock({
  vm,
  sessionsInList,
  usage,
  gridStartMs,
  nowMs,
}: {
  vm: ProjectVM;
  sessionsInList: number;
  usage: UsageResponse | null;
  gridStartMs: number;
  nowMs: number;
}) {
  const [open, setOpen] = useState(true);
  const [tab, setTab] = useState<"overview" | "models">(() =>
    new URLSearchParams(window.location.search).get("tab") === "models" ? "models" : "overview",
  );
  const st = heatStats(vm.yearCells, vm.todayIndex);
  const dayKeys = usage
    ? Array.from({ length: 30 }, (_, i) => dayKey(nowMs - (29 - i) * DAY_MS))
    : [];
  const bands = usage ? modelBands(usage, dayKeys) : [];
  const peakHour =
    usage && usage.hours.length > 0
      ? usage.hours.reduce((best, h) => (h.tokens > best.tokens ? h : best)).hour
      : null;
  const mostly = bands.length > 0 && bands[0].id !== "other" ? bands[0].label : null;
  const repo = vm.row.git_repo;

  return (
    <div className="ah-meta" style={{ "--tint": vm.tint } as CSSProperties}>
      <div className="ah-meta__head">
        <span className="ah-colhead">
          <i />
          <strong title={vm.row.name}>{vm.row.name}</strong>
          <span className="ah-bubble" title={`${sessionsInList} sessions in this list`}>
            {sessionsInList}
          </span>
        </span>
        <span className="ah-meta__acts">
          <button
            type="button"
            className="ah-iconbtn"
            aria-pressed={open}
            title={open ? "Hide the figures" : "Show the figures"}
            aria-label="Toggle the figures"
            onClick={() => setOpen((v) => !v)}
          >
            <StatsGlyph />
          </button>
          {/* absent, not disabled: a project without a known remote has no
              repo we can claim to open */}
          {repo !== null && (
            <button
              type="button"
              className="ah-iconbtn"
              title={`Open ${repo}`}
              aria-label="Open the git repository"
              onClick={() => window.open(repo.includes("://") ? repo : `https://${repo}`)}
            >
              <GitGlyph />
            </button>
          )}
        </span>
      </div>
      {open && (
        <div className="ah-meta__body">
          <StatChips
            chips={[
              { value: <b>{vm.sessionCount}</b>, label: "sessions" },
              { value: <b>{vm.messagesLife.toLocaleString()}</b>, label: "messages" },
              { value: <TokenChipValue n={vm.yearTokens} />, label: "tokens" },
              { value: <b>{st.active}</b>, label: "active days" },
            ]}
          />
          <div className="ah-chartcard">
            <div className="ah-seg ah-tabseg" role="tablist">
              <button
                type="button"
                role="tab"
                className={tab === "overview" ? "is-on" : ""}
                aria-selected={tab === "overview"}
                onClick={() => setTab("overview")}
              >
                Overview
              </button>
              <button
                type="button"
                role="tab"
                className={tab === "models" ? "is-on" : ""}
                aria-selected={tab === "models"}
                onClick={() => setTab("models")}
              >
                Models
              </button>
            </div>
            <div className="ah-tabbody">
              {tab === "models" ? (
                <ModelsTab bands={bands} nowMs={nowMs} />
              ) : (
                <>
                  <div>
                    <MonthRow gridStartMs={gridStartMs} />
                    <Ribbon vm={vm} />
                  </div>
                  <div className="ah-heatfoot">
                    <span>
                      {st.active} active days · 53 weeks
                    </span>
                    <span className="ah-heat__scale">
                      less
                      {[0, 1, 2, 3, 4].map((l) => (
                        <i key={l} data-l={l} style={{ "--l": l } as CSSProperties} />
                      ))}
                      more
                    </span>
                  </div>
                  <p className="ah-meta__facts">
                    <b>{st.current}d</b> streak · <b>{st.longest}d</b> best
                    {peakHour !== null && (
                      <>
                        {" "}
                        · peak <b>{hourLabel(peakHour)}</b>
                      </>
                    )}
                    {mostly !== null && (
                      <>
                        {" "}
                        · mostly <b>{mostly}</b>
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

/* ── the portfolio block (column ①'s header, the same block one level up) ── */

export function PortfolioBlock({
  vms,
  activeId,
  onSelect,
}: {
  vms: ProjectVM[];
  activeId: string | null;
  onSelect: (id: string) => void;
}) {
  const [open, setOpen] = useState(true);
  const sessions = vms.reduce((t, vm) => t + vm.sessionCount, 0);
  const messages = vms.reduce((t, vm) => t + vm.messagesLife, 0);
  const tokens = vms.reduce((t, vm) => t + vm.yearTokens, 0);
  // A day the portfolio was active is a day ANY project was, which is not
  // the sum of the per-project counts and must not be drawn as one.
  const anyActive = new Set<number>();
  for (const vm of vms) {
    vm.yearCells.forEach((v, i) => {
      if (v > 0) anyActive.add(i);
    });
  }
  const bySpend = [...vms].sort((a, b) => b.tokens30 - a.tokens30);
  const { tiles, floored, unlabelled } = treemapTiles(bySpend, (vm) => vm.tokens30);
  const caveats: string[] = [];
  if (unlabelled > 0) caveats.push(`${unlabelled} unlabelled`);
  if (floored > 0) caveats.push(`${floored} floored`);

  return (
    <div className="ah-meta">
      <div className="ah-meta__head">
        <span className="ah-colhead">
          {/* the marker keeps the component's shape and spends no hue: this
              head names a set, and hue in this room names one project */}
          <i style={{ "--tint": "var(--vf-text-tertiary)" } as CSSProperties} />
          <strong>Projects</strong>
          <span className="ah-bubble" title={`${vms.length} projects in this list`}>
            {vms.length}
          </span>
        </span>
        <span className="ah-meta__acts">
          <button
            type="button"
            className="ah-iconbtn"
            aria-pressed={open}
            title={open ? "Hide the figures" : "Show the figures"}
            aria-label="Toggle the figures"
            onClick={() => setOpen((v) => !v)}
          >
            <StatsGlyph />
          </button>
        </span>
      </div>
      {open && (
        <div className="ah-meta__body">
          <StatChips
            chips={[
              { value: <b>{sessions}</b>, label: "sessions" },
              { value: <b>{messages.toLocaleString()}</b>, label: "messages" },
              { value: <TokenChipValue n={tokens} />, label: "tokens" },
              { value: <b>{anyActive.size}</b>, label: "active days" },
            ]}
          />
          <div className="ah-chartcard">
            <div className="ah-chart__head">
              <span className="ah-chart__title">Spend · 30 days</span>
              <span className="ah-chart__note">area is the share</span>
            </div>
            <div className="ah-treemap" role="group" aria-label="Spend by project, area is the share">
              {tiles.map(({ item: vm, box, share, floored: isFloored, labelled }) => {
                const t = fmtTokens(vm.tokens30);
                return (
                  <button
                    key={vm.row.id}
                    type="button"
                    className={`ah-tile${vm.row.id === activeId ? " is-on" : ""}`}
                    style={
                      {
                        "--tint": vm.tint,
                        left: `${((box.x / TREE_W) * 100).toFixed(3)}%`,
                        top: `${((box.y / TREE_H) * 100).toFixed(3)}%`,
                        width: `${((box.w / TREE_W) * 100).toFixed(3)}%`,
                        height: `${((box.h / TREE_H) * 100).toFixed(3)}%`,
                      } as CSSProperties
                    }
                    title={`${vm.row.name} · ${t.n}${t.u} over 30 days · ${(share * 100).toFixed(1)}% of all spend${
                      isFloored ? " · drawn at the floor, not to scale" : ""
                    }`}
                    onClick={() => onSelect(vm.row.id)}
                  >
                    <span>
                      {labelled && (
                        <>
                          <b>{vm.row.name}</b>
                          <em>
                            {t.n}
                            {t.u}
                          </em>
                        </>
                      )}
                    </span>
                  </button>
                );
              })}
            </div>
            <div className="ah-heatfoot">
              <span>{bySpend.length} projects</span>
              <span>{caveats.length > 0 ? caveats.join(" · ") : "every tile true to scale"}</span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
