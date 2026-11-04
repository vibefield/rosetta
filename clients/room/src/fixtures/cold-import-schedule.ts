// When things happen in a simulated cold import: each runtime's stages, each
// root tree's flip and every phase, for a fresh build and for a cross-launch
// resume, as offsets in simulated ms. `cold-import.ts` replays it. Only the
// structure is rosettad's; every number is indicative.
//
// A fresh RAM build (crates/rosettad/src/serve.rs `run_fresh_fast_serve`):
//
// - Consent: the daemon waits for the roll call with nothing read
//   (serve.rs:651-689); the grant registers each runtime queued
//   (crates/server/src/health.rs:161), and its skim makes the phase
//   `backfilling` (health.rs:179-189).
// - Metadata: each runtime's batch is public in registry order and its
//   producer starts streaming at once
//   (crates/rosettad/src/cold_stream/writer.rs:361-365).
// - Flips: newest trees first in two lanes per runtime, every tree whose
//   largest source is at most 64 MiB in lane S and the rest in the single big
//   lane B0 (crates/pipeline/src/closed/walk_order.rs:1-11, 38-43); a tree
//   flips once its last source commits, the first batch at once and then at
//   most one batch per 100 ms per runtime, capped at 64 trees, 512 sessions
//   and 16 MiB of buffered tails, a batch's first tree always taken
//   (crates/rosettad/src/cold_stream/flips.rs:29-43;
//   crates/pipeline/src/streamed/trees.rs:199-213). A tree flips Indexed,
//   except a session whose source was unclean, which flips Streaming
//   (trees.rs:226-232, 274). Bound trees flip at their producer's close; a
//   placeholder root never flips.
// - Status: `preparing` when reading ends (crates/rosettad/src/cold_progress.rs:
//   73-76), `applying` and `done` at the producer's close, `finalizing` and
//   `publishing` (serve.rs:1575, 1924-1935), canonical readiness once the read
//   router is published while the phase still says publishing (serve.rs:1953;
//   crates/server/src/http.rs:821), `reveal_pending` (serve.rs:1605), the
//   search worker's wake (serve.rs:1616) and then `live` (serve.rs:1784).
//
// A cross-launch resume (serve.rs `run_resumed_fresh_serve`, 1114-1452) takes
// each consented runtime in registry order, one after the other: it replays
// that runtime's staged ledger (`restoring`, the runtime streaming its replay,
// serve.rs:1192-1193), skims its metadata (`backfilling`, serve.rs:1246-1280)
// and makes it public (serve.rs:1279), seeds the board with every session but
// no byte totals (cold_progress.rs:28-46: `fresh` false), then catches up on
// the sources the interrupted build had not read, and at the end resyncs that
// runtime's open sessions, which still read empty (serve.rs:1336). The last
// runtime's skim stamps metadata-ready (serve.rs:1295-1296). Every readiness
// decision is withheld: the single commit lands in the RAM store just before
// `publishing` and the hub hears nothing of it (serve.rs:1929-1934). Only once
// the read router is published does the board count every session done, the
// barrier lift and one whole-surface diff carry every flip and every revealed
// activity at once (`rewarm`, crates/server/src/hub.rs:230-241, a snapshot
// when it overflows the queue), then every open session resyncs, then the run
// succeeds (serve.rs:1397-1408).
//
// Deliberate simplifications:
// - Times are anchored at full scale on the milestones of the 2026-11-02
//   MacBook R1 cold-build trace (structure-only aggregates, not committed):
//   metadata-ready 2.48 s, first-readable 5.35 s, newest giant 8.93 s,
//   runtime-readable 110.9 s, canonical-ready 139.3 s. A smaller scale keeps
//   the timeline and shrinks the counts.
// - A batch's buffered tails are 2.5% of its trees' bytes (the trace: 169 MB of
//   tails over 6.8 GB of trees, never more than 13 MB in one batch).
// - The search worker wakes before `live`, but reports progress only once it
//   has counted its documents (crates/search/src/worker.rs:280-282); here the
//   count always outlasts the step to `live`, so the room's "Preparing
//   search" (search waiting, phase live) is always on screen for a while. On
//   the real daemon the two race and that state may be fleeting.
// - A resume's catch-up reads the older half of each runtime's trees (the
//   interrupted build walked newest first); its replay restores the rest.
import type { RuntimeRow } from "@vibefield/rosetta-protocol";
import type { ColdCorpus, ColdTree } from "./cold-import-corpus";

/** The hub's per-subscriber queue (crates/server/src/hub.rs:44). */
export const SUBSCRIBER_QUEUE = 1_024;
const FLIP_INTERVAL_MS = 100;
const FLIP_MAX_TREES = 64;
const FLIP_MAX_SESSIONS = 512;
const FLIP_MAX_TAIL_BYTES = 16 * 1024 * 1024;
const TAIL_SHARE = 0.025;
/** The sole writer returns to the flip loop between apply transactions;
 * this grid gives about the trace's cadence (178 batches over ~106 s). */
const WRITER_RETURN_MS = 600;
/** Before consent the daemon's first walk is still discovering. */
export const DISCOVERY_MS = 300;

/** A fresh build: offsets from consent, ms. Two entries: claude-code, codex. */
const FRESH = {
  register: 40, skimStart: 80,
  metaPublic: [2_400, 2_480], captureLatency: 2_200,
  readEnd: [108_900, 32_700], applying: [110_400, 33_200], close: [110_900, 33_500],
  publishing: 133_000, canonical: 139_200, reveal: 139_290, live: 139_350,
  search: { indexing: 139_600, optimizing: 144_600, validating: 144_800, publishing: 145_000, ready: 145_300 },
} as const;

/** A resume: each runtime's stages in turn (ms each), then the end, as
 * offsets from the last runtime's close. */
const RESUMED = {
  runtimes: [
    { replay: 2_400, skim: 300, catchUp: 21_000, prepare: 500 },
    { replay: 800, skim: 150, catchUp: 3_500, prepare: 300 },
  ],
  finalizing: 5, publishing: 8_705, canonical: 12_705, reveal: 12_745, live: 12_805,
  search: { indexing: 13_005, optimizing: 16_005, validating: 16_205, publishing: 16_505, ready: 16_705 },
} as const;

export type ColdPhase =
  | "consent" | "restoring" | "metadata" | "first-flip" | "newest-giant" | "reading-done"
  | "runtime-close" | "finalizing" | "publishing" | "canonical" | "reveal" | "live"
  | "search-indexing" | "search-optimizing" | "search-validating" | "search-publishing" | "search-ready";

export interface Lane {
  readonly start: number;
  readonly end: number;
  readonly bytes: number;
  /** Each tree's byte span inside the lane, in walk order. */
  readonly spans: readonly { readonly tree: ColdTree; readonly from: number; readonly to: number }[];
}

/** One runtime's stages, in simulated ms. */
export interface RuntimeTimes {
  readonly runtime: RuntimeRow;
  /** Registered, queued, with discovery's census denominators. */
  readonly register: number;
  /** A resume's replay of the staged ledger (`restoring`); null when fresh. */
  readonly replay: { readonly start: number; readonly end: number; readonly bytes: number } | null;
  readonly skimStart: number;
  /** The skim lands and its metadata is public. */
  readonly metaPublic: number;
  /** Capture (fresh) or catch-up (resumed) streams from here. */
  readonly streamStart: number;
  /** Reading is over: `preparing`. */
  readonly readEnd: number;
  /** The producer's close applies (fresh only). */
  readonly applying: number | null;
  /** `done`. */
  readonly close: number;
  /** What this run reads: fresh, every tree; resumed, the catch-up. */
  readonly lanes: readonly Lane[];
  readonly trees: readonly ColdTree[];
  readonly bytes: number;
  readonly files: number;
  readonly sessions: number;
}

/** How a batch of trees leaves Pending. `tree`: a flip transaction during
 * capture. `close`: bound trees at their producer's close. `publication`:
 * a resume's one commit, public only at publication. */
export type FlipKind = "tree" | "close" | "publication";

/** One flip: these trees' rostered sessions leave Pending. */
export interface FlipBatch {
  readonly at: number;
  /** Empty for a resume's publication, which spans every runtime. */
  readonly runtimeId: string;
  readonly trees: readonly ColdTree[];
  readonly sessions: readonly string[];
  readonly kind: FlipKind;
}

export interface Schedule {
  readonly course: "fresh" | "resumed";
  readonly consentAt: number;
  readonly runtimes: readonly RuntimeTimes[];
  readonly batches: readonly FlipBatch[];
  readonly flipAt: ReadonlyMap<string, number>;
  readonly phases: ReadonlyMap<ColdPhase, number>;
  /** A resume's commit into the RAM store (snapshots read it; the hub does
   * not); null for a fresh build, whose flips commit and publish at once. */
  readonly commitAt: number | null;
}

/** Walk one lane newest first (recency, then tree id) at a constant rate
 * over [start, end]: a tree is ready once its last byte is read. */
function walkLane(trees: readonly ColdTree[], start: number, end: number): Lane {
  const ordered = [...trees].sort((a, b) => b.recency - a.recency || a.id.localeCompare(b.id));
  let at = 0;
  const spans = ordered.map((tree) => {
    const from = at;
    at += tree.bytes;
    return { tree, from, to: at };
  });
  return { start, end, bytes: at, spans };
}

const tailOf = (tree: ColdTree) => Math.round(tree.bytes * TAIL_SHARE);

/** D9: the first ready batch at once, then at most one per 100 ms, taken
 * when the writer comes back, capped by trees, sessions and tail bytes; a
 * batch's first tree is always taken. */
function batchFlips(runtimeId: string, ready: readonly { tree: ColdTree; at: number }[]): FlipBatch[] {
  const queue = [...ready].sort((a, b) => a.at - b.at || a.tree.id.localeCompare(b.tree.id));
  const batches: FlipBatch[] = [];
  let last: number | null = null;
  let next = 0;
  while (next < queue.length) {
    const head: number = queue[next]!.at;
    let at: number = last === null ? head : Math.max(head, last + FLIP_INTERVAL_MS);
    if (last !== null) at = Math.ceil(at / WRITER_RETURN_MS) * WRITER_RETURN_MS;
    const trees: ColdTree[] = [];
    let sessions = 0;
    let tails = 0;
    while (next < queue.length && queue[next]!.at <= at && trees.length < FLIP_MAX_TREES) {
      const tree = queue[next]!.tree;
      if (trees.length > 0 && (sessions + tree.members.length > FLIP_MAX_SESSIONS || tails + tailOf(tree) > FLIP_MAX_TAIL_BYTES)) break;
      trees.push(tree);
      sessions += tree.members.length;
      tails += tailOf(tree);
      next += 1;
    }
    batches.push({ at, runtimeId, trees, sessions: trees.flatMap((tree) => tree.members), kind: "tree" });
    last = at;
  }
  return batches;
}

const filesOf = (trees: readonly ColdTree[]) => Math.round(trees.reduce((sum, tree) => sum + tree.members.length, 0) * 1.72);
const bytesOf = (trees: readonly ColdTree[]) => trees.reduce((sum, tree) => sum + tree.bytes, 0);

function planFresh(corpus: ColdCorpus, enrolled: readonly RuntimeRow[], consentAt: number) {
  const runtimes: RuntimeTimes[] = [];
  const batches: FlipBatch[] = [];
  for (const runtime of enrolled) {
    const slot = corpus.runtimes.indexOf(runtime);
    const trees = corpus.trees.filter((tree) => tree.runtimeId === runtime.id);
    const metaPublic = consentAt + FRESH.metaPublic[slot]!;
    const laneStart = metaPublic + FRESH.captureLatency;
    const readEnd = consentAt + FRESH.readEnd[slot]!;
    const lanes = [walkLane(trees.filter((t) => !t.big), laneStart, readEnd), walkLane(trees.filter((t) => t.big), laneStart, readEnd)];
    const times: RuntimeTimes = {
      runtime, register: consentAt + FRESH.register, replay: null, skimStart: consentAt + FRESH.skimStart,
      metaPublic, streamStart: metaPublic, readEnd,
      applying: consentAt + FRESH.applying[slot]!, close: consentAt + FRESH.close[slot]!,
      lanes, trees, bytes: bytesOf(trees), files: filesOf(trees),
      sessions: corpus.sessions.filter((row) => row.runtime_id === runtime.id).length,
    };
    runtimes.push(times);
    const ready = lanes.flatMap((lane) => lane.spans.filter((span) => !span.tree.bound).map((span) => ({
      tree: span.tree, at: Math.round(lane.start + ((lane.end - lane.start) * span.to) / Math.max(1, lane.bytes)),
    })));
    batches.push(...batchFlips(runtime.id, ready));
    const bound = trees.filter((tree) => tree.bound);
    if (bound.length > 0) {
      batches.push({ at: times.close, runtimeId: runtime.id, trees: bound, sessions: bound.flatMap((t) => t.members), kind: "close" });
    }
  }
  return { runtimes, batches };
}

function planResumed(corpus: ColdCorpus, enrolled: readonly RuntimeRow[], consentAt: number) {
  const runtimes: RuntimeTimes[] = [];
  let at = consentAt;
  for (const runtime of enrolled) {
    const slot = corpus.runtimes.indexOf(runtime);
    const stages = RESUMED.runtimes[slot]!;
    const trees = corpus.trees.filter((tree) => tree.runtimeId === runtime.id);
    // The interrupted build walked newest first: its ledger holds the newer
    // half, which the replay restores; the catch-up reads the rest.
    const byRecency = [...trees].sort((a, b) => b.recency - a.recency || a.id.localeCompare(b.id));
    const restored = byRecency.slice(0, Math.ceil(byRecency.length / 2));
    const unread = byRecency.slice(restored.length);
    const replayEnd = at + stages.replay;
    const metaPublic = replayEnd + stages.skim;
    const readEnd = metaPublic + stages.catchUp;
    const close = readEnd + stages.prepare;
    runtimes.push({
      runtime, register: consentAt, replay: { start: at, end: replayEnd, bytes: bytesOf(restored) },
      skimStart: replayEnd, metaPublic, streamStart: metaPublic, readEnd, applying: null, close,
      lanes: [walkLane(unread, metaPublic, readEnd)], trees, bytes: bytesOf(trees), files: filesOf(trees),
      sessions: corpus.sessions.filter((row) => row.runtime_id === runtime.id).length,
    });
    at = close;
  }
  return { runtimes, lastClose: at };
}

export function planSchedule(
  corpus: ColdCorpus, enrolled: readonly RuntimeRow[], consentAt: number, course: "fresh" | "resumed",
): Schedule {
  const phases = new Map<ColdPhase, number>([["consent", consentAt]]);
  let runtimes: RuntimeTimes[];
  let batches: FlipBatch[];
  let end: { readonly finalizing: number; readonly publishing: number; readonly canonical: number; readonly reveal: number; readonly live: number;
    readonly search: { readonly indexing: number; readonly optimizing: number; readonly validating: number; readonly publishing: number; readonly ready: number } };
  let commitAt: number | null = null;
  if (course === "fresh") {
    ({ runtimes, batches } = planFresh(corpus, enrolled, consentAt));
    const lastClose = Math.max(...runtimes.map((r) => r.close));
    const from = (offset: number) => consentAt + offset;
    end = {
      finalizing: lastClose + 5, publishing: from(FRESH.publishing), canonical: from(FRESH.canonical),
      reveal: from(FRESH.reveal), live: from(FRESH.live),
      search: {
        indexing: from(FRESH.search.indexing), optimizing: from(FRESH.search.optimizing), validating: from(FRESH.search.validating),
        publishing: from(FRESH.search.publishing), ready: from(FRESH.search.ready),
      },
    };
  } else {
    const planned = planResumed(corpus, enrolled, consentAt);
    runtimes = planned.runtimes;
    const from = (offset: number) => planned.lastClose + offset;
    end = {
      finalizing: from(RESUMED.finalizing), publishing: from(RESUMED.publishing), canonical: from(RESUMED.canonical),
      reveal: from(RESUMED.reveal), live: from(RESUMED.live),
      search: {
        indexing: from(RESUMED.search.indexing), optimizing: from(RESUMED.search.optimizing), validating: from(RESUMED.search.validating),
        publishing: from(RESUMED.search.publishing), ready: from(RESUMED.search.ready),
      },
    };
    // Withheld until publication: one commit, public at the reveal.
    const trees = runtimes.flatMap((r) => r.trees);
    batches = [{ at: end.reveal, runtimeId: "", trees, sessions: trees.flatMap((t) => t.members), kind: "publication" }];
    commitAt = end.publishing;
    phases.set("restoring", consentAt);
  }
  batches.sort((a, b) => a.at - b.at || a.runtimeId.localeCompare(b.runtimeId));
  const flipAt = new Map<string, number>();
  for (const batch of batches) for (const tree of batch.trees) if (!flipAt.has(tree.id)) flipAt.set(tree.id, batch.at);
  phases.set("metadata", Math.max(...runtimes.map((r) => r.metaPublic)));
  if (course === "fresh" && batches[0] !== undefined) phases.set("first-flip", batches[0].at);
  const giant = corpus.trees[0];
  if (course === "fresh" && giant !== undefined && flipAt.has(giant.id)) phases.set("newest-giant", flipAt.get(giant.id)!);
  phases.set("reading-done", Math.max(...runtimes.map((r) => r.readEnd)));
  phases.set("runtime-close", Math.max(...runtimes.map((r) => r.close)));
  for (const name of ["finalizing", "publishing", "canonical", "reveal", "live"] as const) phases.set(name, end[name]);
  phases.set("search-indexing", end.search.indexing);
  phases.set("search-optimizing", end.search.optimizing);
  phases.set("search-validating", end.search.validating);
  phases.set("search-publishing", end.search.publishing);
  phases.set("search-ready", end.search.ready);
  return { course, consentAt, runtimes, batches, flipAt, phases, commitAt };
}
