// ui-run/1 metrics: derived from the probe's export (renderer clock), the Electron
// main process's notes (main clock) and the parsed daemon lines (daemon clock), all
// mapped onto ONE import clock whose zero is the daemon's `milestone:
// consent-granted`. Pure and deterministic; definitions and roles are those of
// docs/ARCHITECTURE.md, "Cold-build measurement", UI readiness. Every metric object
// carries a role: claim-candidate, guard, invariant or report-only (a nested object
// may carry its own), and a `reached` map: for each value a pair compares, `yes`,
// `not-reached` (the state never happened in this run, so the value is null) or
// `not-applicable` (the script declares it never exercises it, or the frozen set it
// measures is empty). A value whose precondition never happened is null, never 0.
import { createHash } from 'node:crypto';
import { allReadableS } from './daemon-lines.mjs';

export const ENROLLED = ['claude-code', 'codex'];
const LIST_AIMS = new Set(['newest-project', 'other-project', 'newest-row', 'recent-next', 'ready-next', 'pending-row']);
const OFFSET_APPLY_MS = 2;

export function pct(xs, p) {
  const v = xs.filter((x) => Number.isFinite(x)).sort((a, b) => a - b);
  if (!v.length) return null;
  return v[Math.min(v.length - 1, Math.max(0, Math.ceil((p / 100) * v.length) - 1))];
}
const r1 = (x) => (x === null || x === undefined || !Number.isFinite(x) ? null : Math.round(x * 10) / 10);
const r3 = (x) => (x === null || x === undefined || !Number.isFinite(x) ? null : Math.round(x * 1000) / 1000);
const dist = (xs) => ({ n: xs.filter(Number.isFinite).length, p50Ms: r1(pct(xs, 50)), p95Ms: r1(pct(xs, 95)), maxMs: r1(pct(xs, 100)) });
const sub = (a, b) => (a === null || a === undefined || b === null || b === undefined ? null : a - b);

/** The clocks: daemon elapsed → wall, renderer and main → driver (wall), and T0. */
export function alignClocks({ daemon, probe, main, mainPings }) {
  let base = null;
  let method = 'none';
  if (daemon.clockWallMs !== null) { base = daemon.clockWallMs; method = 'clock-line'; }
  else {
    const mr = daemon.milestones['metadata-ready'];
    const at = probe?.status?.metaReadyAtWall ?? null;
    if (mr && at !== null) { base = at - mr.elapsedS * 1000; method = 'metadata-ready-anchor'; }
  }
  const pick = (a, b) => [a, b].filter((x) => x && Number.isFinite(x.offsetMs)).sort((x, y) => x.rttMs - y.rttMs)[0] ?? null;
  const rStart = main?.offsets?.start ?? null;
  const rEnd = main?.offsets?.end ?? null;
  const rBest = pick(rStart, rEnd);
  const mBest = pick(mainPings?.start ?? null, mainPings?.end ?? null);
  const rOff = rBest?.offsetMs ?? 0;
  const mOff = mBest?.offsetMs ?? 0;
  const applyR = Math.abs(rOff) > OFFSET_APPLY_MS;
  const applyM = Math.abs(mOff) > OFFSET_APPLY_MS;
  const fromRenderer = (t) => (t === null || t === undefined ? null : t - (applyR ? rOff : 0) - (applyM ? mOff : 0));
  const fromMain = (t) => (t === null || t === undefined ? null : t - (applyM ? mOff : 0));
  const consent = daemon.milestones['consent-granted'];
  let t0 = null;
  let t0Kind = 'none';
  if (base !== null && consent && consent.elapsedS !== null) { t0 = base + consent.elapsedS * 1000; t0Kind = 'consent-granted'; }
  else if (probe?.d1?.beginAt != null) { t0 = fromRenderer(probe.d1.beginAt); t0Kind = 'begin-click'; }
  const daemonWall = (s) => (base === null || s === null || s === undefined ? null : base + s * 1000);
  const drift = (a, b) => (a && b && Number.isFinite(a.offsetMs) && Number.isFinite(b.offsetMs) ? Math.abs(a.offsetMs - b.offsetMs) : null);
  const errMs = (rBest ? rBest.rttMs / 2 : 0) + (mBest ? mBest.rttMs / 2 : 0) + (method === 'clock-line' ? 1 : 10);
  return {
    t0, base, fromRenderer, fromMain, daemonWall,
    imp: (wall) => (wall === null || t0 === null ? null : wall - t0),
    report: {
      method, t0Kind, rendererOffsetMs: r3(rOff), rendererRttMs: r3(rBest?.rttMs ?? null), rendererOffsetApplied: applyR,
      mainOffsetMs: r3(mOff), mainRttMs: r3(mBest?.rttMs ?? null), mainOffsetApplied: applyM,
      rendererDriftMs: r3(drift(rStart, rEnd)), mainDriftMs: r3(drift(mainPings?.start, mainPings?.end)),
      roundedMilestones: Object.values(daemon.milestones).filter((v) => v.precise === false).length,
      // Drain receipt − the daemon's own unrounded stamp: never below 0 if the mapping is right.
      daemonReceiptLagMs: { min: r3(pct(daemon.receiptLagMs, 0.0001)), p50: r3(pct(daemon.receiptLagMs, 50)), n: daemon.receiptLagMs.length },
      estimatedErrorMs: r3(errMs + Math.max(0, -(pct(daemon.receiptLagMs, 0.0001) ?? 0))),
    },
  };
}

function windows(clock, daemon) {
  const ms = (s) => clock.imp(clock.daemonWall(s));
  const allR = ms(allReadableS(daemon));
  const cr = ms(daemon.milestones['canonical-ready']?.elapsedS ?? null);
  const sr = ms((daemon.milestones['search-ready'] ?? daemon.milestones['search-failed'])?.elapsedS ?? null);
  return { streaming: [0, allR], finalizing: [allR, cr], post: [cr, sr === null ? null : sr + 5000] };
}

function h6(clock, probe, main, daemon, wallMs) {
  const w = windows(clock, daemon);
  const loaf = (probe.loaf || []).map(([t, d, b]) => [clock.imp(clock.fromRenderer(t)), d, b]);
  const byWindow = {};
  for (const [name, [a, b]] of Object.entries(w)) {
    if (a === null || b === null || b < a) { byWindow[name] = { wallMs: null, longestMs: null, blockingMs: null, blockingPct: null, n100: null, nLoaf: null }; continue; }
    const xs = loaf.filter(([t]) => t !== null && t >= a && t < b);
    const blocking = xs.reduce((s, x) => s + x[2], 0);
    byWindow[name] = { wallMs: r1(b - a), longestMs: r1(xs.length ? Math.max(...xs.map((x) => x[1])) : 0), blockingMs: r1(blocking),
      blockingPct: r1(b > a ? (100 * blocking) / (b - a) : null), n100: xs.filter((x) => x[1] >= 100).length, nLoaf: xs.length };
  }
  // Interactions: the longest event per interaction id; scripted ones under 16 ms are
  // not reported by Event Timing, so they count as 16 (an upper bound) in the p98.
  const per = new Map();
  for (const [, d, , id] of probe.events || []) if (id) per.set(id, Math.max(per.get(id) ?? 0, d));
  const scripted = (main?.inputs || []).filter(([, k]) => k === 'down' || k === 'key' || k === 'shortcut').length;
  const reported = [...per.values()];
  const padded = reported.concat(Array(Math.max(0, scripted - reported.length)).fill(16));
  const self = probe.cost || {};
  return {
    role: 'guard', loafThresholdMs: 50, byWindow,
    input: { role: 'guard', scripted, reported: reported.length, p98Ms: r1(pct(padded, 98)), maxMs: r1(padded.length ? Math.max(...padded) : null), unreportedAsMs: 16 },
    report: {
      // probeMainMs is the probe's own JavaScript self-time on the page's main thread
      // (handlers, frame passes, structured-clone posts, the reader's queries), not
      // the browser's observer bookkeeping or the debugger's own traffic.
      role: 'report-only', heapPeakMb: r1(main?.heap?.peakMb ?? null), appWorkingSetPeakMb: r1(main?.heap?.appPeakMb ?? null),
      probeCostKind: 'js-self-time', probeMainMs: r1(self.mainMs ?? null), probePostMs: r1(self.postMs ?? null), probePostMb: r1((self.postBytes ?? 0) / 2 ** 20),
      probePasses: self.passes ?? null, probeErrors: self.errors ?? null,
      probeMainPctOfWall: r1(wallMs ? (100 * (self.mainMs ?? 0)) / wallMs : null), probeBudgetPct: 1,
      probeWithinBudget: wallMs ? (100 * (self.mainMs ?? 0)) / wallMs <= 1 : null, workerParseMs: r1(probe.end?.workerParseMs ?? null),
    },
  };
}

function hitDigest(search) {
  const parts = (search || []).map((s) => `${s.q}:${s.digest ?? 'none'}`);
  return parts.length ? createHash('sha256').update(parts.join('\n')).digest('hex') : null;
}

function readyCountLag(clock, probe) {
  const flips = (probe.meta?.flipFrames || []).filter((t) => probe.d1?.beginAt == null || t >= probe.d1.beginAt);
  const counts = probe.readyCount || [];
  const lags = [];
  for (const f of flips) {
    let prev = null;
    for (const [t, n] of counts) {
      if (t <= f) { prev = n; continue; }
      if (prev === null || n > prev) { lags.push(t - f); break; }
      prev = n;
    }
  }
  return lags;
}

function countUnit(probe) {
  const out = {};
  const rr = probe.readyRoots || [];
  const rc = probe.readyCount || [];
  for (const f of [25, 50, 100]) {
    const hit = rr.find(([, ready, total]) => total > 0 && ready / total >= f / 100);
    if (!hit) { out[`at${f}`] = null; continue; }
    const shown = rc.filter(([t]) => t <= hit[0] + 1000).pop();
    out[`at${f}`] = shown && hit[1] > 0 ? Math.round((shown[1] / hit[1]) * 1000) / 1000 : null;
  }
  return out;
}

function wireLeg(clock, daemon, probe) {
  const flips = (probe.meta?.flipFrames || []).map((t) => clock.fromRenderer(t)).sort((a, b) => a - b);
  const batches = daemon.treeAdmissions.filter((b) => b.publishedS !== null && (b.sessions ?? 0) > 0)
    .map((b) => clock.daemonWall(b.publishedS)).filter((x) => x !== null).sort((a, b) => a - b);
  const legs = [];
  batches.forEach((at, i) => {
    const next = batches[i + 1] ?? Infinity;
    const f = flips.find((x) => x >= at - 1);
    if (f !== undefined && f < next) legs.push(f - at);
  });
  return legs;
}

/** reached: `not-applicable` when the script declares the value never exercised (or
 * its measured set is empty), else `yes` when the value exists, else `not-reached`. */
function reacher(notApplicable) {
  const na = new Set(notApplicable || []);
  return (key, value, { empty = false } = {}) => {
    if (na.has(key) || empty) return 'not-applicable';
    return value === null || value === undefined || value === false || Number.isNaN(value) ? 'not-reached' : 'yes';
  };
}

export function derive({ probe, main, daemon, mainPings, notApplicable = [] }) {
  // The probe's budget is against the page's own life: first frame to its export.
  const wallMs = probe.finishedAt != null && probe.page?.firstFrameAt != null ? probe.finishedAt - probe.page.firstFrameAt : null;
  const clock = alignClocks({ daemon, probe, main, mainPings });
  const R = (t) => clock.imp(clock.fromRenderer(t));
  const D = (key) => { const m = daemon.milestones[key]; return m ? clock.imp(clock.daemonWall(m.elapsedS)) : null; };
  const lag = (uiImp, key) => sub(uiImp, D(key));
  const reach = reacher(notApplicable);
  const m = {};
  const metaReadyMs = D('metadata-ready');
  const colsUp = probe.h1?.paintAt != null;

  // H1 — projects on screen; split into the meta-push and the status-poll parts.
  const h1 = R(probe.h1?.paintAt ?? null);
  m.H1 = { role: 'guard', ms: r1(h1), projects: probe.h1?.projects ?? null,
    metaPushMs: r1(R(probe.meta?.firstProjectFrameAt ?? null)), statusPollMs: r1(R(probe.status?.firstNonConsentAt ?? null)),
    lagVsMetadataReadyMs: r1(sub(h1, metaReadyMs)), reached: { ms: reach('H1.ms', h1) } };

  // H2 — NEWEST ready on screen (painted), with its receipt leg, lag and hold. The
  // paint lag runs from the later of the flip's receipt and NEWEST's first draw: a
  // row that flipped before it was ever drawn has nothing to lag behind until it is.
  const h2 = probe.h2 || {};
  const h2paint = R(h2.paintReadyAt ?? null);
  const h2recv = R(h2.receiptAt ?? null);
  const h2draw = R(h2.firstDrawAt ?? null);
  const flippedBeforeDrawn = h2recv !== null && h2draw !== null ? h2recv < h2draw : null;
  m.H2 = { role: 'claim-candidate', ms: r1(h2paint), receiptMs: r1(h2recv), heldReadyMs: h2draw === null ? null : r1(h2.heldReadyMs ?? null),
    firstDrawMs: r1(h2draw), firstDrawStanding: h2.firstDrawStanding ?? 'none',
    indexAtFirstDraw: h2.indexAtFirstDraw ?? null, flippedBeforeDrawn, newestExcluded: h2.newestExcluded === true,
    lag: { role: 'guard', ms: r1(h2paint === null || h2recv === null ? null : h2paint - Math.max(h2recv, h2draw ?? -Infinity)), rawMs: r1(sub(h2paint, h2recv)) },
    reached: { ms: reach('H2.ms', h2paint), receiptMs: reach('H2.receiptMs', h2recv) } };

  // H3 — last 24 h / 7 d @newest, roots: receipt leg and painted-in-NEWEST's-project leg,
  // over the frozen sets less their never-rostered members.
  const h3reached = {};
  const hz = (name, key) => {
    const recv = R(probe.h3?.[`${name}ReceiptAt`] ?? null);
    const paint = R(probe.h3?.[`${name}PaintSelAt`] ?? null);
    const d = daemon.milestones[key];
    const roots = probe.h3?.[name === 'h24' ? 'n24' : 'n7d'] ?? null;
    const inSel = probe.h3?.[name === 'h24' ? 'n24Sel' : 'n7dSel'] ?? null;
    h3reached[`${name}.receiptMs`] = reach(`H3.${name}.receiptMs`, recv, { empty: roots === 0 });
    h3reached[`${name}.paintedSelMs`] = reach(`H3.${name}.paintedSelMs`, paint, { empty: inSel === 0 });
    return { receiptMs: r1(recv), paintedSelMs: r1(paint), roots, rootsInSelected: inSel,
      neverRosteredExcluded: probe.h3?.[name === 'h24' ? 'excluded24' : 'excluded7d'] ?? null, viaCaptureOver: probe.h3?.[`${name}ViaCaptureOver`] === true,
      daemonHorizonSessions: d?.horizonSessions ?? null, daemonMs: r1(D(key)), lagVsDaemonMs: r1(lag(recv, key)) };
  };
  m.H3 = { role: 'claim-candidate', h24: hz('h24', 'recent-ready:24h@newest'), h7d: hz('h7d', 'recent-ready:7d@newest'), reached: h3reached };

  // H-all — every readable root ready (metadata-only and never-rostered excluded).
  const allD = clock.imp(clock.daemonWall(allReadableS(daemon)));
  const allRecv = R(probe.end?.lastRootReady ?? null);
  const allPaint = R(probe.hall?.paintSelAt ?? null);
  m.Hall = { role: 'claim-candidate', receiptMs: r1(allRecv), paintedSelMs: r1(allPaint), daemonMs: r1(allD),
    lagVsDaemonMs: r1(sub(allRecv, allD)), neverRostered: probe.end?.neverRostered ?? null, metadataOnlyRoots: probe.end?.metaOnlyRoots ?? null,
    reached: { receiptMs: reach('Hall.receiptMs', allRecv), paintedSelMs: reach('Hall.paintedSelMs', allPaint) } };

  // H4 — click → first own content, by phase; the opened-while-Pending face beside it.
  const opens = probe.opens || [];
  const bucket = (o) => (o.canonical ? 'postCanonical' : o.phase === 'finalizing' || o.phase === 'publishing' ? 'finalizing' : 'reading');
  const ready = opens.filter((o) => o.standing === 'ready');
  const ms4 = (o) => sub(o.contentAt, o.clickAt);
  const byPhase = {};
  for (const b of ['reading', 'finalizing', 'postCanonical']) {
    const xs = ready.filter((o) => bucket(o) === b).map(ms4);
    byPhase[b] = { n: xs.filter((x) => x !== null).length, p50Ms: r1(pct(xs.filter((x) => x !== null), 50)), maxMs: r1(pct(xs.filter((x) => x !== null), 100)) };
  }
  const all4 = ready.map(ms4).filter((x) => x !== null);
  const pend = opens.filter((o) => o.standing === 'not-imported');
  const faces = pend.map((o) => sub(o.faceAt, o.clickAt)).filter((x) => x !== null);
  m.H4 = { role: 'guard', n: all4.length, unresolved: ready.length - all4.length, p50Ms: r1(pct(all4, 50)), maxMs: r1(pct(all4, 100)), byPhase,
    pendingFace: { role: 'guard', n: faces.length, p50Ms: r1(pct(faces, 50)), maxMs: r1(pct(faces, 100)) },
    reached: { samples: reach('H4.samples', all4.length > 0), pendingFace: reach('H4.pendingFace', faces.length > 0) } };

  // H5 — receipt → paint per flipped root drawn while it flipped.
  const h5 = (probe.h5 || []).map(([a, b]) => b - a);
  m.H5 = { role: 'guard', ...dist(h5), reached: { samples: reach('H5.samples', h5.length > 0) } };

  // H6 — responsiveness by import window; memory and probe cost report-only. Without
  // its observers (self-test) the frame and input values are null, never 0.
  const observed = probe.selfTest?.loaf === true && probe.selfTest?.eventTiming === true;
  m.H6 = h6(clock, probe, main, daemon, wallMs);
  if (!observed) {
    for (const w of Object.values(m.H6.byWindow)) for (const k of Object.keys(w)) w[k] = null;
    Object.assign(m.H6.input, { p98Ms: null, maxMs: null });
  }
  m.H6.reached = { longestFrame: reach('H6.longestFrame', observed && m.H6.byWindow.streaming.longestMs !== null),
    input: reach('H6.input', observed && m.H6.input.scripted > 0) };

  // H7 — list stability; selection jumps and aim misses are absolute-zero invariants
  // (H7.invariant.pass). Moves and jumps are reported under both definitions of
  // "unrequested" (any input incl. pointer motion — gated — and discrete input only).
  const aims = (probe.h7?.aims || []).filter(([, kind]) => LIST_AIMS.has(kind));
  const missed = aims.filter(([, , hit]) => hit === 0);
  const P = probe.h7 || {};
  const n7 = (x) => (colsUp ? x ?? null : null);
  const jumps = n7(P.selectionJumps);
  m.H7 = { role: 'guard', projectsMoves: n7(P.projects?.moves), projectsPx: r1(n7(P.projects?.px)),
    sessionsMoves: n7(P.sessions?.moves), sessionsPx: r1(n7(P.sessions?.px)),
    discreteInput: { role: 'report-only', projectsMoves: n7(P.projects?.movesDiscrete), projectsPx: r1(n7(P.projects?.pxDiscrete)),
      sessionsMoves: n7(P.sessions?.movesDiscrete), sessionsPx: r1(n7(P.sessions?.pxDiscrete)), selectionJumps: n7(P.selectionJumpsDiscrete) },
    heldMs: { projects: r1(n7(P.heldMs?.projects)), sessions: r1(n7(P.heldMs?.sessions)) },
    pillShown: { projects: n7(P.pillShown?.projects), sessions: n7(P.pillShown?.sessions) },
    layoutShift: probe.selfTest?.layoutShift === true ? r1((probe.layoutShift?.sum ?? 0) * 1000) / 1000 : null,
    layoutShiftNoInput: probe.selfTest?.layoutShift === true ? r1((probe.layoutShift?.sumNoInput ?? 0) * 1000) / 1000 : null,
    invariant: { role: 'invariant', pass: jumps === 0 && missed.length === 0, selectionJumps: jumps, aims: aims.length, aimMisses: missed.length,
      missedOtherRow: missed.filter((x) => x[4] === 'other-row').length, missedNoRow: missed.filter((x) => x[4] === 'none').length,
      missedTargetShiftPx: missed.map((x) => x[3] ?? null) },
    reached: { moves: reach('H7.moves', colsUp), aims: reach('H7.aims', aims.length > 0) } };

  // H8 — charts (selected, visible, all, portfolio) and search; UI hit digest. The
  // search guard runs from the later of the UI's own search-ready receipt (the first
  // status saying so) and the query's last key, so it times the app, not the
  // script's queue; the first hit on the import clock is report-only.
  const search = probe.h8?.search || [];
  const first = search.find((s) => s.firstFulltextAt !== null) ?? null;
  const uiSearch = probe.status?.firstSearchReadyAt ?? null;
  const cr = D('canonical-ready');
  const sr = D('search-ready');
  const c = (k) => R(probe.h8?.[k] ?? null);
  const searchLag = first && first.typedAt !== null && uiSearch !== null ? first.firstFulltextAt - Math.max(uiSearch, first.typedAt) : null;
  m.H8 = { role: 'guard', chartsSelectedMs: r1(c('chartsSelectedAt')), chartsVisibleMs: r1(c('chartsVisibleAt')), chartsAllMs: r1(c('chartsAllAt')),
    portfolioMs: r1(c('portfolioAt')), chartsSelectedLagMs: r1(sub(c('chartsSelectedAt'), cr)), chartsAllLagMs: r1(sub(c('chartsAllAt'), cr)),
    searchLagMs: r1(searchLag), queries: search.length, typedToHitMs: search.map((s) => r1(sub(s.firstFulltextAt, s.typedAt))), rows: search.map((s) => s.rows ?? null),
    hitDigest: hitDigest(search),
    report: { role: 'report-only', searchHitMs: r1(first ? R(first.firstFulltextAt) : null), searchAfterDaemonReadyMs: r1(first ? sub(R(first.firstFulltextAt), sr) : null),
      typedAfterUiReadyMs: r1(first && first.typedAt !== null && uiSearch !== null ? first.typedAt - uiSearch : null) },
    reached: { chartsSelectedMs: reach('H8.chartsSelectedMs', c('chartsSelectedAt')), chartsVisibleMs: reach('H8.chartsVisibleMs', c('chartsVisibleAt')),
      chartsAllMs: reach('H8.chartsAllMs', c('chartsAllAt')), portfolioMs: reach('H8.portfolioMs', c('portfolioAt')), searchLagMs: reach('H8.searchLagMs', searchLag) } };

  // I1 — readiness honesty, zero tolerance. Each check says how much it examined.
  const i1 = probe.i1 || {};
  const end = probe.end || {};
  const kinds = [...(probe.status?.kinds || [])].sort();
  // The never-rostered count from the daemon's own evidence: every session the skims
  // published less every session the roster planned (Σ status sessions_total). At the
  // end the UI's Pending sessions must be exactly those, and its ready ones exactly the
  // roster: a rostered tree stuck Pending fails here, wherever the UI counts it.
  const rosterTotal = probe.status?.last ? probe.status.last.total : null;
  const metaPublic = daemon.metadataPublicSessions ?? null;
  const daemonNeverRostered = metaPublic !== null && rosterTotal !== null ? metaPublic - rosterTotal : null;
  const c5Evidence = probe.end != null && metaPublic !== null && rosterTotal !== null && end.daemonRoots != null;
  const checks = {
    c1ReadyBeforeReceipt: { violations: (i1.c1Before ?? 0) + (i1.c1PendingReady ?? 0), exercised: i1.checkedRows ?? 0, checkedRows: i1.checkedRows ?? 0 },
    c2NeverReverts: { violations: (i1.c2 ?? 0) + (end.retractions ?? 0), exercised: i1.checkedRows ?? 0 },
    c3SelectionOnlyByInput: { violations: i1.c3 ?? 0, exercised: i1.c3Frames ?? 0, selectionChanges: i1.selectionChanges ?? 0,
      violationsDiscreteInput: i1.c3Discrete ?? 0 },
    c4NoFlashOfEmpty: { violations: i1.c4 ?? 0, exercised: i1.c4Opens ?? 0, emptyShown: i1.c4Shown ?? 0, indexingShown: i1.c4Indexing ?? 0 },
    c5EndState: {
      violations: [
        !c5Evidence,
        end.daemonPending !== undefined && end.pendingRoots !== undefined && end.pendingRoots + (end.metaOnlyRoots ?? 0) !== end.daemonPending,
        daemonNeverRostered === null || end.pendingSessions !== daemonNeverRostered,
        rosterTotal === null || end.readyAll !== rosterTotal,
        (end.projectMismatches ?? 1) !== 0,
        end.newestProjectRoots !== end.newestProjectDaemonRoots,
        probe.renderedNewestProjectRows != null && probe.renderedNewestProjectRows !== end.newestProjectDaemonRoots,
        probe.status?.last ? probe.status.last.done !== probe.status.last.total : true,
        end.readyAll !== daemon.runtimeReadableSessions,
        end.membershipStable !== true,
        kinds.join(',') !== ENROLLED.join(','),
        end.daemonTruncated === true,
      ].filter(Boolean).length,
      exercised: c5Evidence ? 1 : 0,
      pendingRoots: end.pendingRoots ?? null, pendingSessions: end.pendingSessions ?? null, metadataOnlyRoots: end.metaOnlyRoots ?? null,
      neverRostered: end.neverRostered ?? null, daemonNeverRosteredSessions: daemonNeverRostered, daemonMetadataSessions: metaPublic, rosterSessions: rosterTotal,
      daemonPendingRoots: end.daemonPending ?? null, projectMismatches: end.projectMismatches ?? null,
      newestProjectRoots: end.newestProjectRoots ?? null, newestProjectDaemonRoots: end.newestProjectDaemonRoots ?? null,
      renderedNewestProjectRows: probe.renderedNewestProjectRows ?? null,
      readyCountDone: probe.status?.last?.done ?? null, readyCountTotal: probe.status?.last?.total ?? null,
      uiReadySessions: end.readyAll ?? null, daemonReadableSessions: daemon.runtimeReadableSessions,
      membershipStable: end.membershipStable ?? null, enrolledExact: kinds.join(',') === ENROLLED.join(','), daemonListTruncated: end.daemonTruncated ?? null,
    },
    c7PendingFaceCourse: { violations: i1.c7 ?? 0, exercised: i1.c7Faces ?? 0 },
  };
  for (const v of Object.values(checks)) v.pass = v.violations === 0;
  const selfTest = { firstMeta: probe.selfTest?.firstMetaAt != null, wsWrapped: probe.selfTest?.wsWrapped === true, fetchWrapped: probe.selfTest?.fetchWrapped === true,
    worker: probe.selfTest?.worker ?? 'none', loaf: probe.selfTest?.loaf === true, eventTiming: probe.selfTest?.eventTiming === true, layoutShift: probe.selfTest?.layoutShift === true };
  // Every instrument H1–H8 rests on: the frames, the wraps, the parsing Worker and the
  // three observers (without them H6/H7 would read 0, not "unknown").
  selfTest.pass = selfTest.firstMeta && selfTest.wsWrapped && selfTest.fetchWrapped && selfTest.worker === 'worker'
    && selfTest.loaf && selfTest.eventTiming && selfTest.layoutShift;
  m.I1 = { role: 'invariant', pass: Object.values(checks).every((v) => v.pass), checks };

  // D1 — wait reasons and copy lag (report-only).
  const rtt = (probe.status?.rtt || []).map(([a, b]) => b - a);
  const recvs = (probe.status?.rtt || []).map(([, b]) => b).filter((t) => probe.d1?.beginAt == null || t >= probe.d1.beginAt);
  const gaps = recvs.slice(1).map((t, i) => t - recvs[i]);
  const states = (probe.states || []).map(([t, s]) => [r1(R(t)), s]);
  const firstState = (pred) => { const x = states.find(([, s]) => pred(s)); return x ? x[0] : null; };
  const t = main?.times || {};
  const fromWrapper = (at, renderer) => r1(sub(renderer ? clock.fromRenderer(at) : clock.fromMain(at), clock.fromMain(t.wrapperStartAt ?? null)));
  m.D1 = { role: 'report-only',
    consentToProgressMs: r1(sub(probe.d1?.progressAt ?? null, probe.d1?.beginAt ?? null)), beginToConsentGrantedMs: r1(sub(0, R(probe.d1?.beginAt ?? null))),
    statusRtt: { n: rtt.length, p50Ms: r1(pct(rtt, 50)), p99Ms: r1(pct(rtt, 99)), aborted: probe.status?.aborted ?? null, maxGapMs: r1(gaps.length ? Math.max(...gaps) : null) },
    readyCountLag: dist(readyCountLag(clock, probe)), countUnitRatio: countUnit(probe), phases: states,
    phaseSeenLagMs: { finalizing: r1(sub(firstState((s) => s === 'finalizing'), allD)), canonical: r1(sub(firstState((s) => s.startsWith('search_') || s === 'complete'), cr)),
      search: r1(sub(firstState((s) => s === 'complete'), sr)) },
    titleRowsAfterSearchMs: probe.d1?.sheetAfterSearch === true ? r1(probe.d1?.titleRowsAfterSearchMs ?? null) : null,
    appLaunch: { windowMs: fromWrapper(t.windowAt, false), loadedMs: fromWrapper(t.didFinishLoadAt, false), firstFrameMs: fromWrapper(probe.page?.firstFrameAt ?? null, true),
      rollCallMs: fromWrapper(probe.page?.firstRollCallAt ?? null, true), rollCallReadyMs: fromWrapper(probe.page?.rollCallReadyAt ?? null, true) } };

  // D2 — order honesty (report-only). Rank samples: [H1 + target ms, actual ms after
  // H1 (null: the run ended first), rank, unranked projects].
  const h1at = probe.h1?.paintAt ?? null;
  m.D2 = { role: 'report-only', newestProjectRank: (probe.d2?.rank || []).map(([off, at, rank, unranked]) => [off, r1(sub(at, h1at)), rank, unranked]),
    inversionSelectedMs: probe.d2?.selShown === true ? r1(probe.d2?.inversionSelMs ?? null) : null,
    // Corpus-wide, from the frames, over the whole import: RECENT24 roots the room's
    // lists draw below an older readable root (the D1 placement); Foot: as a list with
    // every not-imported row at the foot would (the pre-D1 placement, and the daemon's
    // own within-project order). Both count × ms (probe-worker.js inversionReplay).
    inversionCorpusMs: probe.frozen != null ? r1(probe.end?.inversionMs ?? null) : null,
    inversionCorpusFootMs: probe.frozen != null ? r1(probe.end?.inversionFootMs ?? null) : null,
    newestGiant: { known: probe.d2?.giantKnown === true, receiptMs: r1(R(probe.d2?.giantReceiptAt ?? null)), paintMs: r1(R(probe.d2?.giantPaintAt ?? null)),
      daemonMs: r1(D('newest-giant-ready')), lagVsDaemonMs: r1(lag(R(probe.d2?.giantReceiptAt ?? null), 'newest-giant-ready')) },
    notImportedSelected: (probe.notImported || []).map(([at, n]) => [r1(R(at)), n]) };

  // D3 — transcript path and cost (report-only).
  const steps = opens.map((o) => ({ standing: o.standing, phase: bucket(o), clickToSocketMs: r1(sub(o.wsAt, o.clickAt)), socketToOpenMs: r1(sub(o.openAt, o.wsAt)),
    openToHelloMs: r1(sub(o.helloAt, o.openAt)), helloToSnapshotMs: r1(sub(o.snapAt, o.helloAt)), snapshotBytes: o.snapBytes, parseMs: r1(o.parseMs),
    snapshotToRevealMs: r1(sub(o.revealAt, o.snapAt)), revealToContentMs: r1(sub(o.contentAt, o.revealAt)), coverMs: r1(sub(o.coverEndAt, o.coverAt)),
    coverLabel: o.label, readerWaitMs: o.standing === 'not-imported' ? r1(sub(o.contentAt, o.clickAt)) : null }));
  const d3 = probe.d3 || {};
  m.D3 = { role: 'report-only', opens: steps, covers: d3.covers ?? null, coversOver100: d3.coversOver100 ?? null, coverLabels: d3.labels ?? {},
    recovers: d3.recovers ?? null, errorCovers: d3.errorCovers ?? null, sessionSockets: d3.sockets ?? null, secondSockets: d3.secondSockets ?? null,
    sessionResyncs: d3.sessionResyncs ?? null, wsErrorFrames: d3.wsErrorFrames ?? null, http503: d3.http503 ?? null, httpNotOk: d3.httpNotOk ?? null,
    longestSnapshotWaitMs: r1(d3.longestSnapshotWaitMs ?? null),
    meta: { frames: end.frames ?? null, snapshots: end.snapshots ?? null, mutations: end.mutations ?? null, resyncs: end.resyncs ?? null, bytes: end.metaBytes ?? null,
      flipFrames: (probe.meta?.flipFrames || []).length, sockets: probe.meta?.sockets ?? null },
    wire: dist(wireLeg(clock, daemon, probe)), uiLoadTax: null };

  // The daemon's own milestones on the import clock, beside the UI's.
  const milestonesMs = {};
  for (const [key, v] of Object.entries(daemon.milestones)) milestonesMs[key] = r1(clock.imp(clock.daemonWall(v.elapsedS)));
  const scriptMarks = (main?.marks || []).map(([at, name]) => [r1(clock.imp(clock.fromMain(at))), name]);
  return { clocks: clock.report, selfTest, metrics: m, milestonesMs, scriptMarks };
}
