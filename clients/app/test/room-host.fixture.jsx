// The whole room, mounted the way a host mounts it: HistoryRoom over ports,
// with no daemon and no desktop bridge. The driver picks a scenario through
// `window.roomFixture.mount(name)` and reads what the room asked of its ports.
import { useRef, useState } from "react";
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
const s2Row = (id, text, type = "agent") => ({ ...entry(type === "user"
  ? { type, text: inline(text), attachments: [] }
  : { type, text: inline(text), attachments: [], citations: [] }, { timeline_id: id, ordinal: Number(id.slice(1)), at: now - 5 * H + Number(id.slice(1)) * 60_000 }), session_id: "s2" });
const timeline = {
  s2: [s2Row("u1", "Cut the ports first.", "user"), s2Row("u2", "The catalog reads the room's own shapes."), s2Row("u3", "Done.")],
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
  canonicalReady: phase === "live", searchReadiness: "ready", searchProgress: null,
  ingest: { phase, runtimes: [], projects: [] }, startedAt: now - H, uptimeMs: H,
});

export const log = { opened: [], closed: [], images: [], attachments: [], favicons: [], boosts: [], selections: [] };

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

/** A desktop that also runs a worktree engine, which never reports. */
const engineDesktop = {
  ...desktop,
  worktree: {
    open: async () => ({ ok: true, generation: 1 }),
    gitFacts: async () => null,
    fileAction: async () => ({ ok: true }),
    ports: () => () => {},
  },
};

function ports(scenario) {
  const connected = scenario !== "disconnected";
  const engine = scenario === "narrow-host" || scenario === "wide-host";
  return {
    ...(scenario === "host" ? { host: desktop } : engine ? { host: engineDesktop } : {}),
    catalog: {
      subscribeMeta(handlers) {
        const deliver = () => {
          if (!connected) { handlers.onStatus?.("closed"); return; }
          handlers.onStatus?.("live");
          handlers.onChange?.(scenario === "consent" || scenario === "consent-default"
            ? { runtimes: [], projects: [], sessions: [] }
            : { runtimes, projects, sessions }, "snapshot");
        };
        // "late": the index delivers its rows a beat after the room mounts.
        if (scenario === "late") setTimeout(deliver, 250); else queueMicrotask(deliver);
        return { close() {} };
      },
      status: () => connected
        ? Promise.resolve(status(scenario.startsWith("consent") ? "awaiting_consent" : "live"))
        : never(),
      projectStats: async (project) => ({ heat: heat(1), contribution: contribution(project) }),
      usage: async () => ({ models: [], hours: [] }),
      dayUsage: async (_project, day) => ({ day, models: [] }),
      search: async () => ({ state: { readiness: "ready", progress: null, projection_epoch: null, revision: null },
        hits: scenario === "prefixed"
          ? [{ timeline_id: "t2", session_id: "s0", kind: "agent", at: now - 20 * 60_000, snippet: "I'll read the [room]'s seams first", score: 1 }]
          : [] }),
      boost: async (id) => { log.boosts.push(id); return {}; },
    },
    sessions: {
      open(id) { log.opened.push(id); return sessionClient(); },
      close(id) { log.closed.push(id); },
    },
    assets: {
      imageAssetUrl(asset, variant) {
        const name = `${asset.digest.slice(0, 4)}:${variant}`;
        log.images.push(name);
        const svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 3"><rect width="4" height="3" fill="#6366f1"/></svg>';
        if (scenario !== "fetching") return `data:image/svg+xml,${encodeURIComponent(svg)}`;
        // A credentialed host: bytes first, then an object URL. The first ask
        // for image b is refused, as a host whose fetch failed would answer.
        const refused = asset.digest.startsWith("bbbb") && log.images.filter((n) => n === name).length === 1;
        return new Promise((resolve) => setTimeout(() => resolve(refused ? null
          : URL.createObjectURL(new Blob([svg], { type: "image/svg+xml" }))), 120));
      },
      attachmentAssetUrl(asset) { log.attachments.push(asset.digest.slice(0, 4)); return asset.url; },
    },
  };
}

/** A host's own store, recording every name the room writes into it. */
const recorded = new Map();
const recordingStore = { getItem: (key) => recorded.get(key) ?? null, setItem: (key, value) => { recorded.set(key, value); } };

function Fixture() {
  const [scenario, setScenario] = useState(null);
  const [sheetOpen, setSheetOpen] = useState(false);
  const room = useRef(null);
  window.roomFixture = {
    log,
    mount(name) {
      for (const list of Object.values(log)) list.length = 0;
      flushSync(() => setScenario({ name, key: Math.random(), ports: ports(name) }));
    },
    openAt(target) { room.current.openAt(target); },
    stored: () => [...recorded.keys()].sort(),
  };
  if (scenario === null) return null;
  const { name, key, ports: p } = scenario;
  // What a host paints beneath a room that asked it to: here, a loud ground,
  // so a capture shows whether anything of the room's paper is left.
  document.body.style.background = name === "glass"
    ? "repeating-linear-gradient(45deg, #2b4a8a 0 40px, #8a2b5e 40px 80px)" : "";
  // A host may seat the room in less than its window.
  const width = name === "narrow-host" ? 1200 : name === "wide-host" ? 1400 : null;
  return <div style={{ height: "100%", ...(width === null ? {} : { width }) }}><HistoryRoom key={key} ref={room} {...p}
    onSelect={(selection) => log.selections.push(selection)}
    {...(name === "sized" ? { edgeInset: { top: 12, right: 8, bottom: 8, left: 8 }, scrollFoot: "48px", floatingFoot: "40px" } : {})}
    {...(name === "search" || name === "prefixed" ? { search: { open: sheetOpen, onOpenChange: setSheetOpen, accessory: <span data-accessory="">host</span> } } : {})}
    {...(name === "prefixed" ? { storagePrefix: "vf.history.", storage: recordingStore } : {})}
    {...(name === "disconnected" ? { unavailable: <span data-host-words="">The index is resting.</span> } : {})}
    {...(name === "consent" ? { consent: <div data-consent="">Choose your agents</div> } : {})}
    {...(name === "glass" ? { surface: "host", portalContainer: document.getElementById("host-layer") } : {})}
  /></div>;
}

createRoot(document.getElementById("root")).render(<Fixture />);
