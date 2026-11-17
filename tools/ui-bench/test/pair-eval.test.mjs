// pair-eval.mjs and pair.mjs's pure parts over synthetic ui-run/1 documents built as
// run_ui.mjs builds them (fixtures.mjs): the gate set's verdicts, the void rules, the
// A/A floors, the privacy gate on ui-pair/1, the schedule and the refusals.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { parseDaemonLog } from '../daemon-lines.mjs';
import { derive } from '../metrics.mjs';
import { METRICS, METRIC_IDS, PAIR_SCHEMA_PATH, evaluatePair, extract, judge, mannWhitneyP, stats, verdictTable } from '../pair-eval.mjs';
import { claimProblems, pairProblems, schedule, sideProblems } from '../pair.mjs';
import { validateRun } from '../privacy.mjs';
import { DEVIATIONS, HARNESS_FILES, R1_FILES, buildRunDoc } from '../run_ui.mjs';
import { HEX, daemonStdout, mainNotes, probeLog } from './fixtures.mjs';

const harness = {};
for (const f of HARNESS_FILES) harness[f] = HEX('1');
for (const f of R1_FILES) harness[`r1-bench/${f}`] = HEX('2');
const SNAP = { walkInputsDigest: HEX('6'), manifestSha256: HEX('7'), files: 639, bytes: 1778445372 };
const SIDES = { before: { rosettadSha256: HEX('a'), distSha256: HEX('5'), distFiles: 3 }, after: { rosettadSha256: HEX('b'), distSha256: HEX('5'), distFiles: 3 } };

/** One ui-run/1 document; `set` patches metric values (path → number). */
function runDoc({ side = 'before', script = 'reader-v1', set = {}, digests = {} } = {}) {
  const probe = probeLog();
  const main = mainNotes();
  const derived = derive({ probe, main, daemon: parseDaemonLog(daemonStdout()), mainPings: { start: { offsetMs: 0, rttMs: 0.1 }, end: null } });
  const doc = buildRunDoc({
    script: { name: script, sha256: HEX(script === 'reader-v1' ? '3' : '4') },
    identity: { rosettadSha256: SIDES[side].rosettadSha256, distSha256: SIDES[side].distSha256, distFiles: 3, electron: '43.7.9', snapshot: structuredClone(SNAP), fixture: null,
      harnessSha256: harness, machine: { model: 'Mac16,10', os: '26.7.2', cpus: 10, memGb: 16 }, windowPx: '1500x940', dpr: 2, dprMeasured: 2,
      deviations: DEVIATIONS, enrolled: ['claude-code', 'codex'] },
    run: { startedAt: '2026-11-03T20:14:55.151Z', outcome: 'complete', outcomeReasons: [], wallS: 60.2, daemonExit: { code: 0, signal: null }, electronExit: 0,
      guard: { cpuIdleAtStart: 85, waitedS: 4, proceededOverGuard: false, load1: 2.9, idleMin: 75, swapMbBefore: 3000, swapMbAfter: 3000, freeGbBefore: 44.2 },
      snapshotVerify: { ok: true, files: 639, bytes: 1778445372, wallMs: 3000 }, scriptOk: true, scriptTimeouts: 0, newestGiantKnown: true, newestGiantTreeBytes: 9e8 },
    probe, mainNotes: main, derived, daemonLog: parseDaemonLog(daemonStdout()), stderr: [],
    digests: { logicalTotal: HEX('8'), strictTotal: null, strictNoidTotal: null, strictNoidRTotal: null, surrogateTotal: null, logicalOutSha256: null,
      cursorOrderOutSha256: null, counts: null, sourceSetDigest: HEX('e'), sourceDigest: null, metaDigest: null, entryDigest: null, ledgerVerifyOk: true,
      rankedIdsSha256: HEX('2'), rankedSha256: null, rankedWarmExit: 0, ...digests },
    forbiddenCount: 1,
  });
  doc.privacy = { schema: 'pass', forbiddenScan: 'pass', forbiddenValues: 1 };
  for (const [p, v] of Object.entries(set)) {
    const keys = p.split('.');
    let o = doc;
    for (const k of keys.slice(0, -1)) o = o[k];
    o[keys.at(-1)] = v;
  }
  return doc;
}

const declaredFor = (over = {}) => ({ format: 'ui-pair-declared/1', gateSet: 'a', kind: 'daemon', order: 'abab', runsPerSide: 5, scripts: ['reader-v1'],
  options: { timeoutS: 600, idleMin: 75, guardWaitS: 1800, idleGuard: true, minFreeGb: 25, dpr: 2, swapRedoMb: 256 }, sides: SIDES, source: SNAP,
  sourceKind: 'snapshot', claims: [], floors: null, ...over });

/** Slots with documents; `h2` gives each side's five H2.ms values. */
function campaign({ h2Before = [3000, 3100, 3050, 2990, 3020], h2After = h2Before, mutate = () => {}, sides = ['before', 'after'] } = {}) {
  const runs = [];
  for (let i = 0; i < h2Before.length; i += 1) {
    for (const [k, side] of sides.entries()) {
      const v = k === 0 ? h2Before[i] : h2After[i];
      const doc = runDoc({ side: side in SIDES ? side : 'before', set: { 'metrics.H2.ms': v } });
      mutate(doc, side, i);
      runs.push({ slot: runs.length, round: i, side, script: 'reader-v1', dir: `x${runs.length}`, doc, power: { source: 'ac', lowPowerMode: false }, idleGuard: true, exitCode: 0 });
    }
  }
  return runs;
}
const evaluate = (declared, runs) => evaluatePair({ declared, runs, campaignSha256: HEX('c'), startedAt: '2026-11-03T20:00:00.000Z', finishedAt: '2026-11-03T21:00:00.000Z' });
const row = (pair, metric, script = 'reader-v1') => pair.metrics.find((r) => r.metric === metric && r.script === script);

test('stats: inclusive quartiles as summarize.py median_iqr; max−min below four values', () => {
  assert.deepEqual(stats([1, 2, 3, 4, 5]), { n: 5, median: 3, iqr: 2, min: 1, max: 5 });
  assert.deepEqual(stats([10, 1, 4, 7]), { n: 4, median: 5.5, iqr: 4.5, min: 1, max: 10 });
  assert.equal(stats([5, 9]).iqr, 4);
  assert.equal(stats([null, 2]).n, 1);
  assert.equal(stats([]), null);
});

test('mann-whitney: clearly smaller after gives a small p; identical samples give a large one', () => {
  assert.ok(mannWhitneyP([10, 11, 12, 13, 14, 15], [1, 2, 3, 4, 5, 6]) < 0.01);
  assert.ok(mannWhitneyP([1, 2, 3, 4], [1, 2, 3, 4]) > 0.4);
  assert.equal(mannWhitneyP([], [1]), null);
});

test('the gate set (a) maps onto ui-run/1 fields and every metric extracts from a real document', () => {
  const gated = METRICS.filter((m) => m.gated).map((m) => m.id);
  assert.deepEqual(gated, ['H2.ms', 'H2.receiptMs', 'H3.h24.receiptMs', 'H3.h24.paintedSelMs', 'H3.h7d.receiptMs', 'H3.h7d.paintedSelMs', 'Hall.receiptMs',
    'Hall.paintedSelMs', 'H5.p95Ms', 'D2.inversionCorpusMs', 'D2.inversionSelectedMs', 'H4.p95Ms', 'H6.streaming.blockingMs', 'H6.finalizing.blockingMs',
    'H6.post.blockingMs']);
  const doc = runDoc();
  for (const m of METRICS) {
    const x = extract(doc, m);
    assert.ok(['yes', 'not-reached', 'not-applicable'].includes(x.reached), m.id);
    assert.equal(x.reached === 'yes', typeof x.value === 'number', m.id);
  }
  // H4 p95 is re-derived from the probe's opens: two ready opens, 250 and 60 ms.
  assert.equal(extract(doc, METRICS.find((m) => m.id === 'H4.p95Ms')).value, 250);
  assert.equal(extract(doc, METRICS.find((m) => m.id === 'H2.ms')).value, doc.metrics.H2.ms);
  const na = runDoc({ set: { 'metrics.H2.reached.ms': 'not-applicable', 'metrics.H2.ms': null } });
  assert.deepEqual(extract(na, METRICS[0]), { value: null, reached: 'not-applicable' });
  // The schema generator's metric list is this list.
  const schema = JSON.parse(readFileSync(PAIR_SCHEMA_PATH, 'utf8'));
  const metricEnum = schema.root.obj.metrics.arr.obj.metric.enum;
  assert.deepEqual(metricEnum, METRIC_IDS);
});

test('guard: no worse than BEFORE median + BEFORE IQR passes; beyond it fails', () => {
  const before = [3000, 3100, 3050, 2990, 3020]; // median 3020, IQR 50
  const ok = evaluate(declaredFor(), campaign({ h2Before: before, h2After: [3060, 3070, 3065, 3050, 3080] }));
  assert.equal(row(ok, 'H2.ms').verdict, 'pass');
  assert.equal(row(ok, 'H2.ms').toleranceMs, 50);
  assert.equal(row(ok, 'H2.ms').toleranceSource, 'before-iqr');
  assert.equal(row(ok, 'H2.ms').role, 'guard');
  assert.equal(ok.summary.verdict, 'pass');
  const bad = evaluate(declaredFor(), campaign({ h2Before: before, h2After: [3200, 3210, 3190, 3220, 3205] }));
  assert.equal(row(bad, 'H2.ms').verdict, 'fail');
  assert.equal(bad.summary.verdict, 'fail');
  assert.equal(row(bad, 'H1.ms').role, 'report-only');
  assert.equal(row(bad, 'H1.ms').verdict, 'report');
  assert.match(verdictTable(bad), /H2\.ms\s+guard.*fail/);
});

test('an A/A floor widens the tolerance only when declared', () => {
  const before = [3000, 3100, 3050, 2990, 3020];
  const after = [3200, 3210, 3190, 3220, 3205];
  const floors = { aaCampaignSha256: HEX('d'), values: { 'reader-v1:H2.ms': 250 } };
  const p = evaluate(declaredFor({ floors }), campaign({ h2Before: before, h2After: after }));
  assert.equal(row(p, 'H2.ms').verdict, 'pass');
  assert.equal(row(p, 'H2.ms').toleranceSource, 'aa-floor');
  assert.equal(p.floors.source, 'aa');
});

test('claim: declared ratio and p must both hold', () => {
  const claims = [{ script: 'reader-v1', metric: 'H2.ms', maxRatio: 0.75, maxP: 0.05 }];
  const before = [3000, 3100, 3050, 2990, 3020];
  const win = evaluate(declaredFor({ claims }), campaign({ h2Before: before, h2After: [2000, 2100, 2050, 1990, 2020] }));
  assert.equal(row(win, 'H2.ms').role, 'claim');
  assert.equal(row(win, 'H2.ms').verdict, 'pass');
  assert.equal(win.summary.claims.pass, 1);
  const shy = evaluate(declaredFor({ claims }), campaign({ h2Before: before, h2After: [2500, 2600, 2550, 2490, 2520] }));
  assert.equal(row(shy, 'H2.ms').verdict, 'fail');
  assert.equal(shy.summary.verdict, 'fail');
});

test('reachability: never reached on both sides is not measured; reached on one side only is incomparable', () => {
  const none = (doc) => { doc.metrics.H5.p95Ms = null; doc.metrics.H5.reached.samples = 'not-reached'; };
  const p = evaluate(declaredFor(), campaign({ mutate: none }));
  assert.equal(row(p, 'H5.p95Ms').verdict, 'not-reached');
  assert.equal(p.summary.verdict, 'pass');
  const half = evaluate(declaredFor(), campaign({ mutate: (doc, side) => { if (side === 'after') none(doc); } }));
  assert.equal(row(half, 'H5.p95Ms').verdict, 'incomparable');
  assert.equal(half.summary.verdict, 'fail');
  const na = judge({ metric: METRICS[0], script: 'passive-v1', role: 'guard', before: [runDoc({ set: { 'metrics.H2.reached.ms': 'not-applicable' } })],
    after: [runDoc({ set: { 'metrics.H2.reached.ms': 'not-applicable' } })] });
  assert.equal(na.verdict, 'not-applicable');
});

test('void: a differing digest, a failed I1, an incomplete run, a missing run, a foreign identity', () => {
  const cases = [
    [(doc, side, i) => { if (side === 'after' && i === 2) doc.equality.rankedIdsSha256 = HEX('9'); }, 'digest-differs'],
    [(doc, side, i) => { if (i === 0) doc.equality.logicalTotal = null; }, 'digest-missing'],
    [(doc, side, i) => { if (side === 'after' && i === 1) doc.metrics.I1.pass = false; }, 'i1-failed'],
    [(doc, side, i) => { if (i === 4) doc.run.outcome = 'timeout'; }, 'run-not-complete'],
    [(doc, side, i) => { if (side === 'after' && i === 3) doc.identity.machine.memGb = 32; }, 'identity-differs'],
    [(doc, side, i) => { if (side === 'after' && i === 3) doc.identity.distSha256 = HEX('f'); }, 'dist-differs-from-declared'],
    [(doc) => { doc.identity.snapshot.manifestSha256 = HEX('0'); }, 'source-differs-from-declared'],
    [(doc, side, i) => { if (i === 0) doc.equality.hitDigest = HEX('4'); }, 'digest-differs'],
  ];
  for (const [mutate, reason] of cases) {
    const p = evaluate(declaredFor(), campaign({ mutate }));
    assert.equal(p.summary.verdict, 'void', reason);
    assert.ok(p.voidReasons.includes(reason), `${reason}: ${p.voidReasons}`);
  }
  const runs = campaign();
  runs[3].doc = null;
  assert.ok(evaluate(declaredFor(), runs).voidReasons.includes('run-missing'));
  // A discarded swap redo does not count, and does not void.
  const redo = campaign();
  redo.push({ ...redo[0], slot: 99, discarded: true });
  assert.equal(evaluate(declaredFor(), redo).summary.verdict, 'pass');
  assert.equal(evaluate(declaredFor(), redo).summary.quiet.swapRedos, 1);
});

test('A/A: report-only, with a floor per gated metric', () => {
  const declared = declaredFor({ kind: 'aa', sides: { a1: SIDES.before, a2: SIDES.before } });
  const runs = campaign({ sides: ['a1', 'a2'], h2Before: [3000, 3100, 3050, 2990, 3020], h2After: [3010, 3200, 3000, 2900, 3100] });
  const p = evaluate(declared, runs);
  assert.equal(p.summary.verdict, 'aa');
  assert.ok(p.metrics.every((r) => r.role === 'report-only'));
  // floor = max(IQR a1 = 50, IQR a2 = 100, |Δ median| = 10)
  assert.equal(row(p, 'H2.ms').floorMs, 100);
  assert.equal(row(p, 'H1.ms').floorMs, undefined);
  assert.match(verdictTable(p), /floor/);
});

test('privacy: the pair document passes the ui-pair/1 whitelist; a free string does not', () => {
  const schema = JSON.parse(readFileSync(PAIR_SCHEMA_PATH, 'utf8'));
  const p = evaluate(declaredFor({ claims: [{ script: 'reader-v1', metric: 'H2.ms', maxRatio: 0.75 }] }), campaign());
  const v = validateRun(p, { schema, forbidden: ['/Users/someone/frozen/snap'] });
  assert.deepEqual(v.errors, []);
  const leaky = structuredClone(p);
  leaky.sides.before.rosettadSha256 = '/Users/someone/bin/rosettad';
  assert.equal(validateRun(leaky, { schema }).ok, false);
  const extra = structuredClone(p);
  extra.note = 'branch r15';
  assert.equal(validateRun(extra, { schema }).ok, false);
  const aa = evaluate(declaredFor({ kind: 'aa', sides: { a1: SIDES.before, a2: SIDES.before } }), campaign({ sides: ['a1', 'a2'] }));
  assert.deepEqual(validateRun(aa, { schema }).errors, []);
});

test('pair.mjs: side files, kinds, claims and the interleaved schedule', () => {
  const side = { rosettad: '/x/rosettad', rosettadSha256: HEX('a'), dist: '/x/dist', distSha256: HEX('5'), snapshot: '/x/snap', scripts: ['reader-v1', 'passive-v1'], runs: 3 };
  assert.deepEqual(sideProblems(side), []);
  assert.ok(sideProblems({ ...side, home: '/h' }).length);
  assert.ok(sideProblems({ ...side, scripts: ['reader-v2'] }).length);
  assert.ok(sideProblems({ ...side, extra: 1 }).length);
  const daemon = { ...side, rosettadSha256: HEX('b') };
  const ux = { ...side, distSha256: HEX('6') };
  assert.deepEqual(pairProblems(side, daemon), { kind: 'daemon', problems: [] });
  assert.deepEqual(pairProblems(side, ux), { kind: 'ux', problems: [] });
  assert.equal(pairProblems(side, ux, 'daemon').problems.length, 1);
  assert.equal(pairProblems(side, { ...ux, rosettadSha256: HEX('b') }).problems.length, 1);
  assert.equal(pairProblems(side, side).problems.length, 1);
  assert.equal(pairProblems(side, { ...daemon, runs: 4 }).problems.length, 1);
  assert.equal(pairProblems(side, { ...daemon, snapshot: '/y/snap' }).problems.length, 1);
  assert.equal(pairProblems(side, { ...daemon, options: { idleGuard: false } }).problems.length, 1);
  assert.deepEqual(claimProblems([{ script: 'reader-v1', metric: 'H3.h24.receiptMs', maxRatio: 0.7 }], ['reader-v1']), []);
  assert.equal(claimProblems([{ script: 'passive-v1', metric: 'H2.ms' }], ['reader-v1']).length, 1);
  assert.equal(claimProblems([{ script: 'reader-v1', metric: 'H9' }], ['reader-v1']).length, 1);
  const s = schedule({ sides: ['before', 'after'], scripts: ['reader-v1', 'passive-v1'], runs: 2 });
  assert.deepEqual(s.map((x) => `${x.script}:${x.side}`), ['reader-v1:before', 'reader-v1:after', 'passive-v1:before', 'passive-v1:after',
    'reader-v1:before', 'reader-v1:after', 'passive-v1:before', 'passive-v1:after']);
  const abba = schedule({ sides: ['before', 'after'], scripts: ['reader-v1'], runs: 2, order: 'abba' });
  assert.deepEqual(abba.map((x) => x.side), ['before', 'after', 'after', 'before']);
  assert.equal(new Set(s.map((x) => x.dir)).size, s.length);
});
