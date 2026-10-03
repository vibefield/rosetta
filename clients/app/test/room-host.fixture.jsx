// The whole room, mounted the way a host mounts it: HistoryRoom over ports,
// with no daemon and no desktop bridge. The driver picks a scenario through
// `window.roomFixture.mount(name)` and reads what the room asked of its ports.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import { flushSync } from "react-dom";
import { HistoryRoom } from "../renderer/src/HistoryRoom";
import { entry, inline, read, shell, tool, url } from "./support/ledger-fixtures.mjs";
import "../renderer/src/styles/tokens.css";
import "../renderer/src/styles/room.css";
import "../renderer/src/styles/ingest.css";
import "../renderer/src/styles/transcript.css";
import "../renderer/src/styles/search.css";

const H = 3_600_000;
const now = Date.now();
const never = () => new Promise(() => {});
const runtimes = [
  { id: "claude", kind: "claude-code", data_root: "~/.claude", enabled: true, account: null, config: null, skills: null, versions_seen: null, discovered_at: now - 900 * H },
  { id: "codex", kind: "codex", data_root: "~/.codex", enabled: true, account: null, config: null, skills: null, versions_seen: null, discovered_at: now - 900 * H },
];
const projects = [
  { id: "p0", name: "vibe-field", root_paths: ["/Users/demo/Projects/vibe-field"], git_repo: null },
  { id: "p1", name: "rosetta", root_paths: ["/Users/demo/Projects/rosetta"], git_repo: null },
];
const session = (id, project, title, hoursAgo, runtime = "claude") => ({
  id, native_session_id: `native-${id}`, runtime_id: runtime, project_id: project,
  cwd: `/Users/demo/Projects/${project === "p0" ? "vibe-field" : "rosetta"}`, parent_session_id: null, delegation: null,
  root_session_id: id, role: "main", title, summary: null, started_at: now - hoursAgo * H - 50 * 60_000,
  updated_at: now - hoursAgo * H, activity: { first_at: now - hoursAgo * H - 50 * 60_000, last_at: now - hoursAgo * H },
  stream_state: "indexed",
});
const sessions = [
  session("s0", "p0", "One frame for four views", 0.4),
  session("s1", "p0", "Godview's deck as a sheet", 2.2, "codex"),
  session("s2", "p1", "The room as a package", 5.5),
];
const digest = (seed) => seed.repeat(64).slice(0, 64);
const asset = (seed, variant = "") => ({ url: `/api/assets/${digest(seed + variant)}.webp`, digest: digest(seed + variant), media_type: "image/webp", byte_len: 2048 });
const image = (id, seed) => ({
  id, kind: "image", label: `[Image #${id}]`, name: `${id}.png`, media_type: "image/webp", byte_len: 2048,
  dimensions: { width: 640, height: 400 }, message_ranges: [],
  source: { type: "asset", asset: { ...asset(seed), image_variants: { rail: asset(seed, "1"), tile: asset(seed, "2"), tile2x: asset(seed, "3"), preview: asset(seed, "4") } } },
});
let ordinal = 0;
const at = () => now - 30 * 60_000 + ordinal * 40_000;
const row = (body, group = "agent_cycle") => { ordinal += 1; return { ...entry(body, { timeline_id: `t${ordinal}`, ordinal, at: at(), group: { id: `g${ordinal}`, kind: group, derivation: "native" } }), session_id: "s0" }; };
const toolRow = (made) => { ordinal += 1; return { ...made, timeline_id: `t${ordinal}`, ordinal, at: at(), session_id: "s0" }; };
const timeline = {
  s0: [
    row({ type: "user", text: inline("Port the History room into a package the frame can mount."), attachments: [image("a", "a"), image("b", "b")] }, "human_turn"),
    row({ type: "agent", text: inline("I'll read the room's seams first, then cut the ports."), attachments: [], citations: [] }),
    toolRow(read("clients/app/renderer/src/App.tsx")),
    toolRow(tool({ type: "web_search", query: "react context ports pattern", allowed_domains: [], blocked_domains: [], outcome: { sources: [
      { url: "https://github.com/facebook/react", title: "facebook/react" },
      { url: "https://developer.mozilla.org/en-US/docs/Web/API", title: "Web APIs | MDN" },
    ], total_count: 2 } })),
    toolRow(shell("pnpm test")),
    row({ type: "agent", text: inline("The room reads four ports now."), attachments: [], citations: [] }),
  ],
};
const heat = (scale) => Array.from({ length: 30 }, (_, i) => {
  const day = new Date(now - (29 - i) * 24 * H);
  const key = `${day.getFullYear()}-${String(day.getMonth() + 1).padStart(2, "0")}-${String(day.getDate()).padStart(2, "0")}`;
  return { day: key, tokens: (i % 3 === 0 ? 0 : 1_000_000 * scale), cost_micro_usd: 1000, billed_micro_usd: null, unpriced_tokens: 0, unrecorded_output: 0, reported_cost_adjustment_micro_usd: 0 };
});
const contribution = (project) => sessions.filter((s) => s.project_id === project).map((s, i) => ({
  session_id: s.id, project_id: project, tokens: 4_000_000 * (i + 1), commits: 0, edits: 0, loc: 0, messages: 40 * (i + 1),
  cost_micro_usd: 2_000_000, billed_micro_usd: null, unpriced_tokens: 0, unrecorded_output: 0, reported_cost_adjustment_micro_usd: 0,
}));
const status = (phase = "live") => ({
  protocol_version: 26, schema_version: 13, canonical_ready: phase === "live",
  search: { readiness: "ready", progress: null, projection_epoch: null, revision: null },
  daemon: { started_at: now - H, uptime_ms: H, runtimes: [], hub: { subscribers: 0, rings: 0, session_mutations: 0, meta_events: 0, resyncs: 0, rings_evicted: 0, overflow_rewarms: 0 },
    ingest: { phase, runtimes: [], projects: [] } },
});

export const log = { opened: [], closed: [], images: [], attachments: [], favicons: [], boosts: [] };

function sessionClient() {
  return {
    subscribeSession(id, handlers) {
      const rows = timeline[id] ?? [];
      queueMicrotask(() => {
        handlers.onStatus?.("live");
        handlers.onSnapshot?.({ entries: rows, cursor: null, watermark: "1" });
        handlers.onChange?.(rows);
        handlers.onTimelineMetaChange?.({ tasks: [], workflows: [], plans: [], artifacts: [] });
      });
      return { entries: () => rows, fetchPage: never, reveal() {}, loadEntry: async () => false, close() {} };
    },
    subscribeCollaboration: () => ({ close() {}, restart() {} }),
    timelineEntry: never, reportWork: never, command: never, readFileTarget: never, readPlan: never,
    readTextAsset: never, readAsset: never,
  };
}

const icon = `data:image/svg+xml,${encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16" rx="3" fill="#4d79c7"/></svg>')}`;
/** A desktop without a worktree engine: home, platform, browser, icons. */
const desktop = {
  homeDir: "/Users/demo",
  platform: "darwin",
  openInBrowser: async () => true,
  favicon: async (site) => { log.favicons.push(site); return icon; },
};

function ports(scenario) {
  const connected = scenario !== "disconnected";
  return {
    ...(scenario === "host" ? { host: desktop } : {}),
    catalog: {
      subscribeMeta(handlers) {
        queueMicrotask(() => {
          if (!connected) { handlers.onStatus?.("closed"); return; }
          handlers.onStatus?.("live");
          handlers.onChange?.(scenario === "consent" || scenario === "consent-default"
            ? { runtimes: [], projects: [], sessions: [] }
            : { runtimes, projects, sessions }, "snapshot");
        });
        return { close() {} };
      },
      status: () => connected
        ? Promise.resolve(status(scenario.startsWith("consent") ? "awaiting_consent" : "live"))
        : never(),
      heatmap: async () => ({ days: heat(1) }),
      contribution: async (project) => ({ rows: contribution(project) }),
      usage: async () => ({ models: [], hours: [] }),
      dayUsage: async (_project, day) => ({ day, models: [] }),
      search: async () => ({ state: status().search, hits: [] }),
      boost: async (id) => { log.boosts.push(id); return {}; },
    },
    sessions: {
      open(id) { log.opened.push(id); return sessionClient(); },
      close(id) { log.closed.push(id); },
    },
    assets: {
      imageAssetUrl(asset, variant) {
        log.images.push(`${asset.digest.slice(0, 4)}:${variant}`);
        return `data:image/svg+xml,${encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 3"><rect width="4" height="3" fill="#6366f1"/></svg>')}`;
      },
      attachmentAssetUrl(asset) { log.attachments.push(asset.digest.slice(0, 4)); return asset.url; },
    },
  };
}

function Fixture() {
  const [scenario, setScenario] = useState(null);
  const [sheetOpen, setSheetOpen] = useState(false);
  window.roomFixture = {
    log,
    mount(name) { flushSync(() => setScenario({ name, key: Math.random(), ports: ports(name) })); },
  };
  if (scenario === null) return null;
  const { name, key, ports: p } = scenario;
  return <HistoryRoom key={key} {...p}
    {...(name === "search" ? { search: { open: sheetOpen, onOpenChange: setSheetOpen, accessory: <span data-accessory="">host</span> } } : {})}
    {...(name === "disconnected" ? { unavailable: <span data-host-words="">The index is resting.</span> } : {})}
    {...(name === "consent" ? { consent: <div data-consent="">Choose your agents</div> } : {})}
  />;
}

createRoot(document.getElementById("root")).render(<Fixture />);
