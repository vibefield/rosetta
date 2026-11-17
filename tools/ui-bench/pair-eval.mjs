#!/usr/bin/env node
// The ui-pair/1 evaluator: one frozen BEFORE/AFTER pair (or an A/A campaign) from the
// ui-run/1 documents pair.mjs collected, judged by gate set (a), the user's decision
// of 2026-11-03:
//   gated: ui.newest_readable (H2), horizons 24h/7d @newest (H3), all-readable (H-all),
//   flip→paint p95 (H5), recency inversion (D2), open-ready p95 (H4, reader), long-task
//   ms (H6 blocking per window); a gated metric is a GUARD unless the campaign declared
//   it a CLAIM before its first run; every other metric is REPORT-ONLY.
// Pass rule (guard): AFTER's median is no worse than BEFORE's median plus BEFORE's IQR
// (inclusive quartiles, as the R1 kit's summarize.median_iqr; max−min below 4 values),
// widened to the A/A floor only when the campaign declared one. Every value is "lower
// is better". A claim passes when AFTER's median is at most `maxRatio` × BEFORE's (or,
// without a ratio, below BEFORE's median minus the tolerance) and, if declared, the
// one-sided Mann–Whitney p is below `maxP`.
// A pair is VOID — no verdict at all — when any run is missing, not complete, failed its
// self-test or I1, or when the snapshot (or fixture), harness, machine, window, DPR,
// Electron, script or a side's binary/dist hash differs from the declaration, or when
// an equality digest differs between runs of one script (logical TOTAL,
// sourceSetDigest, ranked ids, UI hit digest, final roots and ready sessions).
// Pure functions plus a CLI that re-evaluates a pair directory exactly as declared:
//   node tools/ui-bench/pair-eval.mjs PAIR_DIR
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { pct } from './metrics.mjs';
import { validateRun } from './privacy.mjs';

export const PAIR_SCHEMA_PATH = fileURLToPath(new URL('./ui-pair-1.schema.json', import.meta.url));
export const GATE_SET = 'a';
export const SCRIPTS = ['reader-v1', 'passive-v1'];
export const EQUALITY_KEYS = ['logicalTotal', 'sourceSetDigest', 'rankedIdsSha256', 'hitDigest', 'finalRoots', 'finalReadySessions'];
/** Equality keys that must be non-null in every run (hitDigest is null when a script never searches). */
const REQUIRED_EQUALITY = ['logicalTotal', 'sourceSetDigest', 'rankedIdsSha256'];

const g = (o, ...ks) => ks.reduce((x, k) => (x === null || x === undefined ? null : x[k]), o) ?? null;
/** H4 p95 over the reader's opens of ready rows, re-derived from the probe export
 * exactly as metrics.mjs derives H4's p50 and max (the run document keeps the probe). */
function h4p95(d) {
  const xs = (d.probe?.opens || []).filter((o) => o.standing === 'ready' && o.contentAt != null && o.clickAt != null).map((o) => o.contentAt - o.clickAt);
  const v = pct(xs, 95);
  return v === null ? null : Math.round(v * 10) / 10;
}

// Gate set (a) on the ui-run/1 fields. `group` names the user's gate; `reached` reads
// the run's own reached map (absent: a null value is not-reached).
const gate = (id, group, get, reached) => ({ id, group, gated: true, get, reached });
const rep = (id, group, get, reached) => ({ id, group, gated: false, get, reached });
const R = (metric, key) => (d) => g(d, 'metrics', metric, 'reached', key);
export const METRICS = [
  gate('H2.ms', 'H2 newest readable', (d) => g(d, 'metrics', 'H2', 'ms'), R('H2', 'ms')),
  gate('H2.receiptMs', 'H2 newest readable', (d) => g(d, 'metrics', 'H2', 'receiptMs'), R('H2', 'receiptMs')),
  gate('H3.h24.receiptMs', 'H3 24h@newest', (d) => g(d, 'metrics', 'H3', 'h24', 'receiptMs'), R('H3', 'h24.receiptMs')),
  gate('H3.h24.paintedSelMs', 'H3 24h@newest', (d) => g(d, 'metrics', 'H3', 'h24', 'paintedSelMs'), R('H3', 'h24.paintedSelMs')),
  gate('H3.h7d.receiptMs', 'H3 7d@newest', (d) => g(d, 'metrics', 'H3', 'h7d', 'receiptMs'), R('H3', 'h7d.receiptMs')),
  gate('H3.h7d.paintedSelMs', 'H3 7d@newest', (d) => g(d, 'metrics', 'H3', 'h7d', 'paintedSelMs'), R('H3', 'h7d.paintedSelMs')),
  gate('Hall.receiptMs', 'H-all all readable', (d) => g(d, 'metrics', 'Hall', 'receiptMs'), R('Hall', 'receiptMs')),
  gate('Hall.paintedSelMs', 'H-all all readable', (d) => g(d, 'metrics', 'Hall', 'paintedSelMs'), R('Hall', 'paintedSelMs')),
  gate('H5.p95Ms', 'flip→paint p95', (d) => g(d, 'metrics', 'H5', 'p95Ms'), R('H5', 'samples')),
  gate('D2.inversionCorpusMs', 'inversion', (d) => g(d, 'metrics', 'D2', 'inversionCorpusMs')),
  gate('D2.inversionSelectedMs', 'inversion', (d) => g(d, 'metrics', 'D2', 'inversionSelectedMs')),
  gate('H4.p95Ms', 'open-ready p95', h4p95, R('H4', 'samples')),
  gate('H6.streaming.blockingMs', 'long-task ms', (d) => g(d, 'metrics', 'H6', 'byWindow', 'streaming', 'blockingMs'), R('H6', 'longestFrame')),
  gate('H6.finalizing.blockingMs', 'long-task ms', (d) => g(d, 'metrics', 'H6', 'byWindow', 'finalizing', 'blockingMs'), R('H6', 'longestFrame')),
  gate('H6.post.blockingMs', 'long-task ms', (d) => g(d, 'metrics', 'H6', 'byWindow', 'post', 'blockingMs'), R('H6', 'longestFrame')),
  rep('H1.ms', 'H1', (d) => g(d, 'metrics', 'H1', 'ms'), R('H1', 'ms')),
  rep('H1.metaPushMs', 'H1', (d) => g(d, 'metrics', 'H1', 'metaPushMs')),
  rep('H1.statusPollMs', 'H1', (d) => g(d, 'metrics', 'H1', 'statusPollMs')),
  rep('H2.lag.ms', 'H2 newest readable', (d) => g(d, 'metrics', 'H2', 'lag', 'ms')),
  rep('H2.heldReadyMs', 'H2 newest readable', (d) => g(d, 'metrics', 'H2', 'heldReadyMs')),
  rep('H4.p50Ms', 'open-ready', (d) => g(d, 'metrics', 'H4', 'p50Ms'), R('H4', 'samples')),
  rep('H4.maxMs', 'open-ready', (d) => g(d, 'metrics', 'H4', 'maxMs'), R('H4', 'samples')),
  rep('H4.pendingFace.p50Ms', 'open-ready', (d) => g(d, 'metrics', 'H4', 'pendingFace', 'p50Ms'), R('H4', 'pendingFace')),
  rep('H5.p50Ms', 'flip→paint', (d) => g(d, 'metrics', 'H5', 'p50Ms'), R('H5', 'samples')),
  rep('H5.maxMs', 'flip→paint', (d) => g(d, 'metrics', 'H5', 'maxMs'), R('H5', 'samples')),
  rep('H6.streaming.longestMs', 'long tasks', (d) => g(d, 'metrics', 'H6', 'byWindow', 'streaming', 'longestMs'), R('H6', 'longestFrame')),
  rep('H6.finalizing.longestMs', 'long tasks', (d) => g(d, 'metrics', 'H6', 'byWindow', 'finalizing', 'longestMs'), R('H6', 'longestFrame')),
  rep('H6.post.longestMs', 'long tasks', (d) => g(d, 'metrics', 'H6', 'byWindow', 'post', 'longestMs'), R('H6', 'longestFrame')),
  rep('H6.input.p98Ms', 'input', (d) => g(d, 'metrics', 'H6', 'input', 'p98Ms'), R('H6', 'input')),
  rep('H6.input.maxMs', 'input', (d) => g(d, 'metrics', 'H6', 'input', 'maxMs'), R('H6', 'input')),
  rep('H6.heapPeakMb', 'memory', (d) => g(d, 'metrics', 'H6', 'report', 'heapPeakMb')),
  rep('H6.appWorkingSetPeakMb', 'memory', (d) => g(d, 'metrics', 'H6', 'report', 'appWorkingSetPeakMb')),
  rep('H6.probeMainPctOfWall', 'probe cost', (d) => g(d, 'metrics', 'H6', 'report', 'probeMainPctOfWall')),
  rep('H7.projectsMoves', 'stability', (d) => g(d, 'metrics', 'H7', 'projectsMoves'), R('H7', 'moves')),
  rep('H7.sessionsMoves', 'stability', (d) => g(d, 'metrics', 'H7', 'sessionsMoves'), R('H7', 'moves')),
  rep('H7.aimMisses', 'stability', (d) => g(d, 'metrics', 'H7', 'invariant', 'aimMisses'), R('H7', 'aims')),
  rep('H7.selectionJumps', 'stability', (d) => g(d, 'metrics', 'H7', 'invariant', 'selectionJumps'), R('H7', 'moves')),
  rep('H8.chartsSelectedMs', 'charts and search', (d) => g(d, 'metrics', 'H8', 'chartsSelectedMs'), R('H8', 'chartsSelectedMs')),
  rep('H8.chartsAllMs', 'charts and search', (d) => g(d, 'metrics', 'H8', 'chartsAllMs'), R('H8', 'chartsAllMs')),
  rep('H8.portfolioMs', 'charts and search', (d) => g(d, 'metrics', 'H8', 'portfolioMs'), R('H8', 'portfolioMs')),
  rep('H8.searchLagMs', 'charts and search', (d) => g(d, 'metrics', 'H8', 'searchLagMs'), R('H8', 'searchLagMs')),
  rep('D1.consentToProgressMs', 'wait reasons', (d) => g(d, 'metrics', 'D1', 'consentToProgressMs')),
  rep('D2.newestGiant.receiptMs', 'order', (d) => g(d, 'metrics', 'D2', 'newestGiant', 'receiptMs')),
  rep('D2.inversionCorpusFootMs', 'order', (d) => g(d, 'metrics', 'D2', 'inversionCorpusFootMs')),
  rep('D3.wire.p95Ms', 'transcript path', (d) => g(d, 'metrics', 'D3', 'wire', 'p95Ms')),
  ...['metadata-ready', 'first-readable', 'newest-giant-ready', 'recent-ready:24h@newest', 'recent-ready:7d@newest', 'canonical-ready', 'search-ready']
    .map((k) => rep(`daemon.${k}`, 'daemon milestones', (d) => g(d, 'daemon', 'milestonesMs', k))),
  rep('daemon.footprintMb', 'daemon memory', (d) => g(d, 'daemon', 'memoryPeak', 'footprintMb')),
  rep('run.wallS', 'run', (d) => g(d, 'run', 'wallS')),
];
export const METRIC_IDS = METRICS.map((m) => m.id);
const BY_ID = new Map(METRICS.map((m) => [m.id, m]));

/** One run's value of a metric: { value, reached } (reached: yes, not-reached, not-applicable). */
export function extract(doc, metric) {
  const v = metric.get(doc);
  const value = typeof v === 'number' && Number.isFinite(v) ? v : null;
  const declared = metric.reached ? metric.reached(doc) : null;
  if (declared === 'not-applicable') return { value: null, reached: 'not-applicable' };
  if (declared === 'not-reached' || value === null) return { value: null, reached: 'not-reached' };
  return { value, reached: 'yes' };
}

/** Inclusive quartiles (Python statistics.quantiles(n=4, method='inclusive'); R type 7). */
function quantile(sorted, q) {
  const pos = q * (sorted.length - 1);
  const lo = Math.floor(pos);
  const hi = Math.min(sorted.length - 1, lo + 1);
  return sorted[lo] + (pos - lo) * (sorted[hi] - sorted[lo]);
}
/** Median and IQR as tools/r1-bench/summarize.py median_iqr: below 4 values the IQR is max − min. */
export function stats(values) {
  const v = values.filter((x) => typeof x === 'number' && Number.isFinite(x)).sort((a, b) => a - b);
  if (!v.length) return null;
  const median = quantile(v, 0.5);
  const iqr = v.length >= 4 ? quantile(v, 0.75) - quantile(v, 0.25) : v[v.length - 1] - v[0];
  return { n: v.length, median: r3(median), iqr: r3(iqr), min: v[0], max: v[v.length - 1] };
}
const r3 = (x) => (x === null || !Number.isFinite(x) ? null : Math.round(x * 1000) / 1000);

/** One-sided Mann–Whitney p that `after` is stochastically smaller than `before`
 * (normal approximation with tie correction and continuity correction). */
export function mannWhitneyP(before, after) {
  const n1 = after.length;
  const n2 = before.length;
  if (!n1 || !n2) return null;
  let u = 0;
  for (const a of after) for (const b of before) u += a < b ? 1 : a === b ? 0.5 : 0;
  const all = [...after, ...before].sort((x, y) => x - y);
  let ties = 0;
  for (let i = 0; i < all.length;) {
    let j = i;
    while (j < all.length && all[j] === all[i]) j += 1;
    const t = j - i;
    ties += t ** 3 - t;
    i = j;
  }
  const n = n1 + n2;
  const mu = (n1 * n2) / 2;
  const sigma = Math.sqrt(((n1 * n2) / 12) * ((n + 1) - ties / (n * (n - 1))));
  if (sigma === 0) return u > mu ? 0 : 1;
  const z = (u - mu - 0.5) / sigma;
  return r3(1 - normalCdf(z));
}
function normalCdf(z) {
  // Abramowitz–Stegun 7.1.26 for erf.
  const x = Math.abs(z) / Math.SQRT2;
  const t = 1 / (1 + 0.3275911 * x);
  const erf = 1 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * Math.exp(-x * x);
  return z >= 0 ? (1 + erf) / 2 : (1 - erf) / 2;
}

function reachedCounts(xs) {
  return { yes: xs.filter((x) => x.reached === 'yes').length, notReached: xs.filter((x) => x.reached === 'not-reached').length,
    notApplicable: xs.filter((x) => x.reached === 'not-applicable').length };
}

/**
 * One metric of one script over both sides. `role`: claim, guard or report-only.
 * Returns the row the pair document carries.
 */
export function judge({ metric, script, role, before, after, claim = null, floorMs = null }) {
  const b = before.map((d) => extract(d, metric));
  const a = after.map((d) => extract(d, metric));
  const sb = stats(b.map((x) => x.value));
  const sa = stats(a.map((x) => x.value));
  const row = { script, metric: metric.id, role, before: { ...(sb ?? emptyStats()), reached: reachedCounts(b) }, after: { ...(sa ?? emptyStats()), reached: reachedCounts(a) },
    deltaMedian: sb && sa ? r3(sa.median - sb.median) : null, ratio: sb && sa && sb.median !== 0 ? r3(sa.median / sb.median) : null,
    toleranceMs: null, toleranceSource: 'none', maxRatio: claim?.maxRatio ?? null, maxP: claim?.maxP ?? null,
    mannWhitneyP: sb && sa ? mannWhitneyP(b.map((x) => x.value).filter((x) => x !== null), a.map((x) => x.value).filter((x) => x !== null)) : null, verdict: 'report' };
  if (role === 'report-only') return row;
  const all = [...b, ...a];
  if (all.every((x) => x.reached === 'not-applicable')) return { ...row, verdict: 'not-applicable' };
  if (all.every((x) => x.reached !== 'yes')) return { ...row, verdict: 'not-reached' };
  // Compared only when every run of both sides reached it: a state that happens in some
  // runs and not others (or on one side only) is a change the pair cannot rank.
  if (!all.every((x) => x.reached === 'yes')) return { ...row, verdict: 'incomparable' };
  const tol = Math.max(sb.iqr, floorMs ?? 0);
  row.toleranceMs = r3(tol);
  row.toleranceSource = floorMs !== null && floorMs > sb.iqr ? 'aa-floor' : 'before-iqr';
  if (role === 'guard') return { ...row, verdict: sa.median <= sb.median + tol ? 'pass' : 'fail' };
  const better = claim?.maxRatio != null ? sa.median <= claim.maxRatio * sb.median : sa.median < sb.median - tol;
  const significant = claim?.maxP != null ? row.mannWhitneyP !== null && row.mannWhitneyP < claim.maxP : true;
  return { ...row, verdict: better && significant ? 'pass' : 'fail' };
}
const emptyStats = () => ({ n: 0, median: null, iqr: null, min: null, max: null });

const sameJson = (x, y) => JSON.stringify(x) === JSON.stringify(y);
/** The identity every run of a campaign must share (not the side's binary or dist). */
export function sharedIdentity(doc) {
  const id = doc.identity ?? {};
  return { snapshot: id.snapshot ? { walkInputsDigest: id.snapshot.walkInputsDigest, manifestSha256: id.snapshot.manifestSha256, files: id.snapshot.files, bytes: id.snapshot.bytes } : null,
    fixture: id.fixture ? { name: id.fixture.name, specSha256: id.fixture.specSha256, treeSha256: id.fixture.treeSha256, files: id.fixture.files, bytes: id.fixture.bytes,
      walkInputsDigest: id.fixture.walkInputsDigest, generatorSha256: id.fixture.generatorSha256, idleGuard: id.fixture.idleGuard } : null,
    harnessSha256: id.harnessSha256 ?? null, machine: id.machine ?? null, windowPx: id.windowPx ?? null, dpr: id.dpr ?? null, dprMeasured: id.dprMeasured ?? null,
    electron: id.electron ?? null, deviations: id.deviations ?? null, enrolled: id.enrolled ?? null };
}

/**
 * Validity of every run and the pair: returns { voidReasons, runRows }. `runs`:
 * [{ slot, round, side, script, doc|null, redo, power }]. `declared`: the campaign's
 * declaration (sides' hashes, scripts, runs per side).
 */
export function validity(declared, runs) {
  const reasons = new Set();
  const counted = runs.filter((r) => !r.discarded);
  for (const side of Object.keys(declared.sides)) {
    for (const script of declared.scripts) {
      const n = counted.filter((r) => r.side === side && r.script === script && r.doc).length;
      if (n !== declared.runsPerSide) reasons.add('run-missing');
    }
  }
  let ref = null;
  const scriptSha = {};
  for (const r of counted) {
    const d = r.doc;
    if (!d) { reasons.add('run-missing'); continue; }
    if (d.format !== 'ui-run/1' || d.script?.name !== r.script) reasons.add('wrong-document');
    if (d.run?.outcome !== 'complete') reasons.add('run-not-complete');
    if (d.run?.selfTestPass !== true) reasons.add('self-test-failed');
    if (g(d, 'metrics', 'I1', 'pass') !== true) reasons.add('i1-failed');
    if (d.privacy?.schema !== 'pass' || d.privacy?.forbiddenScan !== 'pass') reasons.add('privacy-failed');
    const want = declared.sides[r.side];
    if (d.identity?.rosettadSha256 !== want?.rosettadSha256) reasons.add('rosettad-differs-from-declared');
    if (d.identity?.distSha256 !== want?.distSha256) reasons.add('dist-differs-from-declared');
    const shared = sharedIdentity(d);
    if (ref === null) ref = shared; else if (!sameJson(ref, shared)) reasons.add('identity-differs');
    if (declared.source && !sameJson(shared.snapshot ?? shared.fixture, declared.source)) reasons.add('source-differs-from-declared');
    if (scriptSha[r.script] === undefined) scriptSha[r.script] = d.script?.sha256 ?? null;
    else if (scriptSha[r.script] !== d.script?.sha256) reasons.add('script-differs');
  }
  // Equality digests, per script, across both sides.
  const equality = {};
  for (const script of declared.scripts) {
    const docs = counted.filter((r) => r.script === script && r.doc).map((r) => r.doc);
    const e = {};
    for (const k of EQUALITY_KEYS) {
      const vals = docs.map((d) => g(d, 'equality', k));
      const equal = vals.length > 0 && vals.every((v) => v === vals[0]) && (!REQUIRED_EQUALITY.includes(k) || vals[0] !== null);
      if (!equal) reasons.add(vals.some((v) => v === null) && REQUIRED_EQUALITY.includes(k) ? 'digest-missing' : 'digest-differs');
      e[k] = { value: equal ? vals[0] ?? null : null, equal };
    }
    equality[script] = e;
  }
  return { voidReasons: [...reasons].sort(), identity: ref, scriptSha256: scriptSha, equality };
}

/**
 * The ui-pair/1 document. `declared`: what pair.mjs wrote before the first run
 * (kind, sides, scripts, runsPerSide, order, source, claims, floors, options);
 * `runs`: every slot with its document; `campaignSha256`: sha256 of declared.json.
 */
export function evaluatePair({ declared, runs, campaignSha256, startedAt, finishedAt }) {
  const v = validity(declared, runs);
  const sides = Object.keys(declared.sides);
  const [first, second] = sides;
  const aa = declared.kind === 'aa';
  const claims = declared.claims ?? [];
  const floors = declared.floors?.values ?? {};
  const rows = [];
  const counted = runs.filter((r) => !r.discarded && r.doc);
  for (const script of declared.scripts) {
    const before = counted.filter((r) => r.side === first && r.script === script).map((r) => r.doc);
    const after = counted.filter((r) => r.side === second && r.script === script).map((r) => r.doc);
    for (const metric of METRICS) {
      const claim = claims.find((c) => c.script === script && c.metric === metric.id) ?? null;
      const role = aa || !metric.gated ? 'report-only' : claim ? 'claim' : 'guard';
      const row = judge({ metric, script, role, before, after, claim, floorMs: floors[`${script}:${metric.id}`] ?? null });
      if (aa && metric.gated) {
        // A/A: the noise floor each gated metric's later guard may widen to.
        const b = row.before;
        const a = row.after;
        row.floorMs = b.n && a.n ? r3(Math.max(b.iqr, a.iqr, Math.abs(a.median - b.median))) : null;
      }
      rows.push(row);
    }
  }
  const verdicts = rows.filter((r) => r.role !== 'report-only');
  const count = (role, verdict) => verdicts.filter((r) => r.role === role && r.verdict === verdict).length;
  const failing = verdicts.filter((r) => r.verdict === 'fail' || r.verdict === 'incomparable').length;
  const verdict = v.voidReasons.length ? 'void' : aa ? 'aa' : failing ? 'fail' : 'pass';
  const quiet = {
    runsOverGuard: counted.filter((r) => r.doc.run?.guard?.proceededOverGuard === true && r.idleGuard).length,
    swapRedos: runs.filter((r) => r.discarded).length,
    maxSwapGrowthMb: Math.max(0, ...counted.map((r) => (r.doc.run?.guard?.swapMbAfter ?? 0) - (r.doc.run?.guard?.swapMbBefore ?? 0))),
    onBattery: counted.filter((r) => r.power?.source === 'battery').length,
    lowPowerMode: counted.filter((r) => r.power?.lowPowerMode === true).length,
  };
  return {
    format: 'ui-pair/1', gateSet: GATE_SET, kind: declared.kind, campaignSha256, startedAt, finishedAt,
    order: declared.order, runsPerSide: declared.runsPerSide, scripts: declared.scripts,
    sides: Object.fromEntries(sides.map((s) => [s, { rosettadSha256: declared.sides[s].rosettadSha256, distSha256: declared.sides[s].distSha256,
      distFiles: declared.sides[s].distFiles ?? null }])),
    identity: v.identity, scriptSha256: v.scriptSha256,
    options: declared.options,
    claims: claims.map((c) => ({ script: c.script, metric: c.metric, maxRatio: c.maxRatio ?? null, maxP: c.maxP ?? null })), floors: { source: declared.floors ? 'aa' : 'none', aaCampaignSha256: declared.floors?.aaCampaignSha256 ?? null },
    equality: v.equality,
    void: v.voidReasons.length > 0, voidReasons: v.voidReasons,
    runs: runs.map((r) => ({ slot: r.slot, round: r.round, side: r.side, script: r.script, discarded: r.discarded === true, present: Boolean(r.doc),
      outcome: r.doc?.run?.outcome ?? null, selfTestPass: r.doc?.run?.selfTestPass ?? null, i1Pass: g(r.doc, 'metrics', 'I1', 'pass'),
      h7InvariantPass: g(r.doc, 'metrics', 'H7', 'invariant', 'pass'), wallS: r.doc?.run?.wallS ?? null,
      cpuIdleAtStart: r.doc?.run?.guard?.cpuIdleAtStart ?? null, guardWaitedS: r.doc?.run?.guard?.waitedS ?? null,
      proceededOverGuard: r.doc?.run?.guard?.proceededOverGuard ?? null, load1: r.doc?.run?.guard?.load1 ?? null,
      swapMbBefore: r.doc?.run?.guard?.swapMbBefore ?? null, swapMbAfter: r.doc?.run?.guard?.swapMbAfter ?? null,
      power: r.power ?? { source: 'unknown', lowPowerMode: null }, exitCode: r.exitCode ?? null })),
    metrics: rows,
    summary: { verdict, claims: { pass: count('claim', 'pass'), fail: count('claim', 'fail'), incomparable: count('claim', 'incomparable') },
      guards: { pass: count('guard', 'pass'), fail: count('guard', 'fail'), incomparable: count('guard', 'incomparable'),
        notMeasured: count('guard', 'not-reached') + count('guard', 'not-applicable') }, quiet },
  };
}

const fmt = (x) => (x === null || x === undefined ? '—' : Number.isInteger(x) ? String(x) : x.toFixed(1));
/** The short plain-text verdict table. */
export function verdictTable(p) {
  const [s1, s2] = Object.keys(p.sides);
  const out = [];
  out.push(`ui-pair/1 ${p.kind} gate set (${p.gateSet}): ${p.summary.verdict.toUpperCase()}${p.void ? ` (void: ${p.voidReasons.join(', ')})` : ''}`);
  out.push(`campaign ${p.campaignSha256.slice(0, 12)}  ${p.runsPerSide} runs a side, order ${p.order}, scripts ${p.scripts.join(', ')}`);
  out.push(`${s1}: rosettad ${p.sides[s1].rosettadSha256.slice(0, 12)} dist ${p.sides[s1].distSha256.slice(0, 12)}   ${s2}: rosettad ${p.sides[s2].rosettadSha256.slice(0, 12)} dist ${p.sides[s2].distSha256.slice(0, 12)}`);
  const q = p.summary.quiet;
  out.push(`quiet: ${q.runsOverGuard} runs over the idle guard, ${q.swapRedos} swap redos, max swap growth ${fmt(q.maxSwapGrowthMb)} MB, ${q.onBattery} on battery, ${q.lowPowerMode} in Low Power Mode`);
  const head = ['script', 'metric', 'role', `${s1} med`, 'iqr', `${s2} med`, 'iqr', 'Δ', 'tol', p.kind === 'aa' ? 'floor' : 'verdict'];
  const lines = p.metrics.filter((r) => p.kind === 'aa' ? BY_ID.get(r.metric)?.gated : r.role !== 'report-only' || r.before.n || r.after.n).map((r) => [
    r.script, r.metric, r.role === 'report-only' ? 'report' : r.role, fmt(r.before.median), fmt(r.before.iqr), fmt(r.after.median), fmt(r.after.iqr),
    fmt(r.deltaMedian), fmt(r.toleranceMs), p.kind === 'aa' ? fmt(r.floorMs) : r.verdict === 'report' ? '' : r.verdict]);
  const widths = head.map((h, i) => Math.max(h.length, ...lines.map((l) => l[i].length)));
  const pad = (cells) => cells.map((c, i) => (i < 3 ? c.padEnd(widths[i]) : c.padStart(widths[i]))).join('  ').trimEnd();
  out.push('', pad(head), ...lines.map(pad));
  const eq = Object.entries(p.equality).map(([s, e]) => `${s}: ${EQUALITY_KEYS.map((k) => `${k} ${e[k].equal ? 'equal' : 'DIFFERS'}`).join(', ')}`);
  out.push('', ...eq);
  const n = p.summary;
  out.push('', `claims pass ${n.claims.pass} fail ${n.claims.fail}; guards pass ${n.guards.pass} fail ${n.guards.fail} incomparable ${n.guards.incomparable} not measured ${n.guards.notMeasured}`);
  return `${out.join('\n')}\n`;
}

/** Read a pair directory as pair.mjs left it: declared.json, slots.json and runs/<slot>/ui-run.json. */
export function loadPairDir(dir) {
  const declaredText = readFileSync(path.join(dir, 'declared.json'), 'utf8');
  const declared = JSON.parse(declaredText);
  const slots = JSON.parse(readFileSync(path.join(dir, 'slots.json'), 'utf8'));
  const runs = slots.slots.map((s) => {
    const f = path.join(dir, 'runs', s.dir, 'ui-run.json');
    return { ...s, doc: existsSync(f) ? JSON.parse(readFileSync(f, 'utf8')) : null };
  });
  return { declared, runs, campaignSha256: createHash('sha256').update(declaredText).digest('hex'), startedAt: slots.startedAt, finishedAt: slots.finishedAt };
}

/** Evaluate, gate and write ui-pair.json + verdict.txt; returns { pair, ok, errors }. */
export function writePair(dir, { forbidden = [] } = {}) {
  const input = loadPairDir(dir);
  const pair = evaluatePair(input);
  const schema = JSON.parse(readFileSync(PAIR_SCHEMA_PATH, 'utf8'));
  const gateResult = validateRun(pair, { schema, forbidden: [...forbidden, path.resolve(dir)] });
  const table = verdictTable(pair);
  writeFileSync(path.join(dir, 'verdict.txt'), table, { mode: 0o644 });
  if (gateResult.ok) writeFileSync(path.join(dir, 'ui-pair.json'), `${JSON.stringify(pair, null, 1)}\n`, { mode: 0o644 });
  return { pair, table, ok: gateResult.ok, errors: gateResult.errors };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const dir = process.argv[2];
  if (!dir || process.argv.length > 3) { console.error('usage: pair-eval.mjs PAIR_DIR'); process.exit(2); }
  const { pair, table, ok, errors } = writePair(path.resolve(dir));
  process.stdout.write(table);
  if (!ok) { for (const e of errors.slice(0, 20)) console.error(`privacy: ${e}`); console.error('ui-pair.json not written (privacy gate)'); process.exit(2); }
  process.exit(pair.summary.verdict === 'pass' || pair.summary.verdict === 'aa' ? 0 : 1);
}
