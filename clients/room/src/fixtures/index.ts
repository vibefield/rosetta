// A demo set for the History room: one desktop's projects, sessions, figures
// and two transcripts, with ports that answer them — so any host (a design
// catalog, a harness, a browser tab) can render the room with no daemon and
// no desktop. Built relative to `now`, so its times never go stale. Ported
// from VibeField's One Frame capture (draft/one-frame/tools/hx/room.jsx),
// which drew the columns from hand-made view models; here the room derives
// them itself, activity order included, from rows and figures.
import type {
  AssetRef,
  AttachmentRef,
  ContributionRow,
  DayUsageResponse,
  GroupKind,
  HeatmapDay,
  HourTotal,
  MetaState,
  ModelDay,
  ProjectRow,
  RuntimeRow,
  SearchHit,
  SearchResponse,
  SessionRow,
  TimelineBody,
  TimelineEntry,
  TimelineMeta,
  ToolAction,
  ToolStatus,
  UsageResponse,
} from "@vibefield/rosetta-protocol";
import type { AssetSource, CatalogPort, CatalogStatus, RoomPorts, SessionClient, SessionFeed, SessionPort } from "../ports";
import { advanceActivityOrder } from "../lib/activity-order";
import type { ActivityOrder } from "../lib/activity-order";
import { buildProjectViewModel, getActivityGridStartTimestamp, getCalendarDayKey } from "../lib/model";
import type { ProjectStats, ProjectViewModel } from "../lib/model";
import { createTintDealer } from "../lib/tint";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** Everything the demo room shows. */
export interface RoomDemo {
  readonly now: number;
  readonly meta: MetaState;
  readonly status: CatalogStatus;
  readonly stats: ReadonlyMap<string, ProjectStats>;
  readonly usage: ReadonlyMap<string, UsageResponse>;
  readonly timelines: ReadonlyMap<string, readonly TimelineEntry[]>;
}

/* ── the rows ───────────────────────────────────────────────────────────── */

const RUNTIMES: readonly (readonly [string, string])[] = [["claude", "claude-code"], ["codex", "codex"]];

/** [name, lifetime cost in USD, a scale for its daily tokens]. */
const PROJECTS: readonly (readonly [string, number, number])[] = [
  ["vibe-field", 2840, 1_700_000],
  ["rosetta", 1120, 850_000],
  ["puppeteer", 760, 566_000],
  ["infinite-canvas-engine", 690, 425_000],
  ["ghosttea", 310, 340_000],
  ["truffle", 120, 283_000],
];

/** [project, title, hours ago, runtime, tokens, messages, cost in USD]. */
const SESSIONS: readonly (readonly [number, string, number, string, number, number, number])[] = [
  [0, "One frame for four views", 0.4, "claude", 8.2e6, 184, 21.4],
  [0, "Godview's deck as a sheet", 2.2, "claude", 21e6, 512, 58.9],
  [0, "DS landing fix-ups", 5.5, "codex", 4.1e6, 96, 9.8],
  [0, "Spawn bloom review", 8.1, "claude", 13e6, 330, 33.1],
  [0, "Chrome on the mat", 25, "claude", 18e6, 421, 47.5],
  [0, "Edge tray notch and drawer", 28, "claude", 9.6e6, 207, 24.2],
  [0, "Mesh board reader backoff", 31, "codex", 3.3e6, 75, 7.9],
  [0, "Session glyphs, pass two", 34, "claude", 6.1e6, 140, 15.6],
  [1, "The room as a package", 1.5, "claude", 5.2e6, 120, 13.0],
  [1, "Grants on the direct plane", 50, "codex", 2.8e6, 64, 6.1],
  [2, "Views behind injected ports", 72, "claude", 4.4e6, 98, 11.2],
];

const PROVENANCE = { agent_version: null, src_cursor: null, src_offset: null } as const;
const NO_USAGE = {
  input: null, output: null, cache_read: null, cache_write: null, thinking: null, cache_write_1h: null,
  web_searches: null, service_tier: null, route: null, billed_pico_usd: null, scope: "request", requests: null,
} as const;

/** A 64-hex digest, the same for the same seed (FNV-1a over the whole seed,
 * eight rounds). Not a content hash: the demo's members have no bytes. */
function digestOf(seed: string): string {
  let hex = "";
  for (let round = 0; hex.length < 64; round += 1) {
    let hash = (0x811c9dc5 ^ round) >>> 0;
    for (let i = 0; i < seed.length; i += 1) {
      hash ^= seed.charCodeAt(i);
      hash = Math.imul(hash, 0x01000193) >>> 0;
    }
    hex += hash.toString(16).padStart(8, "0");
  }
  return hex.slice(0, 64);
}

function member(seed: string): AssetRef {
  const digest = digestOf(seed);
  return { url: `/api/assets/${digest}.webp`, digest, media_type: "image/webp", byte_len: 2048 };
}

/** An optimized image the room will draw: a member and its four renditions. */
function image(id: string, seed: string, name: string, width: number, height: number): AttachmentRef {
  const full = member(seed);
  return {
    id, kind: "image", label: `[Image #${id}]`, name, media_type: "image/webp", byte_len: full.byte_len,
    source: { type: "asset", asset: { ...full, image_variants: {
      rail: member(`${seed}:rail`), tile: member(`${seed}:tile`), tile2x: member(`${seed}:tile2x`), preview: member(`${seed}:preview`),
    } } },
    dimensions: { width, height }, message_ranges: [],
  };
}

function transcript(sessionId: string, startedAt: number) {
  const rows: TimelineEntry[] = [];
  const add = (body: TimelineBody, kind: GroupKind, model: string | null = null): void => {
    const ordinal = rows.length + 1;
    rows.push({
      timeline_id: `${sessionId}-t${ordinal}`, session_id: sessionId, ordinal,
      group: { id: `${sessionId}-g${ordinal}`, kind, derivation: "native" },
      response: null, block_index: null, native_id: null, parent_native_id: null,
      at: startedAt + ordinal * 40_000, model, effort: null, body, parent_tool_native_id: null,
      usage: NO_USAGE, provenance: PROVENANCE,
    });
  };
  return {
    rows,
    user: (text: string, attachments: AttachmentRef[] = []) =>
      add({ type: "user", text: { type: "inline", text }, attachments }, "human_turn"),
    agent: (text: string) =>
      add({ type: "agent", text: { type: "inline", text }, attachments: [], citations: [] }, "agent_cycle", "claude-opus-5-5"),
    tool: (name: string, action: ToolAction, status: ToolStatus = "succeeded") =>
      add({ type: "tool_use", tool: {
        correlation: { key: `${sessionId}-k${rows.length + 1}`, derivation: "native_id" },
        native: { id: null, namespace: null, name, provider: null },
        status: { value: status, basis: "native_state" },
        action, attachments: [], failure: null, timing: null,
      } }, "agent_cycle"),
  };
}

const read = (path: string): ToolAction => ({
  type: "file_read", targets: [{ resource: { type: "file", path, resolved: true }, range: null, media_kind: null }],
  outcome: { truncated: false },
});
const shell = (command: string, finished = true): ToolAction => ({
  type: "shell", command: { type: "inline", text: command }, source_truncated: false, cwd: null, timeout_ms: null,
  background: false, outcome: finished ? { exit_code: 0, signal: null, process_id: null } : null,
});

/* ── the demo ───────────────────────────────────────────────────────────── */

export function createRoomDemo(now: number = Date.now()): RoomDemo {
  const runtimes: RuntimeRow[] = RUNTIMES.map(([id, kind]) => ({
    id, kind, data_root: `~/.${id}`, enabled: true, account: null, config: null, skills: null,
    versions_seen: null, discovered_at: now - 400 * DAY,
  }));
  const projects: ProjectRow[] = PROJECTS.map(([name], i) => ({
    id: `p${i}`, name, root_paths: [`/Users/demo/Projects/${name}`], git_repo: i < 4 ? `github.com/demo/${name}` : null,
  }));
  const sessions: SessionRow[] = SESSIONS.map(([project, title, hoursAgo, runtime], i) => {
    const last = now - hoursAgo * HOUR;
    return {
      id: `s${i}`, native_session_id: `native-${runtime}-${i}`, runtime_id: runtime, project_id: `p${project}`,
      cwd: `/Users/demo/Projects/${PROJECTS[project]![0]}`, parent_session_id: null, delegation: null,
      root_session_id: `s${i}`, role: "main", title, summary: null, started_at: last - 50 * MINUTE,
      updated_at: last, activity: { first_at: last - 50 * MINUTE, last_at: last }, stream_state: "indexed",
    };
  });

  // The figures: a year of days, quiet long ago and busy lately, per project.
  const start = getActivityGridStartTimestamp(now);
  const stats = new Map<string, ProjectStats>();
  const usage = new Map<string, UsageResponse>();
  PROJECTS.forEach(([, usd, scale], i) => {
    const heat: HeatmapDay[] = [];
    for (let d = 0, at = start; at <= now; d += 1, at += DAY) {
      const seed = i * 3 + 1;
      const tokens = Math.round(at > now - 40 * DAY ? ((d * 7 + seed) % 5 === 0 ? 0 : (8 + ((d * 13 + seed) % 23)) * scale)
        : at > now - 120 * DAY ? ((d + seed) % 3 === 0 ? (2 + ((d * 5 + seed) % 9)) * scale * 0.6 : 0)
        : (d + seed) % 11 === 0 ? (1 + (d % 5)) * scale * 0.3 : 0);
      heat.push({ day: getCalendarDayKey(at + DAY / 2), tokens, cost_micro_usd: Math.round(tokens * 2.4),
        billed_micro_usd: null, unpriced_tokens: 0, unrecorded_output: 0, reported_cost_adjustment_micro_usd: 0 });
    }
    const contribution: ContributionRow[] = SESSIONS.flatMap(([project, , , , tokens, messages, cost], s) =>
      project !== i ? [] : [{ session_id: `s${s}`, project_id: `p${i}`, tokens, commits: 0, edits: 0, loc: 0, messages,
        cost_micro_usd: Math.round(cost * 1e6), billed_micro_usd: null, unpriced_tokens: 0, unrecorded_output: 0,
        reported_cost_adjustment_micro_usd: 0 }]);
    stats.set(`p${i}`, { heat, contribution });
    const spend = (tokens: number) => ({ cost_micro_usd: Math.round(tokens * 2.4), billed_micro_usd: null,
      unpriced_tokens: 0, unrecorded_output: 0, reported_cost_adjustment_micro_usd: 0 });
    const models: ModelDay[] = heat.filter((day) => day.tokens > 0).slice(-30).flatMap((day, k) => [
      { day: day.day, model: "claude-opus-5-5", tokens: Math.round(day.tokens * 0.7), ...spend(day.tokens * 0.7) },
      { day: day.day, model: k % 3 === 0 ? "gpt-5.4" : "claude-sonnet-4-5", tokens: Math.round(day.tokens * 0.3), ...spend(day.tokens * 0.3) },
    ]);
    const hours: HourTotal[] = [1, 10, 15, 16, 23].map((hour, k) => {
      const tokens = Math.round((k + 2) * 4e6 * (usd / 2840));
      return { hour, tokens, ...spend(tokens) };
    });
    usage.set(`p${i}`, { models, hours });
  });

  // Two transcripts: the conversation first, its activity one press away.
  const timelines = new Map<string, readonly TimelineEntry[]>();
  const one = transcript("s0", sessions[0]!.started_at);
  one.user("Port the latest Godview look and rosetta's History view into the four-views frame, and fix the old flaws while we're there: the keys in the file pill, and the two plus buttons in the composer line.",
    [image("1", "frame-before", "frame-before.png", 1440, 900), image("2", "frame-after", "frame-after.png", 900, 1200)]);
  one.agent("I'll read DESIGN.md and the composer record first, then capture Godview from the catalog and History from rosetta's own components, so the page shows each view as it really looks.");
  one.tool("Read", read("DESIGN.md"));
  one.tool("Read", read("draft/composer/compact.md"));
  one.tool("WebSearch", { type: "web_search", query: "react context injected ports", allowed_domains: [], blocked_domains: [], outcome: { sources: [
    { url: "https://github.com/facebook/react", title: "facebook/react" },
    { url: "https://developer.mozilla.org/en-US/docs/Web/API", title: "Web APIs | MDN" },
  ], total_count: 2 } });
  one.tool("Bash", shell("pnpm exec vite --mode design"));
  one.tool("Bash", shell("node capture.mjs --view godview --theme dark"));
  one.agent("The frame has five places: the head, the line, the two edges and the corner. Every view keeps its interior. When you switch views, only the head's contents and the stage change.");
  one.user("Show me the line with one plus.");
  one.agent("The attach button retires. @ puts anything into the message, agents and things alike, and the clipboard notch stays the store. The create act is the view's own, and only Canvas draws it as a plus.");
  one.tool("Bash", shell("node verify.mjs --states all", false), "running");
  timelines.set("s0", one.rows);
  const two = transcript("s8", sessions[8]!.started_at);
  two.user("Extract the room into a package that dials nothing.");
  two.agent("The room reads four ports now: the catalog, one session's body, the desktop and the asset source.");
  two.agent("rosetta's own app is the first host, over its one client.");
  timelines.set("s8", two.rows);

  const status: CatalogStatus = {
    canonicalReady: true, searchReadiness: "ready", searchProgress: null,
    ingest: { phase: "live", runtimes: [], projects: [] }, startedAt: now - 6 * HOUR, uptimeMs: 6 * HOUR,
  };
  return { now, meta: { runtimes, projects, sessions }, status, stats, usage, timelines };
}

/* ── its ports ──────────────────────────────────────────────────────────── */

const EMPTY_META: TimelineMeta = { tasks: [], workflows: [], plans: [], artifacts: [] };

function textOf(row: TimelineEntry): string {
  return (row.body.type === "user" || row.body.type === "agent") && row.body.text?.type === "inline" ? row.body.text.text : "";
}

/** A tile drawn from the digest, so each image has its own colour. */
function placeholderSvg(digest: string): string {
  const hue = Number.parseInt(digest.slice(0, 2), 16) * 1.4;
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 10"><rect width="16" height="10" fill="hsl(${hue.toFixed(0)} 55% 55%)"/><circle cx="11" cy="3.5" r="1.6" fill="hsl(${hue.toFixed(0)} 60% 85%)"/><path d="M0 10 6 4l4 4 2-2 4 4z" fill="hsl(${hue.toFixed(0)} 45% 35%)"/></svg>`;
}

/** The demo's addresses: every stored image is a drawn tile, inline. */
export function demoAssets(): AssetSource {
  return {
    imageAssetUrl: (asset) => `data:image/svg+xml,${encodeURIComponent(placeholderSvg(asset.digest))}`,
    attachmentAssetUrl: (asset) => asset.url,
  };
}

function demoSessionClient(demo: RoomDemo): SessionClient {
  const rowsOf = (sessionId: string) => demo.timelines.get(sessionId) ?? [];
  const never = () => Promise.reject(new Error("The demo keeps no recorded file"));
  return {
    subscribeSession(sessionId, handlers): SessionFeed {
      const rows = [...rowsOf(sessionId)];
      let open = true;
      queueMicrotask(() => {
        if (!open) return;
        handlers.onStatus?.("live");
        handlers.onSnapshot?.({ entries: rows, cursor: null, watermark: "1" });
        handlers.onChange?.(rows);
        handlers.onTimelineMetaChange?.(EMPTY_META);
      });
      return {
        entries: () => rows,
        fetchPage: async () => ({ entries: [], cursor: null, watermark: "1" }),
        reveal: () => {},
        loadEntry: async (timelineId) => rows.some((row) => row.timeline_id === timelineId),
        close: () => {
          open = false;
        },
      };
    },
    subscribeCollaboration: () => ({ close() {}, restart() {} }),
    timelineEntry: async (sessionId, timelineId) => {
      const row = rowsOf(sessionId).find((entry) => entry.timeline_id === timelineId);
      if (row === undefined) throw new Error("That message is not in the demo");
      return row;
    },
    reportWork: async () => [],
    command: async () => ({ command: "" }),
    readFileTarget: never,
    readPlan: never,
    readTextAsset: never,
    readAsset: async (asset) => new TextEncoder().encode(placeholderSvg(asset.digest)),
  };
}

/** The demo's catalog: rows on subscribe, figures by project, and a search
 * over the demo's own words. No boost: nothing here is still queued. */
function demoCatalog(demo: RoomDemo): CatalogPort {
  return {
    subscribeMeta(handlers) {
      let open = true;
      queueMicrotask(() => {
        if (!open) return;
        handlers.onStatus?.("live");
        handlers.onChange?.(demo.meta, "snapshot");
      });
      return {
        close: () => {
          open = false;
        },
      };
    },
    status: async () => demo.status,
    projectStats: async (projectId) => demo.stats.get(projectId) ?? { heat: [], contribution: [] },
    usage: async (projectId) => demo.usage.get(projectId) ?? { models: [], hours: [] },
    dayUsage: async (projectId, day): Promise<DayUsageResponse> => ({
      day,
      models: (demo.usage.get(projectId)?.models ?? []).filter((model) => model.day === day).flatMap((model) =>
        [9, 14, 16, 22].map((hour) => ({ hour, model: model.model, tokens: Math.round(model.tokens / 4) }))),
    }),
    search: async (request): Promise<SearchResponse> => {
      const q = request.q.trim().toLowerCase();
      const hits: SearchHit[] = [];
      for (const [sessionId, rows] of demo.timelines) {
        for (const row of rows) {
          const text = textOf(row);
          const at = text.toLowerCase().indexOf(q);
          if (q === "" || at < 0 || hits.length >= request.limit) continue;
          hits.push({ timeline_id: row.timeline_id, session_id: sessionId, kind: row.body.type, at: row.at,
            snippet: `${text.slice(Math.max(0, at - 40), at)}[${text.slice(at, at + q.length)}]${text.slice(at + q.length, at + q.length + 60)}`,
            score: 1 });
        }
      }
      return { state: { readiness: "ready", progress: null, projection_epoch: null, revision: null }, hits };
    },
  };
}

/** Ports over the demo: a catalog, sessions and asset addresses. No host —
 * a host that is a desktop adds its own `HostPort`. */
export function createDemoPorts(demo: RoomDemo = createRoomDemo()): RoomPorts {
  const client = demoSessionClient(demo);
  const sessions: SessionPort = { open: () => client, close: () => {} };
  return { catalog: demoCatalog(demo), sessions, assets: demoAssets() };
}

/** The demo's projects as the room's columns draw them, for a harness that
 * mounts a column on its own. */
export function demoProjectViewModels(demo: RoomDemo = createRoomDemo()): ProjectViewModel[] {
  const kinds = new Map(demo.meta.runtimes.map((runtime) => [runtime.id, runtime.kind]));
  const tints = createTintDealer(null);
  return demo.meta.projects.map((project) => buildProjectViewModel(project, demo.meta.sessions,
    demo.stats.get(project.id), (id) => kinds.get(id) ?? id, demo.now, tints.projectTint));
}

/** The order the projects and sessions column would settle on for the demo. */
export function demoActivityOrder(demo: RoomDemo = createRoomDemo()): ActivityOrder {
  return advanceActivityOrder(null, demo.meta.sessions, { now: demo.now, live: false, bootstrapping: false });
}
