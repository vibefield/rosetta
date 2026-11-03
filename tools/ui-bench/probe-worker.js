// ui-bench probe worker (classic worker script; app-entry.mjs injects its text and
// probe.js starts it from a Blob). It parses what the page received, off the main
// thread: /ws/meta frames and /api/status bodies, in receipt order. Ids stay here and
// in the probe's page memory; nothing leaves the page but counts and timings.
//
// In:  {k:'meta', t, d}    one /ws/meta frame (t = page receipt, epoch ms)
//      {k:'status', t, d}  one /api/status body
//      {k:'cfg', giantNative}  the harness's private newest-giant id (D2c), or null
//      {k:'end', daemon}   end-state rows from the harness (roots per project)
//      {k:'parse', ref, d}  one transcript snapshot frame, to time its parse (D3 a)
//      {k:'flush', id}
// Out: {k:'meta', t, kind, bytes, flips:[ids], projFlips:[project keys], retract, unranked}
//      {k:'status', t, phase, canonical, search, run, metaReadyAt, done, total, kinds}
//      {k:'frozen', t, ...}  NEWEST and the RECENT sets, frozen once
//      {k:'horizon', name, t, at, n, excluded}  every member of a frozen set received
//        non-Pending; or, once the import's capture is over (status finalizing,
//        publishing or live), every member that will be: the rest are never-rostered
//      {k:'end', ...} / {k:'flushed', id, ...}
// The count unit excludes metadata-only placeholders (known from the rows) and
// never-rostered roots (known only at the end: still Pending once every runtime is
// readable). The frozen sets keep every candidate; the end state names the members
// still Pending, and the receipt legs are recomputed over the rest.
'use strict';
const HOUR = 3_600_000;
const UNASSIGNED = '__rosetta_unassigned__';
const rows = new Map(); // id -> {p, par, s, u, own, g}: own = has a cwd, title, summary or delegation
const parents = new Set();
const firstReady = new Map(); // id -> first receipt that showed it non-pending
const kinds = new Map(); // runtime id -> kind
let frozen = null;
let giantNative = null;
let giantId = null;
let lastPhase = null;
let parseMs = 0;
let counts = { frames: 0, snapshots: 0, mutations: 0, resyncs: 0, hellos: 0, other: 0, bytes: 0, retractions: 0, statusBodies: 0, statusBad: 0 };
// D2(b), corpus-wide: integral of RECENT24 roots pending while an older root of their
// project is ready, over receipt time.
let inversionMs = 0;
let inversionNow = 0;
let inversionAt = null;

const projectKey = (r) => (r.p === null ? UNASSIGNED : r.p);
const pending = (r) => r.s === 'pending';
const metadataOnly = (id, r) => parents.has(id) && r.s === 'pending' && !r.own;
const isRoot = (r) => r.par === null;

// Only what the metrics need: no title, cwd or native id is kept, just whether the
// row carries anything of its own (metadataOnly) and whether it is the giant (D2 c).
function compact(row) {
  return { p: row.project_id ?? null, par: row.parent_session_id ?? null, s: row.stream_state,
    u: Number(row.updated_at) || 0,
    own: row.cwd != null || row.title != null || row.summary != null || row.delegation != null,
    g: giantNative !== null && row.native_session_id === giantNative };
}

function rebuildParents() {
  parents.clear();
  for (const r of rows.values()) if (r.par !== null) parents.add(r.par);
}

function unrankedProjects() {
  // A project ranks once one of its roots carries activity, which Pending withholds.
  const ready = new Map();
  for (const r of rows.values()) {
    if (!isRoot(r)) continue;
    const key = projectKey(r);
    if (!ready.has(key)) ready.set(key, false);
    if (!pending(r)) ready.set(key, true);
  }
  let n = 0;
  for (const v of ready.values()) if (!v) n += 1;
  return n;
}

// Roots that are not metadata-only placeholders: ready ones, and all of them.
function rootCounts() {
  let ready = 0;
  let total = 0;
  for (const [id, r] of rows) {
    if (!isRoot(r) || metadataOnly(id, r)) continue;
    total += 1;
    if (!pending(r)) ready += 1;
  }
  return [ready, total];
}

function inversionCount() {
  if (frozen === null) return 0;
  let n = 0;
  const oldestReady = new Map(); // project -> smallest updated_at among ready roots
  for (const r of rows.values()) {
    if (!isRoot(r) || pending(r)) continue;
    const key = projectKey(r);
    const cur = oldestReady.get(key);
    if (cur === undefined || r.u < cur) oldestReady.set(key, r.u);
  }
  for (const id of frozen.r24) {
    const r = rows.get(id);
    if (r === undefined || !pending(r)) continue;
    const older = oldestReady.get(projectKey(r));
    if (older !== undefined && older < r.u) n += 1;
  }
  return n;
}

function advanceInversion(t) {
  if (inversionAt !== null) inversionMs += inversionNow * Math.max(0, t - inversionAt);
  inversionAt = t;
  inversionNow = inversionCount();
}

function onMeta(m) {
  counts.frames += 1;
  counts.bytes += m.d.length;
  const f = JSON.parse(m.d);
  const flips = [];
  let kind = 'other';
  let retract = 0;
  const touch = (id, row) => {
    const before = rows.get(id);
    const next = compact(row);
    rows.set(id, next);
    if (next.par !== null) parents.add(next.par);
    if (next.s !== 'pending') {
      if (!firstReady.has(id)) {
        firstReady.set(id, m.t);
        flips.push(id);
      }
    } else if (before !== undefined && before.s !== 'pending') {
      retract += 1;
    }
    if (giantId === null && next.g) giantId = id;
  };
  if (f.frame === 'meta_snapshot') {
    kind = 'snapshot';
    counts.snapshots += 1;
    const seen = new Set();
    for (const rt of f.runtimes || []) kinds.set(rt.id, rt.kind);
    for (const row of f.sessions || []) { seen.add(row.id); touch(row.id, row); }
    for (const id of [...rows.keys()]) if (!seen.has(id)) rows.delete(id);
    rebuildParents();
  } else if (f.frame === 'mutation') {
    kind = 'mutation';
    counts.mutations += 1;
    const mu = f.mutation || {};
    if (mu.type === 'session') touch(mu.id, mu);
    else if (mu.type === 'runtime') kinds.set(mu.id, mu.kind);
  } else if (f.frame === 'resync') {
    kind = 'resync';
    counts.resyncs += 1;
  } else if (f.frame === 'hello') {
    kind = 'hello';
    counts.hellos += 1;
  } else {
    counts.other += 1;
  }
  counts.retractions += retract;
  const projFlips = [];
  for (const id of flips) {
    const r = rows.get(id);
    if (r !== undefined && isRoot(r)) projFlips.push(projectKey(r));
  }
  const changed = flips.length > 0 || kind === 'snapshot';
  if (changed) advanceInversion(m.t);
  const [readyRoots, totalRoots] = changed ? rootCounts() : [null, null];
  const out = { k: 'meta', t: m.t, kind, bytes: m.d.length, flips, projFlips, retract,
    unranked: changed ? unrankedProjects() : null, readyRoots, totalRoots,
    hasProject: Array.isArray(f.projects) ? f.projects.length > 0 : (f.mutation && f.mutation.type === 'project') === true,
    giantId };
  postMessage(out);
  if (frozen !== null) horizons(m.t, false);
}

// NEWEST: the root with the max updated_at, not metadata-only; ties to the smallest id.
// RECENT24/RECENT7: roots within 24 h / 7 d of that max (the @newest anchor), the same
// exclusion. Frozen once, at the first status body that carries metadata_ready_at.
function freeze(t) {
  let newest = null;
  for (const [id, r] of rows) {
    if (!isRoot(r) || metadataOnly(id, r)) continue;
    if (newest === null || r.u > newest.u || (r.u === newest.u && id < newest.id)) newest = { id, u: r.u };
  }
  const r24 = [];
  const r7 = [];
  let roots = 0;
  let metaOnly = 0;
  if (newest !== null) {
    for (const [id, r] of rows) {
      if (!isRoot(r)) continue;
      if (metadataOnly(id, r)) { metaOnly += 1; continue; }
      roots += 1;
      if (r.u >= newest.u - 24 * HOUR) r24.push(id);
      if (r.u >= newest.u - 7 * 24 * HOUR) r7.push(id);
    }
  }
  const newestProject = newest === null ? null : projectKey(rows.get(newest.id));
  const projectRoots = {};
  if (newestProject !== null) {
    for (const [id, r] of rows) if (isRoot(r) && projectKey(r) === newestProject) projectRoots[id] = r.u;
  }
  frozen = { t, newest: newest?.id ?? null, newestProject, r24, r7, roots, metaOnly, projectRoots,
    r24Set: new Set(r24), r7Set: new Set(r7) };
  postMessage({ k: 'frozen', t, newest: frozen.newest, newestProject, r24, r7, roots, metaOnly,
    projectRoots, newestReadyAt: newest === null ? null : (firstReady.get(newest.id) ?? null) });
  advanceInversion(t);
  horizons(t, lastPhase === 'finalizing' || lastPhase === 'publishing' || lastPhase === 'live');
}

// The last receipt over the members received non-Pending, the count received, the
// count still Pending. An empty set has no receipt (null), never "now".
function legOf(list) {
  let last = null;
  let n = 0;
  let pendingN = 0;
  for (const id of list) {
    const at = firstReady.get(id);
    if (at === undefined) { pendingN += 1; continue; }
    n += 1;
    if (last === null || at > last) last = at;
  }
  return { at: last, n, pending: pendingN };
}

const horizonSent = { h24: false, h7d: false };
function horizons(t, captureOver) {
  for (const [name, list] of [['h24', frozen.r24], ['h7d', frozen.r7]]) {
    if (horizonSent[name]) continue;
    const leg = legOf(list);
    if (leg.pending === 0 || captureOver) {
      horizonSent[name] = true;
      postMessage({ k: 'horizon', name, t, at: leg.at, n: leg.n, excluded: leg.pending, viaCaptureOver: leg.pending > 0 });
    }
  }
}

function onStatus(m) {
  counts.statusBodies += 1;
  let s;
  try { s = JSON.parse(m.d); } catch { counts.statusBad += 1; return; }
  const ingest = s.daemon?.ingest ?? null;
  const projects = ingest?.projects ?? [];
  let done = 0;
  let total = 0;
  for (const p of projects) { done += Number(p.sessions_done) || 0; total += Number(p.sessions_total) || 0; }
  const phase = ingest?.phase ?? null;
  lastPhase = phase;
  const metaReadyAt = ingest?.metadata_ready_at ?? null;
  postMessage({ k: 'status', t: m.t, phase, canonical: s.canonical_ready === true,
    search: s.search?.readiness ?? null, run: ingest?.run?.state ?? null, metaReadyAt, done, total,
    kinds: (s.daemon?.runtimes ?? []).map((r) => r.kind).sort(),
    ingestKinds: (ingest?.runtimes ?? []).map((r) => r.kind).sort() });
  if (frozen === null && metaReadyAt !== null && phase !== 'awaiting_consent') freeze(m.t);
  else if (frozen !== null && (phase === 'finalizing' || phase === 'publishing' || phase === 'live')) horizons(m.t, true);
}

function endState(daemon) {
  // daemon: {roots: [[id, project key, stream_state]], truncated} from /api/sessions after the run.
  let pendingRoots = 0;
  let pendingSessions = 0;
  let metaOnlyRoots = 0;
  let neverRostered = 0;
  let rootsSeen = 0;
  let readyAll = 0;
  let lastRootReady = null;
  const perProject = new Map();
  const stillPending = new Set(); // never-rostered roots, when the daemon's count agrees (I1 c5)
  for (const [id, r] of rows) {
    if (!pending(r)) readyAll += 1; else pendingSessions += 1;
    if (!isRoot(r)) continue;
    rootsSeen += 1;
    const key = projectKey(r);
    perProject.set(key, (perProject.get(key) ?? 0) + 1);
    if (metadataOnly(id, r)) { metaOnlyRoots += 1; continue; }
    if (pending(r)) { pendingRoots += 1; neverRostered += 1; stillPending.add(id); continue; }
    const at = firstReady.get(id);
    if (at !== undefined && (lastRootReady === null || at > lastRootReady)) lastRootReady = at;
  }
  const daemonPer = new Map();
  let daemonPending = 0;
  for (const [, key, state] of daemon?.roots ?? []) {
    daemonPer.set(key, (daemonPer.get(key) ?? 0) + 1);
    if (state === 'pending') daemonPending += 1;
  }
  let projectMismatches = 0;
  for (const key of new Set([...perProject.keys(), ...daemonPer.keys()])) {
    if ((perProject.get(key) ?? 0) !== (daemonPer.get(key) ?? 0)) projectMismatches += 1;
  }
  const newestProjectRoots = frozen?.newestProject == null ? null : (perProject.get(frozen.newestProject) ?? 0);
  const newestProjectDaemonRoots = frozen?.newestProject == null ? null : (daemonPer.get(frozen.newestProject) ?? 0);
  // Membership stability: recompute NEWEST and the sets from the final rows.
  let stable = null;
  // The count unit at the end: the frozen sets less their never-rostered members, the
  // receipt legs over the rest, and NEWEST replaced by the newest root that was
  // rostered if NEWEST itself never was. Ids stay in page memory (eff).
  let eff = null;
  if (frozen !== null) {
    const keep = { newest: frozen.newest, r24: frozen.r24.slice().sort().join(','), r7: frozen.r7.slice().sort().join(',') };
    let n = null;
    for (const [id, r] of rows) {
      if (!isRoot(r) || metadataOnly(id, r)) continue;
      if (n === null || r.u > n.u || (r.u === n.u && id < n.id)) n = { id, u: r.u };
    }
    const r24 = [];
    const r7 = [];
    for (const [id, r] of rows) {
      if (!isRoot(r) || metadataOnly(id, r) || n === null) continue;
      if (r.u >= n.u - 24 * HOUR) r24.push(id);
      if (r.u >= n.u - 7 * 24 * HOUR) r7.push(id);
    }
    stable = (n?.id ?? null) === keep.newest && r24.sort().join(',') === keep.r24 && r7.sort().join(',') === keep.r7;
    const r24Eff = frozen.r24.filter((id) => !stillPending.has(id));
    const r7Eff = frozen.r7.filter((id) => !stillPending.has(id));
    let newestEff = frozen.newest;
    if (newestEff !== null && stillPending.has(newestEff)) {
      newestEff = null;
      let best = null;
      for (const [id, r] of rows) {
        if (!isRoot(r) || metadataOnly(id, r) || stillPending.has(id)) continue;
        if (best === null || r.u > best.u || (r.u === best.u && id < best.id)) best = { id, u: r.u };
      }
      newestEff = best?.id ?? null;
    }
    const projectEff = Object.keys(frozen.projectRoots).filter((id) => !stillPending.has(id) && !(rows.has(id) && metadataOnly(id, rows.get(id))));
    const l24 = legOf(r24Eff);
    const l7 = legOf(r7Eff);
    eff = { newest: newestEff, newestExcluded: newestEff !== frozen.newest, newestReceiptAt: newestEff === null ? null : (firstReady.get(newestEff) ?? null),
      r24: r24Eff, r7: r7Eff, projectRoots: projectEff, h24At: l24.at, h7dAt: l7.at, n24: r24Eff.length, n7d: r7Eff.length,
      excluded24: frozen.r24.length - r24Eff.length, excluded7d: frozen.r7.length - r7Eff.length,
      excludedProject: Object.keys(frozen.projectRoots).filter((id) => stillPending.has(id)).length };
  }
  return { rootsSeen, readyAll, sessionsSeen: rows.size, pendingSessions, pendingRoots, metaOnlyRoots, neverRostered,
    lastRootReady, daemonRoots: daemon?.roots?.length ?? null, daemonPending,
    daemonTruncated: daemon?.truncated === true, projectMismatches, newestProjectRoots,
    newestProjectDaemonRoots, membershipStable: stable, eff };
}

onmessage = (event) => {
  const m = event.data;
  const a = performance.now();
  try {
    if (m.k === 'meta') onMeta(m);
    else if (m.k === 'status') onStatus(m);
    else if (m.k === 'cfg') giantNative = m.giantNative ?? null;
    else if (m.k === 'parse') {
      // D3(a): what parsing one transcript snapshot costs, measured here, never on
      // the page's main thread.
      const b = performance.now();
      JSON.parse(m.d);
      postMessage({ k: 'parsed', ref: m.ref, ms: performance.now() - b });
    }
    else if (m.k === 'end') {
      advanceInversion(m.t);
      postMessage({ k: 'end', id: m.id, ...endState(m.daemon), inversionMs, counts, parseMs });
    } else if (m.k === 'flush') postMessage({ k: 'flushed', id: m.id, counts, parseMs, inversionMs });
  } catch {
    postMessage({ k: 'error' });
  }
  parseMs += performance.now() - a;
};
