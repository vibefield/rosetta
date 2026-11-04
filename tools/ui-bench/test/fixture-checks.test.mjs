// fixture-checks.mjs, the verdict of the real-daemon fixture test, on hand-built run
// documents (no daemon, no app): a consistent run passes every check, and each planted
// fault fails the check that names it, and only that one.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { DOCUMENTED_REGIME, EXERCISED, NOT_ASSERTED, checkRun, regimeOf } from './fixture-checks.mjs';
import { HEX } from './fixtures.mjs';

const GIANT = 71063182;
const MANIFEST = { specSha256: HEX('a'), tree: { sha256: HEX('b') }, shape: { roots: 26, h24: 5, h7d: 12, giantBytes: GIANT } };
const COUNTS = { sessions: 26, projects: 4 };
const EXPECTED = { fixture: { specSha256: HEX('a'), treeSha256: HEX('b') },
  equality: { logicalTotal: HEX('1'), strictNoidRTotal: HEX('2'), sourceSetDigest: HEX('3'), rankedIdsSha256: HEX('4'), hitDigest: HEX('5'),
    finalRoots: 26, finalReadySessions: 26, counts: COUNTS },
  search: { rankedIdsSha256: HEX('6'), perCase: [{ label: 'broad-1', hits: 200 }, { label: 'medium-1', hits: 0 }] } };
const SEARCH = { rankedIdsSha256: HEX('6'), perCase: [{ label: 'broad-1', hits: 200 }, { label: 'medium-1', hits: 0 }], warmExit: 0 };
const check = (id, exercised = 1) => ({ violations: 0, exercised, pass: true });

/** A reader-v1 document with every value the checks read, all consistent, in the fixture's regime. */
function goodDoc() {
  const horizon = (roots) => ({ receiptMs: 1204.2, paintedSelMs: 8162.7, roots, daemonHorizonSessions: roots, neverRosteredExcluded: 0, daemonMs: 1203.7 });
  return {
    format: 'ui-run/1',
    identity: { snapshot: null, fixture: { specSha256: HEX('a'), treeSha256: HEX('b') } },
    run: { outcome: 'complete', outcomeReasons: [], selfTestPass: true, newestGiantKnown: true, newestGiantTreeBytes: GIANT }, selfTest: { pass: true },
    privacy: { schema: 'pass', forbiddenScan: 'pass', forbiddenSource: 'run-db', forbiddenValues: 70586 },
    clocks: { method: 'clock-line', estimatedErrorMs: 1.04 },
    daemon: { newestGiantTreeBytes: GIANT, milestonesMs: { 'canonical-ready': 1539.4 } },
    metrics: {
      H1: { ms: 1721.3, metaPushMs: 29.3, reached: { ms: 'yes' } },
      H2: { ms: 8162.7, receiptMs: 1204.2, firstDrawStanding: 'ready', flippedBeforeDrawn: true, reached: { ms: 'yes', receiptMs: 'yes' } },
      H3: { h24: horizon(5), h7d: horizon(12), reached: { 'h24.receiptMs': 'yes', 'h24.paintedSelMs': 'yes', 'h7d.receiptMs': 'yes', 'h7d.paintedSelMs': 'yes' } },
      // The last runtime-readable (a 10 ms-rounded close stamp) after the UI's receipt:
      // the documented case, not a failure.
      Hall: { receiptMs: 1204.2, paintedSelMs: 8162.7, daemonMs: 1206.5, lagVsDaemonMs: -2.3, reached: { receiptMs: 'yes', paintedSelMs: 'yes' } },
      H4: { n: 2, p50Ms: 104, maxMs: 130.9, byPhase: { reading: { n: 0 }, finalizing: { n: 0 }, postCanonical: { n: 2 } },
        pendingFace: { n: 0, p50Ms: null }, reached: { samples: 'yes', pendingFace: 'not-reached' } },
      H5: { n: 0, p50Ms: null, p95Ms: null, maxMs: null, reached: { samples: 'not-reached' } },
      H7: { invariant: { pass: false, aims: 7, aimMisses: 1, selectionJumps: 0 } },
      D2: { newestGiant: { known: true, receiptMs: 1204.2 }, notImportedSelected: [[8162.7, 0]] },
      I1: { pass: true, checks: { c1ReadyBeforeReceipt: check(), c2NeverReverts: check(), c3SelectionOnlyByInput: check(), c4NoFlashOfEmpty: check(0, 0),
        c5EndState: check(), c6Privacy: check(), c7PendingFaceCourse: check(0, 0) } },
    },
    equality: { ...EXPECTED.equality, strictNoidTotal: HEX('9'), ledgerVerifyOk: true, rankedWarmExit: 0 },
    probe: { h5: [] },
  };
}
const verdict = (doc, opts = {}) => checkRun(doc, { script: 'reader-v1', manifest: MANIFEST, expected: EXPECTED, newestFlipMs: 1203.7, lastRootFlipMs: 1203.7, search: SEARCH, ...opts });
const failing = (r) => r.checks.filter((c) => c.ok === false).map((c) => c.id);
const byId = (r, id) => r.checks.find((c) => c.id === id);

test('a consistent fixture run passes every check; the regime and H7 are recorded, not asserted', () => {
  const r = verdict(goodDoc());
  assert.deepEqual(failing(r), []);
  assert.equal(r.checks.filter((c) => c.ok === null).length, 0, 'nothing is n/a for reader-v1');
  assert.equal(r.recorded.h7Invariant.aimMisses, 1);
  assert.equal(r.recorded.hallLagVsRuntimeReadableMs, -2.3);
  assert.equal(r.recorded.regime.name, DOCUMENTED_REGIME);
});

test('each planted fault fails its own check', () => {
  const cases = [
    [(d) => { d.equality.logicalTotal = HEX('7'); }, ['expected:logicalTotal']],
    [(d) => { d.equality.counts = { ...COUNTS, projects: 5 }; }, ['expected:counts']],
    [(d) => { d.metrics.I1.pass = false; d.metrics.I1.checks.c2NeverReverts = { violations: 1, exercised: 14, pass: false }; }, ['i1-pass']],
    [(d) => { d.metrics.I1.checks.c2NeverReverts.exercised = 0; }, ['i1-exercised:c2NeverReverts']],
    [(d) => { d.metrics.H5.p50Ms = 0; }, ['reached:H5.samples']],
    [(d) => { d.metrics.H2.receiptMs = 1190; d.metrics.D2.newestGiant.receiptMs = 1190; }, ['order:H2-receipt>=daemon-flip-of-NEWEST']],
    [(d) => { d.metrics.H3.h24.receiptMs = 1200; }, ['order:H3-24h-receipt>=recent-ready-24h@newest']],
    [(d) => { d.metrics.H3.h7d.roots = 11; }, ['shape:h7d-members']],
    [(d) => { d.run.outcome = 'incomplete'; }, ['outcome-complete']],
    [(d) => { d.privacy.forbiddenSource = 'unavailable'; }, ['privacy']],
    // The scan's list held only the harness's own values (paths, user, host, queries).
    [(d) => { d.privacy.forbiddenValues = 12; }, ['privacy-scan-input']],
    // A missing ordering end fails; it is never skipped as passed.
    [(d) => { d.metrics.H1.metaPushMs = null; }, ['order:H1>=first-project-frame']],
    [(d) => { d.metrics.H3.h7d.daemonMs = null; }, ['order:H3-7d-receipt>=recent-ready-7d@newest']],
    // Every H5 sample is checked, not only its percentiles.
    [(d) => { d.probe.h5 = [[100, 95], [100, 110], [100, 120]]; Object.assign(d.metrics.H5, { n: 3, p50Ms: 10, p95Ms: 20, maxMs: 20, reached: { samples: 'yes' } }); },
      ['order:H5-samples>=0']],
    [(d) => { delete d.probe; }, ['order:H5-samples>=0']],
    // The harness's giant, the daemon's and the fixture's are one tree.
    [(d) => { d.daemon.newestGiantTreeBytes = 68000000; }, ['shape:giant-identity']],
    [(d) => { d.run.newestGiantTreeBytes = 68000000; }, ['shape:giant-identity']],
  ];
  for (const [plant, ids] of cases) {
    const d = goodDoc();
    plant(d);
    assert.deepEqual(failing(verdict(d)).sort(), ids.slice().sort(), ids.join(', '));
  }
  // The fixture-vocabulary search must find something, and find it the same way.
  assert.deepEqual(failing(verdict(goodDoc(), { search: { ...SEARCH, perCase: [{ label: 'broad-1', hits: 0 }, { label: 'medium-1', hits: 0 }] } })).sort(),
    ['expected:fixtureSearch', 'search:fixture-workload']);
  assert.deepEqual(failing(verdict(goodDoc(), { search: { ...SEARCH, rankedIdsSha256: HEX('7') } })), ['expected:fixtureSearch']);
  assert.deepEqual(failing(verdict(goodDoc(), { search: { error: 'warm start never reported ready' } })).sort(), ['expected:fixtureSearch', 'search:fixture-workload']);
  // The H-all receipt is held to the daemon's last root flip, within the clock error.
  assert.deepEqual(failing(verdict(goodDoc(), { lastRootFlipMs: 1210 })), ['order:Hall-receipt>=daemon-last-root-flip']);
  // Without the daemon's flips the orderings that need them fail, never pass.
  assert.deepEqual(failing(verdict(goodDoc(), { newestFlipMs: null })), ['order:H2-receipt>=daemon-flip-of-NEWEST']);
  assert.deepEqual(failing(verdict(goodDoc(), { lastRootFlipMs: null })).sort(),
    ['order:Hall-receipt>=daemon-last-root-flip', 'order:last-runtime-readable>=daemon-last-root-flip']);
});

test('passive-v1: no hit digest, only c3 must examine something, and only its declared ordering is n/a', () => {
  const d = goodDoc();
  d.equality.hitDigest = null;
  Object.assign(d.metrics.H2, { ms: null, firstDrawStanding: 'none', flippedBeforeDrawn: null, reached: { ms: 'not-applicable', receiptMs: 'yes' } });
  d.metrics.D2.notImportedSelected = [];
  for (const k of ['c1ReadyBeforeReceipt', 'c2NeverReverts', 'c5EndState', 'c6Privacy']) d.metrics.I1.checks[k].exercised = 0;
  const r = verdict(d, { script: 'passive-v1' });
  assert.deepEqual(failing(r), []);
  assert.deepEqual(r.checks.filter((c) => c.ok === null).map((c) => c.id), ['order:H2-painted>=H2-receipt']);
  assert.equal(r.recorded.regime.name, DOCUMENTED_REGIME);
  // The same missing end is a failure when the metric does not say not-applicable.
  d.metrics.H2.reached.ms = 'not-reached';
  assert.deepEqual(failing(verdict(d, { script: 'passive-v1' })), ['order:H2-painted>=H2-receipt']);
});

test('constant and redundant I1 counts are not asserted; every check is either asserted or explained', () => {
  for (const script of Object.keys(EXERCISED)) {
    assert.ok(!EXERCISED[script].includes('c6Privacy') && !EXERCISED[script].includes('c5EndState') && !EXERCISED[script].includes('c1ReadyBeforeReceipt'), script);
  }
  const all = ['c1ReadyBeforeReceipt', 'c2NeverReverts', 'c3SelectionOnlyByInput', 'c4NoFlashOfEmpty', 'c5EndState', 'c6Privacy', 'c7PendingFaceCourse'];
  for (const k of all) assert.ok(EXERCISED['reader-v1'].includes(k) || NOT_ASSERTED[k], k);
});

test('the regime is read from the document, never asserted', () => {
  const d = goodDoc();
  assert.equal(regimeOf(d).name, 'import-done-before-first-paint');
  d.daemon.milestonesMs['canonical-ready'] = 1800;
  assert.equal(regimeOf(d).name, 'flips-done-before-first-paint');
  d.metrics.D2.notImportedSelected = [[1750, 3], [1900, 0]];
  assert.equal(regimeOf(d).name, 'pending-painted');
  assert.equal(regimeOf(d).notImportedDrawnMax, 3);
  // Another regime alone fails nothing.
  assert.deepEqual(failing(verdict(d)), []);
  delete d.daemon.milestonesMs['canonical-ready'];
  d.metrics.D2.notImportedSelected = [];
  assert.equal(regimeOf(d).name, 'unknown');
});
