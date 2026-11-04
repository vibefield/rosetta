// A cold import, simulated: the ports a host hands the room (`RoomPorts`)
// over a seeded synthetic archive (`cold-import-corpus.ts`), the same
// daemon's REST doors for a host that builds its ports over a RosettaClient
// (`cold-import-daemon.ts`), and a controller a test drives through simulated
// time. It is STEP-DRIVEN: nothing here owns a timer or reads a clock.
// `advanceTo(ms)` delivers, in order, every metadata change and session
// resync the daemon would have made by then (`cold-import-schedule.ts` says
// when, citing rosettad), and every ask answers with the state at the current
// simulated time. The room keeps its own timers (the status poll, the card's
// second hand, ALL SET's five seconds, the not-imported face's retry), so a
// test sees exactly what the room does with what the daemon says, and never
// measures time with it.
//
// What the daemon's surfaces say, and where:
// - /ws/meta: the mirror withholds a Pending session's activity and stops
//   every family rollup at a Pending member while the admission barrier holds
//   (crates/store/src/hot.rs:591-641; crates/store/src/activity.rs:61-75). A
//   delivery over the hub's 1,024-item queue trips one resync barrier and the
//   client re-reads a snapshot (crates/server/src/hub.rs:44, 627-655). A
//   fresh build lifts the barrier with `reveal_pending`, which re-reads the
//   still-Pending rows and their ancestors (hub.rs:295-307); a resume with
//   `rewarm`, one whole-surface diff that also carries every flip.
// - Session sockets: a Pending session's snapshot is authoritative and empty
//   (crates/server/src/http.rs:1030-1036); a resync barrier makes the client
//   reset and re-read it (clients/ts/client.ts:1005-1016, 1063-1067;
//   crates/server/src/ws.rs:437-452). Snapshots read the store, which a
//   resume's withheld commit reaches before the hub does.
// - /api/status: phases, runtime meters and the progress board
//   (crates/server/src/health.rs:208-366; crates/server/src/board.rs:63-140),
//   canonical readiness only once the read router is published (http.rs:821).
// - Before publication every cross-session aggregate answers `not_ready`
//   (http.rs:510-518) and search answers no hits while it builds
//   (http.rs:1198-1203); the detached index keeps answering none until its
//   first generation (crates/search/src/sidecar.rs:485-490). A boost is
//   recorded and does nothing: a fresh RAM build attaches no boost queue
//   (crates/rosettad/src/serve.rs:375-379).
import { ApiRequestError, PROTOCOL_VERSION } from "@vibefield/rosetta-protocol";
import type {
  ArtifactPage,
  BackfillState,
  BoostResponse,
  ContributionResponse,
  DiscoveriesResponse,
  EnableResponse,
  IngestPhase,
  IngestProgress,
  Ingestion,
  IngestionPhase,
  IngestionState,
  MetaState,
  MetaSubscriptionHandlers,
  ProjectBackfill,
  ProjectRow,
  RosettaClient,
  RuntimeBackfill,
  SearchBuildProgress,
  SearchHit,
  SearchResponse,
  SearchState,
  SessionActivity,
  SessionRow,
  SessionSubscriptionHandlers,
  StatusResponse,
  TimelineEntry,
  TimelineMeta,
} from "@vibefield/rosetta-protocol";
import type { AssetSource, CatalogPort, CatalogStatus, Closable, RoomPorts, SessionClient, SessionFeed, SessionPort } from "../ports";
import { catalogStatusOf } from "../lib/catalog-status";
import { UNASSIGNED_PROJECT_ID } from "../lib/model";
import { createColdCorpus } from "./cold-import-corpus";
import type { ColdCorpus, ColdCorpusOptions, ColdTree, SessionKind } from "./cold-import-corpus";
import { ColdWireError, aborted, fetchOver } from "./cold-import-daemon";
import type { ColdWire } from "./cold-import-daemon";
import { DISCOVERY_MS, SUBSCRIBER_QUEUE, planSchedule } from "./cold-import-schedule";
import type { ColdPhase, FlipBatch, Lane, RuntimeTimes, Schedule } from "./cold-import-schedule";

export type { ColdCorpus, ColdCorpusOptions, ColdTree, SessionKind } from "./cold-import-corpus";
export type { ColdPhase, FlipBatch, FlipKind, RuntimeTimes } from "./cold-import-schedule";

/** The store's schema version, which /api/status reports (crates/store/src/schema.rs:12). */
const SCHEMA_VERSION = 29;

/* ── synthetic transcripts ──────────────────────────────────────────────── */

const PROVENANCE = { agent_version: null, src_cursor: null, src_offset: null } as const;
const NO_USAGE = {
  input: null, output: null, cache_read: null, cache_write: null, thinking: null, cache_write_1h: null,
  web_searches: null, service_tier: null, route: null, billed_pico_usd: null, scope: "request", requests: null,
} as const;
const EMPTY_META: TimelineMeta = { tasks: [], workflows: [], plans: [], artifacts: [] };

/** A readable session's rows: a few turns inside its recorded bounds, the
 * title's words in the prompt so full text can find it after publication. */
function transcriptOf(row: SessionRow, activity: SessionActivity): TimelineEntry[] {
  const count = 3 + (Number.parseInt(row.id.slice(0, 2), 16) % 6);
  const words = row.title ?? "a delegated task";
  return Array.from({ length: count }, (_, i) => {
    const ordinal = i + 1;
    const user = i % 2 === 0;
    const text = user ? `Please ${words} and keep the change small.` : `Done: ${words}. The tests pass and the diff is short.`;
    return {
      timeline_id: `${row.id.slice(0, 16)}-t${ordinal}`, session_id: row.id, ordinal,
      group: { id: `${row.id.slice(0, 16)}-g${Math.floor(i / 2)}`, kind: user ? "human_turn" : "agent_cycle", derivation: "native" },
      response: null, block_index: null, native_id: null, parent_native_id: null,
      at: Math.round(activity.first_at + ((activity.last_at - activity.first_at) * i) / Math.max(1, count - 1)),
      model: user ? null : "claude-opus-5-5", effort: null,
      body: user
        ? { type: "user", text: { type: "inline", text }, attachments: [] }
        : { type: "agent", text: { type: "inline", text }, attachments: [], citations: [] },
      parent_tool_native_id: null, usage: NO_USAGE, provenance: PROVENANCE,
    };
  });
}

/* ── the simulator's surface ────────────────────────────────────────────── */

export interface ColdImportOptions extends ColdCorpusOptions {
  /** `fresh` (default): a first import. `resumed`: a cross-launch resume. */
  readonly course?: "fresh" | "resumed";
  /** `awaiting` (default for fresh): the app's roll call grants consent.
   * `granted`: consent was given before (a resume always has it). */
  readonly consent?: "awaiting" | "granted";
}

/** One entry of the trail: every status ask and its fate, the grant, every
 * metadata delivery and resync, and what a host marks (a connection change
 * reaching the room), in one order. */
export interface ColdTrailEntry {
  readonly seq: number;
  /** Simulated ms. */
  readonly at: number;
  readonly what: string;
  readonly detail: string | null;
}

/** What the room asked and heard, in order, with the simulated time. */
export interface ColdImportLog {
  readonly status: number[];
  readonly projectStats: { readonly projectId: string; readonly at: number }[];
  readonly usage: number[];
  readonly dayUsage: number[];
  readonly search: { readonly q: string; readonly at: number }[];
  readonly boosts: { readonly sessionId: string; readonly at: number }[];
  readonly opened: string[];
  readonly closed: string[];
  readonly subscribed: { readonly sessionId: string; readonly at: number }[];
  readonly enabled: { readonly kinds: readonly string[]; readonly at: number }[];
  /** Every metadata delivery: a snapshot (after a resync) or mutations. */
  readonly meta: { readonly seq: number; readonly at: number; readonly source: "snapshot" | "mutation"; readonly rows: number; readonly why: string }[];
  readonly resyncs: { readonly seq: number; readonly sessionId: string; readonly at: number; readonly delivered: boolean }[];
  readonly trail: ColdTrailEntry[];
}

/** How a barrier reaches an open transcript. `immediate`: its socket resyncs
 * at once (a flip's resync lands before its metadata,
 * crates/rosettad/src/cold_stream/flips.rs:9-11).
 * `manual`: held until `deliverResyncs()`. `drop`: the subscription raced
 * the barrier and hears nothing. */
export type ResyncMode = "immediate" | "manual" | "drop";

export interface ColdImportController {
  readonly corpus: ColdCorpus;
  /** Epoch ms of the import clock's zero (the daemon's start). */
  readonly now: number;
  /** Simulated ms since the daemon started. */
  readonly clock: number;
  readonly course: "fresh" | "resumed";
  readonly log: ColdImportLog;
  resyncMode: ResyncMode;
  /** When a phase happens, in simulated ms; null before consent places it
   * (or for a phase this course has not). */
  phaseAt(phase: ColdPhase): number | null;
  /** A runtime's stages (by runtime id); null before consent. */
  runtimeAt(runtimeId: string): RuntimeTimes | null;
  /** When a tree (by its root id, or any rostered member) leaves Pending in
   * the catalog; null when it never does or consent has not placed it yet. */
  flipAt(sessionId: string): number | null;
  kindOf(sessionId: string): SessionKind | null;
  treeOf(sessionId: string): ColdTree | null;
  /** The flip batches, in order, once consent placed them. */
  batches(): readonly FlipBatch[];
  /** Deliver everything up to `ms`, in order, then rest there. */
  advanceTo(ms: number): void;
  advanceToPhase(phase: ColdPhase): void;
  /** The metadata the hub publishes now. */
  meta(): MetaState;
  /** One session's published stream state; null while it is not public. */
  streamState(sessionId: string): SessionRow["stream_state"] | null;
  /** What `/api/status` answers now, as the room reads it. */
  status(): CatalogStatus;
  /** The same, as the wire says it. */
  statusResponse(): StatusResponse;
  /** Just its phase, cheaply. */
  phase(): IngestPhase;
  /** Hold every status answer until `releaseStatus()` (answered then, at the
   * simulated time of the release), so a test can see whether the room asks. */
  holdStatus(): void;
  releaseStatus(): number;
  /** Status asks waiting on a hold. */
  heldStatus(): number;
  /** Deliver the resyncs `manual` mode held; returns how many. */
  deliverResyncs(): number;
  /** The run's control state as the wire says it (`run.state`). Only the
   * wire changes: a test that pauses does not advance until it resumes. */
  setRunState(state: IngestionState): void;
  /** Fire what a host fires when its route to the daemon changes, to every
   * listener of `ports.catalog.onConnectionChange`; returns how many. */
  connectionChange(): number;
  /** Put a host-side event on the trail, in order with the daemon's. */
  mark(what: string, detail?: string): void;
}

export interface ColdImport {
  readonly ports: RoomPorts;
  /** The app's roll call client (`RollCallPanel`'s); granting consent
   * places the schedule. */
  readonly consent: Pick<RosettaClient, "discoveries" | "enableRuntimes">;
  /** The same daemon for a host that builds its ports over a RosettaClient:
   * its REST routes as a `fetchFn`, and /ws/meta, which a fixture has no
   * socket for. Every ask goes through the same simulator and its log. */
  readonly daemon: {
    readonly fetch: typeof fetch;
    subscribeMeta(handlers: MetaSubscriptionHandlers): Closable;
  };
  readonly controller: ColdImportController;
}

interface SessionSub {
  readonly sessionId: string;
  readonly handlers: SessionSubscriptionHandlers;
  open: boolean;
  hadSnapshot: boolean;
  rows: TimelineEntry[];
}

const notReady = (what: string) => new ColdWireError(503, "not_ready", `${what} waits for canonical publication`);
/** A wire refusal as the SDK raises it, for a host that takes the ports. */
function asPort<T>(answer: () => T): Promise<T> {
  try {
    return Promise.resolve(answer());
  } catch (error) {
    return Promise.reject(error instanceof ColdWireError ? new ApiRequestError(error.status, error.code, error.message) : error);
  }
}
const records = (bytes: number) => Math.round(bytes / 3_800);

export function createColdImport(options: ColdImportOptions): ColdImport {
  const corpus = createColdCorpus(options);
  const course = options.course ?? "fresh";
  const now = corpus.now;
  const log: ColdImportLog = {
    status: [], projectStats: [], usage: [], dayUsage: [], search: [], boosts: [], opened: [], closed: [],
    subscribed: [], enabled: [], meta: [], resyncs: [], trail: [],
  };
  let clock = 0;
  let seq = 0;
  const mark = (what: string, detail: string | null = null) => {
    seq += 1;
    log.trail.push({ seq, at: clock, what, detail });
    return seq;
  };
  // A resume always has consent; a fresh build waits for the roll call
  // unless told consent was given before.
  const rollCall = course === "fresh" && (options.consent ?? "awaiting") === "awaiting";
  let schedule: Schedule | null = rollCall ? null : planSchedule(corpus, corpus.runtimes, DISCOVERY_MS, course);
  let runOverride: IngestionState | null = null;
  let resyncMode: ResyncMode = "immediate";
  let revealed = false;
  /** The mirror: sessions out of Pending, and which of them Streaming. */
  const flipped = new Set<string>();
  const streaming = new Set<string>();
  /** The store: sessions whose snapshot reads rows. A fresh flip commits and
   * publishes at once; a resume commits before the hub hears. */
  const committed = new Set<string>();
  const publicRuntimes = new Set<string>();
  const metaSubs = new Set<{ handlers: MetaSubscriptionHandlers; open: boolean }>();
  const sessionSubs = new Map<string, Set<SessionSub>>();
  const heldResyncs: SessionSub[] = [];
  const heldStatus: { resolve: (status: StatusResponse) => void }[] = [];
  let holding = false;
  const kindOf = (id: string) => corpus.kind.get(id) ?? null;
  const byId = new Map(corpus.sessions.map((row) => [row.id, row]));
  // The SDK's order for sessions: updated_at descending, then id (client.ts:1206-1209).
  const sessionOrder = [...corpus.sessions].sort((a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id));
  const rostered = corpus.trees.flatMap((tree) => tree.members);

  /* ── the mirror: what a session row says at this moment ─────────────── */

  /** The family rollup, behind the barrier until publication: a Pending
   * member and everything under it are left out (activity.rs:61-75). */
  const activityOf = (id: string, hide: boolean): SessionActivity | null => {
    if (hide && !flipped.has(id)) return null;
    let first = Number.POSITIVE_INFINITY;
    let last = Number.NEGATIVE_INFINITY;
    const visit = (member: string) => {
      if (hide && !flipped.has(member)) return;
      const own = flipped.has(member) ? corpus.own.get(member) : undefined;
      if (own !== undefined) {
        first = Math.min(first, own.first_at);
        last = Math.max(last, own.last_at);
      }
      for (const child of corpus.children.get(member) ?? []) visit(child);
    };
    visit(id);
    return Number.isFinite(first) ? { first_at: first, last_at: last } : null;
  };
  const rowNow = (identity: SessionRow): SessionRow => ({
    ...identity,
    activity: activityOf(identity.id, !revealed),
    stream_state: !flipped.has(identity.id) ? "pending" : streaming.has(identity.id) ? "streaming" : "indexed",
  });
  const rows = new Map<string, SessionRow>();
  const isPublic = (row: SessionRow) => publicRuntimes.has(row.runtime_id);
  const state = (): MetaState => {
    const runtimes = corpus.runtimes.filter((r) => publicRuntimes.has(r.id)).sort((a, b) => a.id.localeCompare(b.id));
    const sessions = sessionOrder.filter(isPublic).map((row) => rows.get(row.id) ?? row);
    const named = new Set(sessions.map((row) => row.project_id));
    const projects: ProjectRow[] = corpus.projects.filter((p) => named.has(p.id))
      .sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
    return { runtimes, projects, sessions };
  };
  /** Re-read the named rows and their ancestors; returns how many changed. */
  const refresh = (ids: Iterable<string>): number => {
    const affected = new Set<string>();
    for (const id of ids) {
      for (let at: string | null = id; at !== null && !affected.has(at); at = byId.get(at)?.parent_session_id ?? null) affected.add(at);
    }
    let changed = 0;
    for (const id of affected) {
      const identity = byId.get(id);
      if (identity === undefined || !isPublic(identity)) continue;
      const before = rows.get(id) ?? identity;
      const after = rowNow(identity);
      if (before.stream_state !== after.stream_state || before.activity?.first_at !== after.activity?.first_at ||
        before.activity?.last_at !== after.activity?.last_at) {
        rows.set(id, after);
        changed += 1;
      }
    }
    return changed;
  };
  const publishMeta = (events: number, why: string) => {
    if (events === 0) return;
    const snapshot = events > SUBSCRIBER_QUEUE;
    log.meta.push({ seq: mark("meta", why), at: clock, source: snapshot ? "snapshot" : "mutation", rows: events, why });
    for (const sub of [...metaSubs]) {
      if (!sub.open) continue;
      if (snapshot) {
        // The barrier: one resync, then the client re-reads the mirror.
        sub.handlers.onStatus?.("resyncing");
        sub.handlers.onStatus?.("live");
        sub.handlers.onChange?.(state(), "snapshot");
      } else {
        sub.handlers.onChange?.(state(), "mutation");
      }
    }
  };
  const subscribeMeta = (handlers: MetaSubscriptionHandlers): Closable => {
    const sub = { handlers, open: true };
    metaSubs.add(sub);
    queueMicrotask(() => {
      if (!sub.open) return;
      handlers.onStatus?.("live");
      handlers.onChange?.(state(), "snapshot");
    });
    return { close: () => { sub.open = false; metaSubs.delete(sub); } };
  };

  /* ── session sockets ─────────────────────────────────────────────────── */

  const snapshotOf = (sessionId: string): TimelineEntry[] => {
    const identity = byId.get(sessionId);
    const own = corpus.own.get(sessionId);
    if (identity === undefined || own === undefined || !committed.has(sessionId)) return [];
    return transcriptOf(identity, own);
  };
  let watermark = 0;
  const deliverSnapshot = (sub: SessionSub) => {
    if (!sub.open) return;
    sub.rows = snapshotOf(sub.sessionId);
    sub.hadSnapshot = true;
    watermark += 1;
    sub.handlers.onSnapshot?.({ entries: sub.rows, cursor: null, watermark: String(watermark) });
    sub.handlers.onStatus?.("live");
    sub.handlers.onChange?.(sub.rows);
    sub.handlers.onTimelineMetaChange?.(EMPTY_META);
  };
  /** The SDK's handling of a resync frame, then the daemon's re-read snapshot. */
  const resync = (sub: SessionSub) => {
    if (!sub.open) return;
    log.resyncs.push({ seq: mark("resync", sub.sessionId), sessionId: sub.sessionId, at: clock, delivered: true });
    if (sub.hadSnapshot) sub.handlers.onReset?.();
    sub.hadSnapshot = false;
    sub.handlers.onStatus?.("resyncing");
    deliverSnapshot(sub);
  };
  /** Raise the barrier on the open subscriptions of `ids`, as `resyncMode` says. */
  const barrier = (ids: Iterable<string>) => {
    for (const id of ids) {
      for (const sub of sessionSubs.get(id) ?? []) {
        if (resyncMode === "immediate") resync(sub);
        else if (resyncMode === "manual") heldResyncs.push(sub);
        else log.resyncs.push({ seq: mark("resync-lost", id), sessionId: id, at: clock, delivered: false });
      }
    }
  };
  const sessionClient: SessionClient = {
    subscribeSession(sessionId, handlers): SessionFeed {
      const sub: SessionSub = { sessionId, handlers, open: true, hadSnapshot: false, rows: [] };
      log.subscribed.push({ sessionId, at: clock });
      const subs = sessionSubs.get(sessionId) ?? new Set<SessionSub>();
      subs.add(sub);
      sessionSubs.set(sessionId, subs);
      queueMicrotask(() => deliverSnapshot(sub));
      return {
        entries: () => sub.rows,
        fetchPage: async () => ({ entries: [], cursor: null, watermark: String(watermark) }),
        reveal: () => {},
        loadEntry: async (timelineId) => sub.rows.some((row) => row.timeline_id === timelineId),
        close: () => {
          sub.open = false;
          subs.delete(sub);
        },
      };
    },
    subscribeCollaboration: () => ({ close() {}, restart() {} }),
    timelineEntry: async (sessionId, timelineId) => {
      const row = snapshotOf(sessionId).find((entry) => entry.timeline_id === timelineId);
      if (row === undefined) throw new ApiRequestError(404, "not_found", "no such message");
      return row;
    },
    reportWork: async () => [],
    command: async () => ({ command: "" }),
    readFileTarget: () => Promise.reject(new Error("The simulated archive keeps no files")),
    readPlan: () => Promise.reject(new Error("The simulated archive keeps no plans")),
    readTextAsset: () => Promise.reject(new Error("The simulated archive keeps no assets")),
    readAsset: () => Promise.reject(new Error("The simulated archive keeps no assets")),
  };
  const sessions: SessionPort = {
    open: (id) => {
      log.opened.push(id);
      return sessionClient;
    },
    close: (id) => {
      log.closed.push(id);
    },
  };

  /* ── applying the schedule ──────────────────────────────────────────── */

  const leavePending = (batch: FlipBatch) => {
    for (const id of batch.sessions) {
      flipped.add(id);
      committed.add(id);
    }
    for (const tree of batch.trees) if (tree.dirty && !tree.placeholderRoot) streaming.add(tree.id);
  };
  /** A fresh flip: the watched sessions' barriers, then the metadata
   * (crates/rosettad/src/cold_stream/flips.rs:9-11; crates/rosettad/src/engine.rs:207-226). */
  const applyBatch = (batch: FlipBatch) => {
    leavePending(batch);
    barrier(batch.sessions);
    publishMeta(refresh(batch.sessions), batch.kind === "close" ? "close" : "flip");
  };
  const publishRuntime = (times: RuntimeTimes) => {
    const projectsBefore = state().projects.length;
    publicRuntimes.add(times.runtime.id);
    for (const row of corpus.sessions) if (row.runtime_id === times.runtime.id) rows.set(row.id, rowNow(row));
    publishMeta(1 + (state().projects.length - projectsBefore) + times.sessions, "metadata");
  };
  /** A fresh build's `reveal_pending`: the still-Pending rows' rollups. */
  const revealPending = () => {
    revealed = true;
    const pending = corpus.sessions.filter((row) => !flipped.has(row.id) && isPublic(row)).map((row) => row.id);
    publishMeta(refresh(pending), "reveal");
  };
  /** A resumed runtime's end: its watched sessions resync and read the
   * store, which still holds them Pending (serve.rs:1329-1336). */
  const closeResumedRuntime = (times: RuntimeTimes) => {
    barrier(corpus.sessions.filter((row) => row.runtime_id === times.runtime.id).map((row) => row.id));
  };
  /** A resume's withheld commit: the store only (serve.rs:1929-1934). */
  const commitResume = () => {
    for (const id of rostered) committed.add(id);
  };
  /** A resume's publication: the barrier lifts and one whole-surface diff
   * carries every flip and every revealed rollup, then every watched session
   * resyncs (serve.rs:1397-1407). */
  const publishResume = (batch: FlipBatch) => {
    leavePending(batch);
    revealed = true;
    publishMeta(refresh(corpus.sessions.filter(isPublic).map((row) => row.id)), "publication");
    barrier(corpus.sessions.map((row) => row.id));
  };

  const events = (): { at: number; run: () => void }[] => {
    if (schedule === null) return [];
    const list: { at: number; run: () => void }[] = [];
    for (const times of schedule.runtimes) list.push({ at: times.metaPublic, run: () => publishRuntime(times) });
    if (schedule.course === "fresh") {
      for (const batch of schedule.batches) list.push({ at: batch.at, run: () => applyBatch(batch) });
      list.push({ at: schedule.phases.get("reveal")!, run: revealPending });
    } else {
      for (const times of schedule.runtimes) list.push({ at: times.close, run: () => closeResumedRuntime(times) });
      list.push({ at: schedule.commitAt!, run: commitResume });
      for (const batch of schedule.batches) list.push({ at: batch.at, run: () => publishResume(batch) });
    }
    // Stable: same-ms events keep the order they were listed in.
    return list.sort((a, b) => a.at - b.at);
  };
  let pendingEvents: { at: number; run: () => void }[] = [];
  let nextEvent = 0;
  const replan = () => {
    pendingEvents = events();
    nextEvent = 0;
  };
  replan();
  const advanceTo = (ms: number) => {
    if (ms < clock) throw new Error(`The simulated clock only moves forward (at ${clock}, asked ${ms})`);
    while (nextEvent < pendingEvents.length && pendingEvents[nextEvent]!.at <= ms) {
      const event = pendingEvents[nextEvent]!;
      clock = Math.max(clock, event.at);
      nextEvent += 1;
      event.run();
    }
    clock = ms;
  };

  /* ── /api/status at this moment ─────────────────────────────────────── */

  const laneBytesAt = (lane: Lane, t: number) =>
    t <= lane.start ? 0 : t >= lane.end ? lane.bytes : (lane.bytes * (t - lane.start)) / (lane.end - lane.start);
  const at = (name: ColdPhase) => schedule?.phases.get(name) ?? Number.POSITIVE_INFINITY;
  const phaseOf = (t: number): IngestPhase => {
    if (schedule === null) return t < DISCOVERY_MS ? "discovering" : "awaiting_consent";
    if (t >= at("live")) return "live";
    if (t >= at("publishing")) return "publishing";
    if (t >= at("finalizing")) return "finalizing";
    if (t < schedule.consentAt) return "discovering";
    if (schedule.course === "resumed") {
      // Each runtime's replay says restoring; its skim says backfilling again.
      return schedule.runtimes.some((r) => r.replay !== null && t >= r.replay.start && t < r.replay.end) ? "restoring" : "backfilling";
    }
    // The grant registers each runtime queued; its skim moves the phase on
    // (health.rs:161-189). Consent given earlier never waits.
    if (t < Math.min(...schedule.runtimes.map((r) => r.skimStart))) return rollCall ? "awaiting_consent" : "discovering";
    return "backfilling";
  };
  const freshRow = (times: RuntimeTimes, t: number): RuntimeBackfill => {
    const streamed = times.lanes.reduce((sum, lane) => sum + laneBytesAt(lane, t), 0);
    const stage: BackfillState = t >= times.close ? "done" : t >= (times.applying ?? Infinity) ? "applying" : t >= times.readEnd ? "preparing"
      : t >= times.metaPublic ? "streaming" : t >= times.metaPublic - 20 ? "queued" : t >= times.skimStart ? "skimming" : "queued";
    const skimmed = t >= times.metaPublic - 20 ? 1
      : stage === "skimming" ? (t - times.skimStart) / Math.max(1, times.metaPublic - 20 - times.skimStart) : 0;
    return {
      kind: times.runtime.kind, runtime_id: times.runtime.id, label: null, state: stage,
      files_total: times.files, bytes_total: times.bytes, files_skimmed: Math.round(times.files * Math.min(1, skimmed)),
      bytes_streamed: Math.round(stage === "done" ? times.bytes : streamed),
      sessions_total: Math.round(times.sessions * Math.min(1, skimmed)), records: records(streamed),
    };
  };
  /** A resumed runtime: queued, its replay streaming, its skim, then its
   * catch-up with no byte total (crates/rosettad/src/cold_progress.rs:37-46),
   * then done. */
  const resumedRow = (times: RuntimeTimes, t: number): RuntimeBackfill => {
    const replay = times.replay!;
    const caughtUp = times.lanes.reduce((sum, lane) => sum + laneBytesAt(lane, t), 0);
    const base = { kind: times.runtime.kind, runtime_id: times.runtime.id, label: null, files_total: times.files };
    if (t >= times.metaPublic) {
      const stage: BackfillState = t >= times.close ? "done" : t >= times.readEnd ? "preparing" : "streaming";
      return { ...base, state: stage, bytes_total: 0, files_skimmed: times.files, sessions_total: times.sessions,
        bytes_streamed: Math.round(caughtUp), records: records(caughtUp) };
    }
    if (t >= times.skimStart) {
      const share = (t - times.skimStart) / Math.max(1, times.metaPublic - times.skimStart);
      return { ...base, state: "skimming", bytes_total: times.bytes, files_skimmed: Math.round(times.files * share),
        sessions_total: Math.round(times.sessions * share), bytes_streamed: 0, records: 0 };
    }
    const restored = t >= replay.start ? (replay.bytes * (t - replay.start)) / Math.max(1, replay.end - replay.start) : 0;
    return { ...base, state: t >= replay.start ? "streaming" : "queued", bytes_total: times.bytes, files_skimmed: 0,
      sessions_total: 0, bytes_streamed: Math.round(restored), records: records(restored) };
  };
  const board = (t: number): ProjectBackfill[] => {
    if (schedule === null) return [];
    const resumed = schedule.course === "resumed";
    // Seeded per planned (rostered) session under its owner, children
    // included; a session counts done once it is out of Pending in the
    // catalog. A resume knows no byte totals (board.rs:51-57, 127-131).
    const byProject = new Map<string | null, ProjectBackfill>();
    const seed = (id: string, streamed: number) => {
      const projectId = byId.get(id)!.project_id;
      const entry = byProject.get(projectId) ?? { project_id: projectId, sessions_total: 0, sessions_done: 0, bytes_total: 0, bytes_streamed: 0 };
      byProject.set(projectId, {
        ...entry,
        sessions_total: entry.sessions_total + 1,
        sessions_done: entry.sessions_done + (flipped.has(id) ? 1 : 0),
        bytes_total: resumed ? 0 : entry.bytes_total + (corpus.bytesOf.get(id) ?? 0),
        bytes_streamed: entry.bytes_streamed + Math.round(streamed),
      });
    };
    for (const times of schedule.runtimes) {
      if (t < times.metaPublic) continue;
      const read = new Map<string, number>();
      for (const lane of times.lanes) {
        for (const span of lane.spans) {
          const streamed = Math.max(0, Math.min(span.to, laneBytesAt(lane, t)) - span.from);
          for (const id of span.tree.members) read.set(id, (streamed * (corpus.bytesOf.get(id) ?? 0)) / Math.max(1, span.tree.bytes));
        }
      }
      for (const tree of times.trees) for (const id of tree.members) seed(id, read.get(id) ?? 0);
    }
    // Largest plans first, ties by id (board.rs:121-140).
    return [...byProject.values()].sort((a, b) => b.bytes_total - a.bytes_total || (a.project_id ?? "").localeCompare(b.project_id ?? ""));
  };
  /** The control's run: running, then succeeded once publication is
   * revealed (serve.rs:1605-1607, 1408). */
  const runAt = (t: number, phase: IngestPhase): Ingestion => {
    const done = t >= at("reveal");
    const runState: IngestionState = runOverride ?? (done ? "succeeded" : "running");
    const runPhase: IngestionPhase | null = done ? null
      : phase === "finalizing" || phase === "publishing" || phase === "restoring" ? phase
      : phase === "discovering" || phase === "awaiting_consent" ? "discovering" : "capturing";
    return {
      id: "ing_cold_import", index_id: "idx_cold_import", revision: done ? 3 : schedule === null ? 1 : 2, state: runState,
      phase: runPhase, created_at: now, updated_at: now + t,
      progress_epoch: course === "resumed" ? "epoch-resumed" : "epoch-fresh",
      runtime_ids: schedule === null ? [] : schedule.runtimes.map((r) => r.runtime.id),
      allowed_actions: runState === "running" ? ["pause"] : runState === "paused" ? ["resume"] : [], error: null,
    };
  };
  const published = () => clock >= at("canonical");
  const searchAt = (t: number): SearchState => {
    const total = Math.max(1, Math.round(899_402 * corpus.scale));
    const building = (progress: SearchBuildProgress | null): SearchState => ({
      readiness: "building", progress, projection_epoch: published() ? "epoch-sim" : null, revision: null,
    });
    if (t >= at("search-ready")) return { readiness: "ready", progress: null, projection_epoch: "epoch-sim", revision: 1 };
    for (const stage of ["publishing", "validating", "optimizing"] as const) {
      if (t >= at(`search-${stage}`)) return building({ stage, documents_done: total, documents_total: total });
    }
    if (t >= at("search-indexing")) {
      const share = (t - at("search-indexing")) / (at("search-optimizing") - at("search-indexing"));
      return building({ stage: "indexing", documents_done: Math.round(total * share), documents_total: total });
    }
    return building(null);
  };
  const statusResponse = (): StatusResponse => {
    const t = clock;
    const phase = phaseOf(t);
    const metadataReady = schedule?.phases.get("metadata") ?? null;
    const ingest: IngestProgress = {
      phase, run: runAt(t, phase),
      metadata_ready_at: metadataReady !== null && t >= metadataReady ? now + metadataReady : null,
      runtimes: schedule === null ? [] : schedule.runtimes.filter((r) => t >= r.register)
        .map((r) => (schedule!.course === "resumed" ? resumedRow(r, t) : freshRow(r, t))),
      projects: board(t),
    };
    return {
      protocol_version: PROTOCOL_VERSION, schema_version: SCHEMA_VERSION, canonical_ready: published(), search: searchAt(t),
      daemon: {
        started_at: now, uptime_ms: t, runtimes: [], ingest,
        hub: { subscribers: metaSubs.size, rings: 0, session_mutations: 0, meta_events: log.meta.length, resyncs: log.resyncs.length,
          rings_evicted: 0, overflow_rewarms: 0 },
      },
    };
  };

  /* ── the daemon's answers, route by route ───────────────────────────── */

  const rootsOf = (projectId: string) => state().sessions
    .filter((row) => row.parent_session_id === null && (row.project_id ?? UNASSIGNED_PROJECT_ID) === projectId);
  const wire: ColdWire = {
    status(signal) {
      log.status.push(clock);
      mark("status-ask");
      if (signal?.aborted) {
        mark("status-abort");
        return Promise.reject(aborted());
      }
      if (!holding) {
        mark("status-answer");
        return Promise.resolve(statusResponse());
      }
      return new Promise<StatusResponse>((resolve, reject) => {
        const held = { resolve };
        heldStatus.push(held);
        signal?.addEventListener("abort", () => {
          const index = heldStatus.indexOf(held);
          if (index < 0) return;
          heldStatus.splice(index, 1);
          mark("status-abort");
          reject(aborted());
        }, { once: true });
      });
    },
    discoveries(): DiscoveriesResponse {
      return {
        discoveries: [
          ...corpus.runtimes.map((runtime) => {
            const trees = corpus.trees.filter((tree) => tree.runtimeId === runtime.id);
            const count = corpus.sessions.filter((row) => row.runtime_id === runtime.id).length;
            return {
              kind: runtime.kind, data_root: runtime.data_root, installed: true, files: count, sessions: count,
              bytes: trees.reduce((sum, tree) => sum + tree.bytes, 0),
              newest_modified_ms: Math.max(...trees.map((tree) => tree.recency)), enabled: schedule !== null,
            };
          }),
          { kind: "gemini", data_root: "~/.gemini", installed: false, files: 0, sessions: 0, bytes: 0, newest_modified_ms: 0, enabled: false },
        ],
      };
    },
    enableRuntimes(kinds): EnableResponse {
      log.enabled.push({ kinds: [...kinds], at: clock });
      if (schedule !== null) throw new ColdWireError(409, "invalid_transition", "consent was already granted");
      const enrolled = corpus.runtimes.filter((runtime) => kinds.includes(runtime.kind));
      if (enrolled.length === 0) throw new ColdWireError(400, "bad_request", "no installed agent was named");
      schedule = planSchedule(corpus, enrolled, Math.max(clock, DISCOVERY_MS), course);
      replan();
      mark("grant", enrolled.map((runtime) => runtime.kind).join(","));
      return { enabled: enrolled.map((runtime) => runtime.kind) };
    },
    heatmap(projectId) {
      log.projectStats.push({ projectId, at: clock });
      if (!published()) throw notReady("Project figures");
      return { days: [] };
    },
    contribution(projectId): ContributionResponse {
      if (!published()) throw notReady("Project figures");
      return {
        rows: rootsOf(projectId).filter((row) => row.stream_state !== "pending").map((row) => {
          const tokens = 20_000 + (Number.parseInt(row.id.slice(2, 6), 16) % 400) * 1_000;
          return { session_id: row.id, project_id: row.project_id, tokens, commits: 0, edits: 0, loc: 0,
            messages: 3 + (Number.parseInt(row.id.slice(0, 2), 16) % 6), cost_micro_usd: tokens * 3, billed_micro_usd: null,
            unpriced_tokens: 0, unrecorded_output: 0, reported_cost_adjustment_micro_usd: 0 };
        }),
      };
    },
    usage() {
      log.usage.push(clock);
      if (!published()) throw notReady("Model usage");
      return { models: [], hours: [] };
    },
    dayUsage(_projectId, day) {
      log.dayUsage.push(clock);
      if (!published()) throw notReady("Day usage");
      return { day, models: [] };
    },
    search(request, signal): SearchResponse {
      log.search.push({ q: request.q, at: clock });
      if (signal?.aborted) throw aborted();
      const answered = searchAt(clock);
      if (answered.readiness !== "ready") return { state: answered, hits: [] };
      const q = request.q.trim().toLowerCase();
      const hits: SearchHit[] = [];
      for (const row of state().sessions) {
        if (hits.length >= request.limit) break;
        const title = row.title?.toLowerCase() ?? "";
        if (q === "" || !title.includes(q) || row.stream_state === "pending") continue;
        const entry = snapshotOf(row.id)[0];
        if (entry === undefined) continue;
        hits.push({ timeline_id: entry.timeline_id, session_id: row.id, kind: entry.body.type, at: entry.at,
          snippet: `Please [${request.q.trim()}] and keep the change small.`, score: -1 });
      }
      return { state: answered, hits };
    },
    boost(sessionId): BoostResponse {
      log.boosts.push({ sessionId, at: clock });
      return { queued: false };
    },
    artifacts(): ArtifactPage {
      return { entries: [], next_cursor: null, canonical_ready: published() };
    },
  };

  /* ── the room's ports ───────────────────────────────────────────────── */

  const connection = new Set<() => void>();
  const catalog: CatalogPort = {
    subscribeMeta,
    status: async (signal) => catalogStatusOf(await wire.status(signal)),
    // A host fires this when its route to the daemon changes
    // (clients/app/renderer/src/host/rosetta-ports.ts:24-31).
    onConnectionChange(listener) {
      const heard = () => {
        mark("connection-change");
        listener();
      };
      connection.add(heard);
      return () => connection.delete(heard);
    },
    projectStats: (projectId) => asPort(() => ({ heat: wire.heatmap(projectId).days, contribution: wire.contribution(projectId).rows })),
    usage: (projectId) => asPort(() => wire.usage(projectId)),
    dayUsage: (projectId, day) => asPort(() => wire.dayUsage(projectId, day)),
    search: (request, signal) => asPort(() => wire.search(request, signal)),
    boost: (sessionId) => asPort(() => wire.boost(sessionId)),
  };
  const assets: AssetSource = {
    imageAssetUrl: () => null,
    attachmentAssetUrl: (asset) => asset.url,
  };
  const consent: ColdImport["consent"] = {
    discoveries: () => asPort(() => wire.discoveries()),
    enableRuntimes: (kinds) => asPort(() => wire.enableRuntimes(kinds)),
  };

  /* ── the controller ─────────────────────────────────────────────────── */

  const controller: ColdImportController = {
    corpus, now, course, log,
    get clock() { return clock; },
    get resyncMode() { return resyncMode; },
    set resyncMode(mode: ResyncMode) { resyncMode = mode; },
    phaseAt: (phase) => schedule?.phases.get(phase) ?? null,
    runtimeAt: (runtimeId) => schedule?.runtimes.find((r) => r.runtime.id === runtimeId) ?? null,
    flipAt: (sessionId) => {
      const tree = corpus.treeOf.get(sessionId) ?? corpus.trees.find((t) => t.id === sessionId);
      return tree === undefined ? null : (schedule?.flipAt.get(tree.id) ?? null);
    },
    kindOf,
    treeOf: (sessionId) => corpus.treeOf.get(sessionId) ?? corpus.trees.find((t) => t.id === sessionId) ?? null,
    batches: () => schedule?.batches ?? [],
    advanceTo,
    advanceToPhase(phase) {
      const when = schedule?.phases.get(phase);
      if (when === undefined) throw new Error(`No ${phase} phase is scheduled${schedule === null ? " before consent" : ` for a ${course} import`}`);
      advanceTo(Math.max(clock, when));
    },
    meta: state,
    streamState: (sessionId) => {
      const identity = byId.get(sessionId);
      return identity === undefined || !isPublic(identity) ? null : (rows.get(sessionId) ?? identity).stream_state;
    },
    status: () => catalogStatusOf(statusResponse()),
    statusResponse,
    phase: () => phaseOf(clock),
    holdStatus() { holding = true; },
    releaseStatus() {
      holding = false;
      const waiting = heldStatus.splice(0);
      for (const held of waiting) {
        mark("status-answer");
        held.resolve(statusResponse());
      }
      return waiting.length;
    },
    heldStatus: () => heldStatus.length,
    deliverResyncs() {
      const waiting = heldResyncs.splice(0);
      for (const sub of waiting) resync(sub);
      return waiting.length;
    },
    setRunState(next) { runOverride = next === "running" ? null : next; },
    connectionChange() {
      const listeners = [...connection];
      for (const listener of listeners) listener();
      return listeners.length;
    },
    mark(what, detail) { mark(what, detail ?? null); },
  };

  return {
    ports: { catalog, sessions, assets }, consent,
    daemon: { fetch: fetchOver(wire), subscribeMeta },
    controller,
  };
}
