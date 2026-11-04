// The archive a simulated cold import walks: runtimes, projects, every
// session's identity row, and the root trees the walk admits — seeded,
// synthetic and structure-only. `cold-import.ts` schedules the import over it.
//
// Its default size is the aggregate shape of the 2026-11-02 MacBook R1
// cold-build trace (structure-only aggregates, not committed; never its
// ids): 8,612 metadata sessions, of which 8,062
// are rostered and become readable, in 1,139 root trees over 119 projects;
// 40 trees take the big lane, the newest of them a 668 MB giant; the recent
// trees are few and large (24 sessions within a day of the newest, 1,055
// within a week). `scale` shrinks every count but keeps every kind of row:
// the giant newest tree, bound trees, dirty trees whose root flips
// Streaming, metadata-only placeholders, never-rostered roots and children,
// the unassigned bucket and both agents.
//
// Rows are shaped exactly as the daemon's metadata pass publishes them
// (crates/pipeline/src/meta.rs:740-826): Pending, no activity, `updated_at`
// the source's mtime. A placeholder is the parent row meta.rs:776-804 makes
// for a retained child whose parent transcript is gone: no project, cwd,
// title, summary or delegation, and it never leaves Pending.
import type { ProjectRow, RuntimeRow, SessionActivity, SessionRow } from "@vibefield/rosetta-protocol";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const MB = 1_000_000;
const MIB = 1024 * 1024;
/** A tree whose largest source is over this walks the big lane
 * (crates/pipeline/src/closed/walk_order.rs:38). */
export const BIG_SOURCE_BYTES = 64 * MIB;

export interface ColdCorpusOptions {
  /** The import clock's zero, epoch ms. Every source is older. */
  readonly now: number;
  /** 1 is the MacBook shape; a smaller scale keeps every kind of row. */
  readonly scale?: number;
  readonly seed?: number;
}

/** What a session is to the import. */
export type SessionKind =
  /** A top-level conversation with a transcript: flips with its tree. */
  | "root"
  /** A subagent transcript under a root: flips with its tree. */
  | "child"
  /** A metadata-only parent (meta.rs:776): Pending forever, with children. */
  | "placeholder"
  /** Metadata named it, but no streamable source ever rosters it. */
  | "never-rostered-root"
  | "never-rostered-child";

/** One root tree as the walk plan sees it (crates/pipeline/src/closed/trees.rs). */
export interface ColdTree {
  /** The tree's root session id, which is also its key. */
  readonly id: string;
  readonly runtimeId: string;
  /** The rostered sessions that flip together, root first (unless the root
   * is a placeholder, which no tree flip touches). */
  readonly members: readonly string[];
  /** Streamable bytes, the walk's work. */
  readonly bytes: number;
  /** Its largest source exceeds BIG_SOURCE_BYTES: the big lane. */
  readonly big: boolean;
  /** The recency key: the newest member's mtime (trees.rs:193). */
  readonly recency: number;
  /** A bound or tainted tree: admitted at its producer's close, not on its
   * own (CLAUDE.md, "Bound and tainted trees … are admitted at the
   * producer's close"). */
  readonly bound: boolean;
  /** Its root's source was unclean at capture (missing, unstable or a
   * malformed record): the flip makes the root Streaming, not Indexed
   * (crates/pipeline/src/streamed/trees.rs:226-232, `stream_state(clean)`). */
  readonly dirty: boolean;
  readonly placeholderRoot: boolean;
}

export interface ColdCorpus {
  readonly now: number;
  readonly scale: number;
  readonly seed: number;
  /** Registry order: claude-code, then codex. */
  readonly runtimes: readonly RuntimeRow[];
  readonly projects: readonly ProjectRow[];
  /** Identity rows as the metadata pass publishes them: Pending, no activity. */
  readonly sessions: readonly SessionRow[];
  readonly kind: ReadonlyMap<string, SessionKind>;
  /** A rostered session's own recorded bounds, public once its tree flips. */
  readonly own: ReadonlyMap<string, SessionActivity>;
  /** Direct children by parent id. */
  readonly children: ReadonlyMap<string, readonly string[]>;
  /** Newest first. */
  readonly trees: readonly ColdTree[];
  /** Rostered session → its tree. */
  readonly treeOf: ReadonlyMap<string, ColdTree>;
  /** Each rostered session's share of its tree's bytes. */
  readonly bytesOf: ReadonlyMap<string, number>;
  /** The topic word every root title starts with. */
  readonly topics: readonly string[];
}

/* ── seeded helpers ─────────────────────────────────────────────────────── */

/** mulberry32: a small, fast, seeded PRNG. Nothing here reads a clock. */
function random(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4_294_967_296;
  };
}

/** A 64-hex id, the shape of a rosetta id (a blake3 digest): four rounds of
 * a well-mixed 64-bit string hash (cyrb53's mixing). Like the daemon's
 * digests, ordering by it is unrelated to recency or creation order. */
export function hexId(seed: string): string {
  let hex = "";
  for (let round = 0; round < 4; round += 1) {
    let h1 = 0xdeadbeef ^ Math.imul(round + 1, 0x9e3779b1);
    let h2 = 0x41c6ce57 ^ Math.imul(round + 1, 0x85ebca6b);
    for (let i = 0; i < seed.length; i += 1) {
      const ch = seed.charCodeAt(i);
      h1 = Math.imul(h1 ^ ch, 2_654_435_761);
      h2 = Math.imul(h2 ^ ch, 1_597_334_677);
    }
    h1 = Math.imul(h1 ^ (h1 >>> 16), 2_246_822_507) ^ Math.imul(h2 ^ (h2 >>> 13), 3_266_489_909);
    h2 = Math.imul(h2 ^ (h2 >>> 16), 2_246_822_507) ^ Math.imul(h1 ^ (h1 >>> 13), 3_266_489_909);
    hex += (h1 >>> 0).toString(16).padStart(8, "0") + (h2 >>> 0).toString(16).padStart(8, "0");
  }
  return hex;
}

function uuidOf(seed: string): string {
  const h = hexId(`native:${seed}`);
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20, 32)}`;
}

/** A standard normal draw (Box–Muller). */
function normal(rnd: () => number): number {
  return Math.sqrt(-2 * Math.log(1 - rnd())) * Math.cos(2 * Math.PI * rnd());
}

/** Split `total` over `weights` exactly (largest remainder). */
function apportion(total: number, weights: readonly number[]): number[] {
  const sum = weights.reduce((a, b) => a + b, 0) || 1;
  const raw = weights.map((w) => (total * w) / sum);
  const out = raw.map(Math.floor);
  let left = total - out.reduce((a, b) => a + b, 0);
  const order = raw.map((r, i) => [r - Math.floor(r), i] as const).sort((a, b) => b[0] - a[0] || a[1] - b[1]);
  for (let k = 0; left > 0; k = (k + 1) % order.length, left -= 1) out[order[k]![1]]! += 1;
  return out;
}

const TOPICS = [
  "parser", "ledger", "walker", "sidebar", "charts", "search", "renderer", "importer",
  "schema", "deploy", "billing", "theme", "auth", "cache", "worker", "router",
  "fixtures", "metrics", "layout", "sync", "queue", "export", "budget", "lanes",
] as const;
const ACTS = [
  "tidy the walk", "fix the flaky test", "review the diff", "split the module", "trace the stall",
  "draft the plan", "port the screen", "measure the cost", "rename the seam", "check the copy",
] as const;
const PROJECT_WORDS = [
  "rosetta", "vibe-field", "puppeteer", "canvas", "ghosttea", "truffle", "atlas", "mille",
  "harbor", "quill", "lantern", "orbit", "pebble", "meadow", "forge", "signal",
] as const;

/* ── the corpus ─────────────────────────────────────────────────────────── */

interface Draft {
  readonly id: string;
  runtime: number;
  readonly age: number;
  big: boolean;
  bytes: number;
  project: number | null;
  children: number;
  bound: boolean;
  dirty: boolean;
  placeholderRoot: boolean;
}

/** Build the archive. Deterministic in (now, scale, seed); `now` only
 * anchors the times, so every id, count and order is the same in any
 * timezone and on any day. */
export function createColdCorpus(options: ColdCorpusOptions): ColdCorpus {
  const scale = Math.min(1, Math.max(0.01, options.scale ?? 1));
  const seed = options.seed ?? 1;
  const now = options.now;
  const rnd = random(seed * 2_654_435_761);
  const n = (count: number, least: number) => Math.max(least, Math.round(count * scale));

  // The trace's counts, scaled (its census line: roots=1139 sessions=8062
  // big=40; its metadata line: sessions=8612 projects=119).
  const treeCount = n(1139, 12);
  const projectCount = n(119, 4);
  const bigCount = n(40, 2);
  const placeholderTrees = n(25, 1);
  const neverRosteredRoots = n(60, 2);
  const boundTrees = n(8, 1);
  const dirtyTrees = n(5, 1);
  const rostered = Math.max(treeCount + 4, n(8062, 0));
  const metadata = Math.max(rostered + placeholderTrees + neverRosteredRoots, n(8612, 0));
  const neverRosteredChildren = metadata - rostered - placeholderTrees - neverRosteredRoots;
  const codexTrees = Math.max(1, Math.round(treeCount * 0.06));

  const runtimes: RuntimeRow[] = (["claude-code", "codex"] as const).map((kind) => ({
    id: hexId(`runtime:${seed}:${kind}`), kind, data_root: kind === "codex" ? "~/.codex" : "~/.claude",
    enabled: true, account: null, config: null, skills: null, versions_seen: null, discovered_at: now - 2 * HOUR,
  }));

  // Tree ages: a few trees within a day of the newest, a few more within a
  // week, then a long tail over the year (the trace's census ages).
  const recent = n(13, 3);
  const week = n(14, 2);
  const ages: number[] = [];
  for (let i = 0; i < recent; i += 1) ages.push(2 * MINUTE + (i / recent) ** 1.6 * 20 * HOUR + rnd() * 10 * MINUTE);
  for (let i = 0; i < week; i += 1) ages.push(DAY + ((i + rnd()) / week) * 6 * DAY);
  for (let i = recent + week; i < treeCount; i += 1) ages.push(7 * DAY * Math.exp(rnd() * Math.log(60)));
  ages.sort((a, b) => a - b);

  const drafts: Draft[] = ages.map((age, i) => ({
    id: hexId(`tree:${seed}:${i}`), runtime: 0, age, big: false, bytes: 0, project: null,
    children: 0, bound: false, dirty: false, placeholderRoot: false,
  }));
  const pick = (from: number, to: number, ok: (d: Draft) => boolean): Draft | null => {
    for (let tries = 0; tries < 400; tries += 1) {
      const d = drafts[from + Math.floor(rnd() * Math.max(1, to - from))];
      if (d !== undefined && ok(d)) return d;
    }
    return drafts.slice(from, to).find(ok) ?? null;
  };

  // Big trees: the newest is the giant; the recent ones are mostly big
  // (le1h holds 2.5 GB in 23 files); the rest scatter over the tail.
  drafts[0]!.big = true;
  const recentBig = Math.min(recent - 2, n(4, 1));
  const weekBig = Math.min(week - 1, n(6, 1));
  for (let k = 0; k < recentBig; k += 1) pick(1, recent, (d) => !d.big)!.big = true;
  for (let k = 0; k < weekBig; k += 1) pick(recent, recent + week, (d) => !d.big)!.big = true;
  for (let k = 1 + recentBig + weekBig; k < bigCount; k += 1) pick(recent + week, treeCount, (d) => !d.big)!.big = true;

  // Codex trees: a few small ones among claude-code's, never the newest.
  for (let k = 0; k < codexTrees; k += 1) pick(1, treeCount, (d) => d.runtime === 0 && !d.big)!.runtime = 1;

  // Bytes: the giant is the trace's 668 MB newest giant; big trees run
  // 70 MB to 1.7 GB; small trees are log-normal around 45 KB (root_p50).
  for (const d of drafts) {
    d.bytes = d.big
      ? Math.round(70 * MB * Math.exp(rnd() ** 1.8 * Math.log(24)))
      : Math.round(Math.min(60 * MB, Math.max(1_500, 45_000 * Math.exp(2.4 * normal(rnd)))));
  }
  drafts[0]!.bytes = 668_350_304;

  // Projects: the newest work concentrates in a few projects; every
  // project owns at least one tree; ~2% of trees have no proven owner.
  const projectWeight = (p: number) => 1 / (p + 1) ** 0.9;
  const zipf = (limit: number): number => {
    const total = Array.from({ length: limit }, (_, p) => projectWeight(p)).reduce((a, b) => a + b, 0);
    let x = rnd() * total;
    for (let p = 0; p < limit; p += 1) {
      x -= projectWeight(p);
      if (x <= 0) return p;
    }
    return limit - 1;
  };
  drafts.forEach((d, i) => {
    d.project = i === 0 ? 0 : i < recent ? zipf(Math.min(3, projectCount)) : i < recent + week ? zipf(Math.min(10, projectCount)) : zipf(projectCount);
  });
  // The newest project keeps a readable morning beside its giant: its small
  // trees today and this week, whatever the draw gave.
  for (const at of [1, 2, recent]) {
    const d = drafts[at];
    if (d !== undefined && !d.big && d.runtime === 0) d.project = 0;
  }
  const owned = (p: number | null) => drafts.reduce((count, d) => count + (d.project === p ? 1 : 0), 0);
  const tail = drafts.slice(recent + week);
  for (let p = 0; p < projectCount; p += 1) {
    if (owned(p) > 0) continue;
    const d = tail.find((t) => t.project !== 0 && owned(t.project) > 2) ?? tail[p % tail.length]!;
    d.project = p;
  }
  for (let k = 0; k < Math.max(1, Math.round(treeCount * 0.02)); k += 1) {
    const d = pick(recent + week, treeCount, (t) => t.project !== null && owned(t.project) > 2);
    if (d !== null) d.project = null;
  }

  // Bound trees: admitted only at the producer's close. One sits in the
  // newest project, so a reader can open a not-imported row whose tree
  // waits for the close.
  const ownBound = drafts.find((d, i) => i > 2 && d.project === 0 && !d.big && d.runtime === 0);
  if (ownBound !== undefined) ownBound.bound = true;
  for (let k = ownBound === undefined ? 0 : 1; k < boundTrees; k += 1) {
    const d = pick(recent + week, treeCount, (t) => !t.big && !t.bound && t.runtime === 0);
    if (d !== null) d.bound = true;
  }
  // Placeholder roots: claude-code trees whose root transcript is gone. The
  // first is the newest such tree outside the newest project: its family's
  // activity is newer than any unassigned tree's, so publication's reveal
  // (which shows it) moves the unassigned bucket at every scale. The rest
  // are older.
  const placeholderOk = (t: Draft) => !t.big && !t.bound && !t.placeholderRoot && t.runtime === 0 && t.project !== null && t.project !== 0;
  for (let k = 0; k < placeholderTrees; k += 1) {
    const d = (k === 0 ? drafts.slice(1).find(placeholderOk) ?? null : null) ?? pick(recent + week, treeCount, placeholderOk);
    if (d !== null) d.placeholderRoot = true;
  }
  // Dirty trees: the newest project's second-newest small tree (it flips
  // long before the giant, so its Streaming row sits beside the
  // not-imported ones), the rest scattered over the tail.
  const ownDirty = [drafts[2], drafts[1]].find((d) => d !== undefined && !d.big && !d.bound && d.runtime === 0 && d.project === 0);
  if (ownDirty !== undefined) ownDirty.dirty = true;
  for (let k = ownDirty === undefined ? 0 : 1; k < dirtyTrees; k += 1) {
    const d = pick(recent + week, treeCount, (t) => !t.big && !t.bound && !t.dirty && !t.placeholderRoot);
    if (d !== null) d.dirty = true;
  }

  // Children: big trees carry the subagent swarms (one flip of 201 sessions
  // in the trace), small trees a few or none; a placeholder needs one.
  const childTotal = rostered - drafts.filter((d) => !d.placeholderRoot).length;
  const weights = drafts.map((d) => d.big ? 40 + rnd() * 160 : d.runtime === 1 ? 0 : rnd() < 0.5 ? 0 : rnd() < 0.9 ? 1 + rnd() * 3 : 8 + rnd() * 30);
  const counts = apportion(childTotal - placeholderTrees, weights);
  drafts.forEach((d, i) => {
    d.children = Math.min(500, counts[i]! + (d.placeholderRoot ? 1 : 0));
  });
  // What the cap clipped goes to the oldest claude-code trees with room.
  let spill = childTotal - drafts.reduce((a, d) => a + d.children, 0);
  for (let pass = 0; spill > 0 && pass < 500; pass += 1) {
    for (let i = drafts.length - 1; i >= 0 && spill > 0; i -= 1) {
      const d = drafts[i]!;
      if (d.children < 500 && d.runtime === 0) {
        d.children += 1;
        spill -= 1;
      }
    }
  }

  const projectRows: ProjectRow[] = Array.from({ length: projectCount }, (_, p) => {
    const name = `${PROJECT_WORDS[p % PROJECT_WORDS.length]}${p < PROJECT_WORDS.length ? "" : `-${Math.floor(p / PROJECT_WORDS.length) + 1}`}`;
    return { id: hexId(`project:${seed}:${p}`), name, root_paths: [`/Users/demo/Projects/${name}`], git_repo: p % 3 === 0 ? null : `github.com/demo/${name}` };
  });

  const sessions: SessionRow[] = [];
  const kind = new Map<string, SessionKind>();
  const own = new Map<string, SessionActivity>();
  const children = new Map<string, string[]>();
  const trees: ColdTree[] = [];
  const treeOf = new Map<string, ColdTree>();
  const bytesOf = new Map<string, number>();
  const adopt = (parent: string, child: string) => {
    const list = children.get(parent);
    if (list === undefined) children.set(parent, [child]);
    else list.push(child);
  };
  /** Recorded bounds a little before the source's mtime. */
  const bounds = (mtime: number): SessionActivity => {
    const last = mtime - Math.round(rnd() * 50_000) - 1_000;
    return { first_at: last - Math.round(MINUTE + rnd() ** 2 * 6 * HOUR), last_at: last };
  };
  const projectOf = (p: number | null) => (p === null ? null : projectRows[p]!);
  const cwdOf = (p: number | null, i: number) => projectOf(p)?.root_paths[0] ?? `/Users/demo/scratch/run-${i % 40}`;

  drafts.forEach((d, i) => {
    const runtime = runtimes[d.runtime]!;
    const recency = now - Math.round(d.age);
    const project = projectOf(d.project);
    const cwd = cwdOf(d.project, i);
    const topic = TOPICS[Math.floor(rnd() * TOPICS.length)]!;
    const members: string[] = [];
    const memberBytes: number[] = [];
    if (d.placeholderRoot) {
      // The parent row the child's skim made: unassigned, unnamed, Pending.
      sessions.push({
        id: d.id, native_session_id: uuidOf(d.id), runtime_id: runtime.id, project_id: null, cwd: null,
        parent_session_id: null, delegation: null, root_session_id: d.id, role: "main", title: null, summary: null,
        started_at: recency - HOUR, updated_at: recency, activity: null, stream_state: "pending",
      });
      kind.set(d.id, "placeholder");
    } else {
      const activity = bounds(recency);
      sessions.push({
        id: d.id, native_session_id: uuidOf(d.id), runtime_id: runtime.id, project_id: project?.id ?? null, cwd,
        parent_session_id: null, delegation: null, root_session_id: d.id, role: "main",
        title: `${topic}: ${ACTS[Math.floor(rnd() * ACTS.length)]} #${i}`, summary: null,
        started_at: activity.first_at, updated_at: recency, activity: null, stream_state: "pending",
      });
      kind.set(d.id, "root");
      own.set(d.id, activity);
      members.push(d.id);
      memberBytes.push(1 + rnd());
    }
    // Subagents: most under the root, some one level deeper; each wrote a
    // little before the tree's newest file.
    const made: string[] = [];
    for (let c = 0; c < d.children; c += 1) {
      const id = hexId(`child:${seed}:${i}:${c}`);
      const parent = made.length > 0 && rnd() < 0.15 ? made[Math.floor(rnd() * made.length)]! : d.id;
      const mtime = recency - Math.round(rnd() * Math.min(d.age / 3, 6 * HOUR));
      const activity = bounds(mtime);
      sessions.push({
        id, native_session_id: uuidOf(id), runtime_id: runtime.id, project_id: project?.id ?? null, cwd,
        parent_session_id: parent, delegation: null, root_session_id: d.id, role: "subagent",
        title: rnd() < 0.7 ? `${topic} subtask ${c + 1}` : null, summary: null,
        started_at: activity.first_at, updated_at: mtime, activity: null, stream_state: "pending",
      });
      kind.set(id, "child");
      own.set(id, activity);
      adopt(parent, id);
      made.push(id);
      members.push(id);
      memberBytes.push(rnd());
    }
    const tree: ColdTree = {
      id: d.id, runtimeId: runtime.id, members, bytes: d.bytes, big: d.big, recency, bound: d.bound,
      dirty: d.dirty && !d.placeholderRoot, placeholderRoot: d.placeholderRoot,
    };
    apportion(d.bytes, memberBytes).forEach((bytes, m) => {
      bytesOf.set(members[m]!, bytes);
      treeOf.set(members[m]!, tree);
    });
    trees.push(tree);
  });

  // Rows no source rosters: roots spread over the year (one of them in the
  // newest project, this morning) and leaf children under flipping trees.
  for (let k = 0; k < neverRosteredRoots; k += 1) {
    const id = hexId(`unrostered:${seed}:${k}`);
    const p = k === 0 ? 0 : zipf(projectCount);
    const updated = now - Math.round(k === 0 ? 3 * HOUR : 2 * DAY * Math.exp(rnd() * Math.log(150)));
    sessions.push({
      id, native_session_id: uuidOf(id), runtime_id: runtimes[0]!.id, project_id: projectOf(p)!.id, cwd: cwdOf(p, k),
      parent_session_id: null, delegation: null, root_session_id: id, role: "main",
      title: `${TOPICS[k % TOPICS.length]}: an unrostered thread #${k}`, summary: null,
      started_at: updated - HOUR, updated_at: updated, activity: null, stream_state: "pending",
    });
    kind.set(id, "never-rostered-root");
  }
  const hosts = trees.filter((t) => !t.placeholderRoot && t.runtimeId === runtimes[0]!.id);
  const rootRow = new Map(sessions.map((row) => [row.id, row]));
  for (let k = 0; k < neverRosteredChildren; k += 1) {
    const host = hosts[Math.floor(rnd() * hosts.length)]!;
    const id = hexId(`unrostered-child:${seed}:${k}`);
    const root = rootRow.get(host.id)!;
    sessions.push({
      ...root, id, native_session_id: uuidOf(id), parent_session_id: host.id, role: "subagent",
      title: null, summary: null, updated_at: host.recency - HOUR, started_at: host.recency - 2 * HOUR,
    });
    kind.set(id, "never-rostered-child");
    adopt(host.id, id);
  }

  return {
    now, scale, seed, runtimes, projects: projectRows, sessions, kind, own, children, trees, treeOf, bytesOf,
    topics: TOPICS,
  };
}
