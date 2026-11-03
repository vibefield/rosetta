// metrics.mjs and daemon-lines.mjs over synthetic probe and daemon logs (fixtures.mjs):
// every metric's derivation, the clock alignment and its fallbacks, and each way I1
// can fail. No snapshot, no live directory.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { allReadableS, parseDaemonLog } from '../daemon-lines.mjs';
import { alignClocks, derive, pct } from '../metrics.mjs';
import { W0, at, daemonStdout, mainNotes, probeLog } from './fixtures.mjs';

const pings = { start: { offsetMs: 0.05, rttMs: 0.2 }, end: { offsetMs: 0.06, rttMs: 0.1 } };
const run = (probeOver = {}, { daemon = daemonStdout(), main = mainNotes(), notApplicable = [] } = {}) =>
  derive({ probe: probeLog(probeOver), main, daemon: parseDaemonLog(daemon), mainPings: pings, notApplicable });

test('daemon lines: clock, milestones (precise JSON twins win), admissions, peak', () => {
  const d = parseDaemonLog(daemonStdout());
  assert.equal(d.clockWallMs, W0);
  assert.equal(d.milestones['consent-granted'].elapsedS, 10);
  assert.equal(d.milestones['consent-granted'].precise, true);
  assert.equal(d.milestones['recent-ready:24h@newest'].elapsedS, 11.0004);
  assert.equal(d.milestones['recent-ready:24h@newest'].horizonSessions, 6);
  assert.equal(d.milestones['metadata-ready'].precise, false);
  assert.equal(d.milestones['runtime-readable:claude-code'].sessions, 10);
  assert.equal(d.runtimeReadableSessions, 12);
  assert.equal(d.milestones['newest-giant-ready'].treeBytes, 900000000);
  assert.equal(d.unknownMilestones, 1);
  assert.equal(d.treeAdmissions.length, 3);
  assert.deepEqual(d.treeAdmissions[1], { elapsedS: 11, publishedS: 11.0005, trees: 1, sessions: 3, roots: 1, wallMs: 1.5, lockHeldMs: 0.7 });
  assert.deepEqual(d.memoryPeak, { footprintMb: 800, residentMb: 900, swapUsedMb: null, swapouts: 0, pageouts: 3 });
  assert.equal(d.panicLines, 1);
  assert.equal(allReadableS(d), 12.5);
  assert.equal(d.metadataPublicSessions, 12);
  assert.equal(d.metadataPublicLines, 2);
  // The receipt cross-check uses unrounded JSON stamps only (2 milestone twins and 3
  // tree admissions here), never a text line's two decimals.
  assert.equal(d.receiptLagMs.length, 5);
  assert.ok(Math.min(...d.receiptLagMs) >= 0);
});

test('clocks: a negative receipt lag on precise stamps widens the error estimate', () => {
  const late = daemonStdout({ extra: [`${(W0 + 11_000).toFixed(3)}\t{"elapsedS":11.004,"event":"milestone","milestone":"first-session-complete"}`] });
  const c = run({}, { daemon: late }).clocks;
  assert.equal(c.daemonReceiptLagMs.min, -4);
  assert.ok(c.estimatedErrorMs >= 4);
});

test('clocks: the clock line anchors T0 at consent-granted; offsets apply only above 2 ms', () => {
  const c = run();
  assert.equal(c.clocks.method, 'clock-line');
  assert.equal(c.clocks.t0Kind, 'consent-granted');
  assert.equal(c.clocks.rendererOffsetApplied, false);
  assert.equal(c.clocks.mainOffsetApplied, false);
  assert.equal(c.clocks.roundedMilestones > 0, true);
  const shifted = derive({ probe: probeLog(), main: mainNotes({ offsets: { start: { offsetMs: 5, rttMs: 0.2 }, end: null } }),
    daemon: parseDaemonLog(daemonStdout()), mainPings: pings });
  assert.equal(shifted.clocks.rendererOffsetApplied, true);
  assert.equal(shifted.metrics.H1.ms, 2045);
});

test('clocks: without the clock line, metadata_ready_at anchors; without consent, the click', () => {
  const noClock = run({}, { daemon: daemonStdout({ clock: false }) });
  assert.equal(noClock.clocks.method, 'metadata-ready-anchor');
  assert.equal(noClock.metrics.H1.ms, 2050);
  const d = parseDaemonLog(daemonStdout({ consent: false }));
  const c = alignClocks({ daemon: d, probe: probeLog(), main: mainNotes(), mainPings: pings });
  assert.equal(c.report.t0Kind, 'begin-click');
  assert.equal(c.imp(at(0)), 2);
});

test('H1 is the columns paint, split into its meta-push and status-poll parts', () => {
  const { H1 } = run().metrics;
  assert.deepEqual(H1, { role: 'guard', ms: 2050, projects: 3, metaPushMs: 60, statusPollMs: 2000, lagVsMetadataReadyMs: 2000, reached: { ms: 'yes' } });
  assert.equal(run({ h1: null }).metrics.H1.reached.ms, 'not-reached');
});

test('H2 paints NEWEST ready; receipt, lag and held-ready ride beside it', () => {
  const { H2 } = run().metrics;
  assert.equal(H2.role, 'claim-candidate');
  assert.equal(H2.ms, 2400);
  assert.equal(H2.receiptMs, 1000.5);
  // Flipped before it was ever drawn: the lag runs from the first draw, the raw
  // receipt → paint difference is kept beside it.
  assert.equal(H2.flippedBeforeDrawn, true);
  assert.equal(H2.lag.ms, 0);
  assert.equal(H2.lag.rawMs, 1399.5);
  assert.equal(H2.lag.role, 'guard');
  assert.equal(H2.heldReadyMs, 40);
  // Drawn not imported first, then flipped: the lag is receipt → paint.
  const drawnFirst = run({ h2: { firstDrawAt: at(900), receiptAt: at(1000.5), paintReadyAt: at(1050) } }).metrics.H2;
  assert.equal(drawnFirst.flippedBeforeDrawn, false);
  assert.equal(drawnFirst.lag.ms, 49.5);
});

test('H2: never drawn means held-ready is unknown, and n/a for a script that selects nothing', () => {
  const never = run({ h2: { firstDrawAt: null, paintReadyAt: null, firstDrawStanding: null, indexAtFirstDraw: null, heldReadyMs: 0 } }, { notApplicable: ['H2.ms'] }).metrics.H2;
  assert.equal(never.heldReadyMs, null);
  assert.equal(never.lag.ms, null);
  assert.deepEqual(never.reached, { ms: 'not-applicable', receiptMs: 'yes' });
  assert.equal(run({ h2: { paintReadyAt: null } }).metrics.H2.reached.ms, 'not-reached');
});

test('H3 reports the receipt and painted legs apart, with the daemon horizon beside them', () => {
  const { H3 } = run().metrics;
  assert.equal(H3.h24.receiptMs, 1001);
  assert.equal(H3.h24.paintedSelMs, 2400);
  assert.equal(H3.h24.daemonHorizonSessions, 6);
  assert.equal(H3.h24.daemonMs, 1000.4);
  assert.equal(H3.h24.lagVsDaemonMs, 0.6);
  assert.equal(H3.h7d.roots, 5);
  assert.equal(H3.h7d.rootsInSelected, 3);
  assert.equal(H3.reached['h24.receiptMs'], 'yes');
  // An empty frozen set has no receipt: null and n/a, never the freeze time.
  const empty = run({ h3: { h24ReceiptAt: null, n24: 0, n24Sel: 0, h24PaintSelAt: null } }).metrics.H3;
  assert.equal(empty.h24.receiptMs, null);
  assert.equal(empty.reached['h24.receiptMs'], 'not-applicable');
  assert.equal(empty.reached['h24.paintedSelMs'], 'not-applicable');
  // Never-rostered members excluded at the end are counted beside the legs.
  const ex = run({ h3: { excluded24: 1, h24ViaCaptureOver: true } }).metrics.H3;
  assert.equal(ex.h24.neverRosteredExcluded, 1);
  assert.equal(ex.h24.viaCaptureOver, true);
});

test('H-all pairs the last root flip received with the last runtime-readable', () => {
  const { Hall } = run().metrics;
  assert.equal(Hall.receiptMs, 2500.3);
  assert.equal(Hall.daemonMs, 2500);
  assert.equal(Hall.lagVsDaemonMs, 0.3);
  assert.equal(Hall.paintedSelMs, 2600);
});

test('H4 counts ready opens by phase and keeps the opened-while-Pending face apart', () => {
  const { H4 } = run().metrics;
  assert.equal(H4.n, 2);
  assert.equal(H4.p50Ms, 60);
  assert.equal(H4.maxMs, 250);
  assert.deepEqual(H4.byPhase.reading, { n: 1, p50Ms: 250, maxMs: 250 });
  assert.deepEqual(H4.byPhase.postCanonical, { n: 1, p50Ms: 60, maxMs: 60 });
  assert.equal(H4.pendingFace.n, 1);
  assert.equal(H4.pendingFace.p50Ms, 30);
});

test('H5 is receipt → paint per flipped drawn root; the wire leg is a diagnostic', () => {
  const m = run().metrics;
  assert.deepEqual({ ...m.H5, role: undefined }, { role: undefined, n: 2, p50Ms: 20, p95Ms: 40, maxMs: 40, reached: { samples: 'yes' } });
  assert.equal(run({ h5: [] }).metrics.H5.reached.samples, 'not-reached');
  assert.equal(m.D3.wire.n, 3);
  assert.equal(m.D3.wire.maxMs, 0.1);
});

test('H6 bins long frames by import window and pads unreported inputs at 16 ms', () => {
  const { H6 } = run().metrics;
  assert.deepEqual(H6.byWindow.streaming, { wallMs: 2500, longestMs: 120, blockingMs: 70, blockingPct: 2.8, n100: 1, nLoaf: 1 });
  assert.equal(H6.byWindow.finalizing.longestMs, 60);
  assert.equal(H6.byWindow.post.nLoaf, 1);
  assert.equal(H6.byWindow.post.wallMs, 5200);
  assert.deepEqual({ ...H6.input, role: undefined }, { role: undefined, scripted: 4, reported: 2, p98Ms: 56, maxMs: 56, unreportedAsMs: 16 });
  assert.equal(H6.report.role, 'report-only');
  assert.equal(H6.report.heapPeakMb, 40);
  assert.equal(H6.report.probeMainPctOfWall, 0.2);
  assert.equal(H6.report.probeWithinBudget, true);
  assert.equal(H6.report.probeCostKind, 'js-self-time');
  assert.deepEqual(H6.reached, { longestFrame: 'yes', input: 'yes' });
});

test('H6 without its observers is unknown (null), never 0, and the self-test fails', () => {
  const m = run({ selfTest: { loaf: false } });
  assert.equal(m.selfTest.pass, false);
  assert.equal(m.metrics.H6.byWindow.streaming.longestMs, null);
  assert.equal(m.metrics.H6.input.p98Ms, null);
  assert.equal(m.metrics.H6.reached.longestFrame, 'not-reached');
  for (const k of ['eventTiming', 'layoutShift']) assert.equal(run({ selfTest: { [k]: false } }).selfTest.pass, false, k);
  assert.equal(run({ selfTest: { worker: 'failed' } }).selfTest.pass, false);
  assert.equal(run({ selfTest: { layoutShift: false } }).metrics.H7.layoutShift, null);
});

test('H7 counts unrequested moves and makes aim misses and selection jumps invariant-grade', () => {
  const ok = run().metrics.H7;
  assert.equal(ok.projectsMoves, 2);
  assert.deepEqual(ok.invariant, { role: 'invariant', pass: true, selectionJumps: 0, aims: 2, aimMisses: 0, missedOtherRow: 0, missedNoRow: 0, missedTargetShiftPx: [] });
  assert.deepEqual(ok.discreteInput, { role: 'report-only', projectsMoves: 3, projectsPx: 45, sessionsMoves: 0, sessionsPx: 0, selectionJumps: 0 });
  assert.deepEqual(ok.reached, { moves: 'yes', aims: 'yes' });
  const miss = run({ h7: { aims: [[at(1), 'begin', 0, 0, 'none'], [at(2), 'newest-row', 0, 34, 'other-row'], [at(3), 'ready-next', 1, 0, 'aimed']] } }).metrics.H7;
  assert.equal(miss.invariant.aims, 2);
  assert.equal(miss.invariant.aimMisses, 1);
  assert.equal(miss.invariant.missedOtherRow, 1);
  assert.deepEqual(miss.invariant.missedTargetShiftPx, [34]);
  assert.equal(miss.invariant.pass, false, 'an aim miss fails the invariant');
  assert.equal(run({ h7: { selectionJumps: 1 } }).metrics.H7.invariant.pass, false, 'a selection jump fails the invariant');
  // Lists never drawn: the move counts are unknown, not 0.
  const noCols = run({ h1: null }).metrics.H7;
  assert.equal(noCols.projectsMoves, null);
  assert.equal(noCols.reached.moves, 'not-reached');
  assert.equal(run({}, { notApplicable: ['H7.aims'] }).metrics.H7.reached.aims, 'not-applicable');
});

test('H8: charts by scope, search on the import clock, a deterministic UI hit digest', () => {
  const a = run().metrics.H8;
  const b = run().metrics.H8;
  assert.equal(a.chartsSelectedMs, 3600);
  assert.equal(a.chartsSelectedLagMs, 600);
  assert.equal(a.chartsAllLagMs, 700);
  assert.equal(a.portfolioMs, 8300);
  // The guard runs from the later of the UI's search-ready receipt (3300) and the
  // last key (3450); the import-clock hit and its daemon lag are report-only.
  assert.equal(a.searchLagMs, 200);
  assert.equal(a.report.searchHitMs, 3650);
  assert.equal(a.report.searchAfterDaemonReadyMs, 450);
  assert.equal(a.report.typedAfterUiReadyMs, 150);
  assert.equal(run({ h8: { search: [{ q: 0, beginAt: at(2900), typedAt: at(3000), firstFulltextAt: at(3650), firstTitleAt: null, quietAt: at(4200), rows: 12, fulltextRows: 12, digest: 'a'.repeat(64) }] } })
    .metrics.H8.searchLagMs, 350, 'typed before the UI knew search was ready: timed from that receipt');
  assert.deepEqual(a.typedToHitMs, [200, 150]);
  assert.equal(a.reached.searchLagMs, 'yes');
  assert.equal(run({ h8: { search: [] } }, { notApplicable: ['H8.searchLagMs'] }).metrics.H8.reached.searchLagMs, 'not-applicable');
  assert.match(a.hitDigest, /^[0-9a-f]{64}$/);
  assert.equal(a.hitDigest, b.hitDigest);
  const other = run({ h8: { search: [{ q: 0, beginAt: at(1), typedAt: at(2), firstFulltextAt: at(3), firstTitleAt: null, quietAt: at(4), rows: 1, fulltextRows: 1, digest: 'c'.repeat(64) }] } });
  assert.notEqual(other.metrics.H8.hitDigest, a.hitDigest);
});

test('I1 passes on an honest run, with metadata-only and never-rostered roots excluded', () => {
  assert.equal(run().metrics.I1.pass, true);
  const excluded = run({ end: { pendingRoots: 2, neverRostered: 2, metaOnlyRoots: 1, daemonPending: 3 } }).metrics.I1;
  assert.equal(excluded.checks.c5EndState.pass, true);
  assert.equal(excluded.pass, true);
});

test('I1 fails on each honesty breach', () => {
  const fails = (over, check, main) => {
    const m = derive({ probe: probeLog(over), main: main ?? mainNotes(), daemon: parseDaemonLog(daemonStdout()), mainPings: pings }).metrics.I1;
    assert.equal(m.pass, false, check);
    assert.equal(m.checks[check].pass, false, check);
  };
  fails({ i1: { c1Before: 1 } }, 'c1ReadyBeforeReceipt');
  fails({ i1: { c1PendingReady: 1 } }, 'c1ReadyBeforeReceipt');
  fails({ i1: { c2: 1 } }, 'c2NeverReverts');
  fails({ end: { retractions: 1 } }, 'c2NeverReverts');
  fails({ i1: { c3: 1 } }, 'c3SelectionOnlyByInput');
  fails({ i1: { c4: 1 } }, 'c4NoFlashOfEmpty');
  fails({ i1: { c7: 1 } }, 'c7PendingFaceCourse');
  fails({ end: { pendingRoots: 1, daemonPending: 0 } }, 'c5EndState');
  fails({ end: { projectMismatches: 1 } }, 'c5EndState');
  fails({ renderedNewestProjectRows: 2 }, 'c5EndState');
  fails({ status: { last: { done: 11 } } }, 'c5EndState');
  fails({ end: { readyAll: 11 } }, 'c5EndState');
  fails({ end: { membershipStable: false } }, 'c5EndState');
  fails({ status: { kinds: ['claude-code', 'codex', 'opencode'] } }, 'c5EndState');
  fails({ end: { daemonTruncated: true } }, 'c5EndState');
});

test('I1(5) checks the never-rostered exclusion against the daemon, not against itself', () => {
  // A rostered root stuck Pending: the UI calls it never-rostered, but the daemon's
  // skims published 12 sessions and its roster planned 12, so none may stay Pending.
  const stuck = run({ end: { pendingRoots: 1, neverRostered: 1, daemonPending: 1, pendingSessions: 1, readyAll: 11 } }).metrics.I1.checks.c5EndState;
  assert.equal(stuck.pass, false);
  assert.equal(stuck.daemonNeverRosteredSessions, 0);
  // Truly never rostered: 13 published, 12 planned, one Pending at the end.
  const extra = daemonStdout({ extra: [`${(W0 + 10_046).toFixed(3)}\tpi: metadata public — 1 sessions, 1 projects, 0 streamable files`] });
  const honest = run({ end: { pendingRoots: 1, neverRostered: 1, daemonPending: 1, pendingSessions: 1, rootsSeen: 7 } }, { daemon: extra }).metrics.I1.checks.c5EndState;
  assert.equal(honest.daemonNeverRosteredSessions, 1);
  assert.equal(honest.pass, true);
  // The UI's ready sessions must equal the roster (Σ sessions_total).
  assert.equal(run({ status: { last: { total: 13, done: 13 } } }).metrics.I1.checks.c5EndState.pass, false);
  // Without the daemon's metadata line there is no evidence: a violation, not a pass.
  const noLine = daemonStdout().split('\n').filter((l) => !l.includes('metadata public')).join('\n');
  const missing = run({}, { daemon: noLine }).metrics.I1.checks.c5EndState;
  assert.equal(missing.exercised, 0);
  assert.equal(missing.pass, false);
});

test('I1 says how much each check examined', () => {
  const c = run().metrics.I1.checks;
  assert.equal(c.c1ReadyBeforeReceipt.exercised, 6);
  assert.equal(c.c2NeverReverts.exercised, 6);
  assert.equal(c.c3SelectionOnlyByInput.exercised, 40);
  assert.equal(c.c3SelectionOnlyByInput.selectionChanges, 2);
  assert.equal(c.c4NoFlashOfEmpty.exercised, 2);
  assert.equal(c.c5EndState.exercised, 1);
  assert.equal(c.c7PendingFaceCourse.exercised, 1);
  // The discrete-input reading of (3) is reported, not gated, until the user decides.
  const d = run({ i1: { c3Discrete: 1 } }).metrics.I1;
  assert.equal(d.checks.c3SelectionOnlyByInput.violationsDiscreteInput, 1);
  assert.equal(d.pass, true);
});

test('the self-test fails without a first meta frame', () => {
  assert.equal(run().selfTest.pass, true);
  assert.equal(run({ selfTest: { firstMetaAt: null } }).selfTest.pass, false);
});

test('a diagnostic whose precondition never happened is null, not 0', () => {
  const m = run({ d1: { sheetAfterSearch: false }, d2: { selShown: false }, frozen: null }).metrics;
  assert.equal(m.D1.titleRowsAfterSearchMs, null);
  assert.equal(m.D2.inversionSelectedMs, null);
  assert.equal(m.D2.inversionCorpusMs, null);
  assert.equal(run().metrics.D1.titleRowsAfterSearchMs, 0);
});

test('D1–D3 diagnostics are report-only and derived from the same logs', () => {
  const m = run().metrics;
  for (const d of ['D1', 'D2', 'D3']) assert.equal(m[d].role, 'report-only');
  assert.equal(m.D1.consentToProgressMs, 2052);
  assert.equal(m.D1.beginToConsentGrantedMs, 2);
  assert.equal(m.D1.readyCountLag.n, 3);
  assert.equal(m.D1.readyCountLag.maxMs, 1950);
  assert.deepEqual(m.D1.countUnitRatio, { at25: null, at50: null, at100: 2 });
  assert.equal(m.D1.phaseSeenLagMs.canonical, 150);
  assert.equal(m.D1.appLaunch.windowMs, 100);
  // [H1 + target, actual offset after H1, rank, unranked]; the 30 s sample came after the end.
  assert.deepEqual(m.D2.newestProjectRank, [[0, 0, 4, 2], [5000, 5000, 0, 0], [30000, null, null, null]]);
  assert.equal(m.D2.newestGiant.lagVsDaemonMs, 1);
  assert.equal(m.D3.opens[0].helloToSnapshotMs, 191);
  assert.equal(m.D3.opens[1].readerWaitMs, null);
});

test('percentiles are nearest-rank', () => {
  assert.equal(pct([5, 1, 3, 2, 4], 50), 3);
  assert.equal(pct([5, 1, 3, 2, 4], 100), 5);
  assert.equal(pct([], 50), null);
});
