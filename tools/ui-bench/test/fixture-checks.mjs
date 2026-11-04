// The verdict of one fixture run (fixture-run.mjs): invariants and digests only, never
// a timing threshold. Pure: it reads the run's ui-run.json, the fixture's manifest, the
// committed expected digests, the daemon's flips of NEWEST and of the last root
// (fixture-run.mjs finds them in the kept daemon stdout) and the fixture search digest,
// and returns every check with its verdict and detail.
//
// The regime this fixture runs in (REGIME, recorded per run, never asserted: it is the
// order of two timings nothing guarantees): the whole import, canonical readiness
// included, is over before the app paints its first columns (H1 waits for the roll
// call's 2.5 s status poll, ux:X7; the fixture imports in under 2 s). So no row is ever
// painted Pending: the test covers flip delivery through the real daemon, SDK, hooks and
// renderer, the end state, the orderings, privacy and the digests — not Pending paint.
// What that leaves unreachable (c1's breach class, c4, c7, H5, the opened-while-Pending
// face) is listed in NOT_ASSERTED; fixture-run.mjs prints the regime and flags a run
// whose regime differs.
//
// Asserted:
//   - the run: outcome complete, probe self-test, privacy gate (from the run db's list,
//     which held at least the db's own session, native and project ids);
//   - I1 passes, and each check in EXERCISED examined something;
//   - every H1–H5 / H-all `reached` entry agrees with its value: a number with `yes`,
//     null with `not-reached` or `not-applicable`;
//   - orderings (err = clocks.estimatedErrorMs): H1 ≥ the first project frame received;
//     H2 painted ≥ H2 receipt ≥ the daemon's flip of NEWEST − err; each H3 receipt ≥ its
//     `recent-ready …@newest` − err; H-all receipt ≥ the daemon's last root flip − err,
//     and that flip ≤ the last `runtime-readable` (a 10 ms-rounded stamp taken at the
//     runtime's close, so the UI may receive the flip before it: the receipt's lag after
//     it is recorded, not asserted); every H5 sample ≥ 0. A missing end FAILS the
//     ordering, unless ORDER_NOT_APPLICABLE declares it for the script and the metric
//     says `not-applicable`;
//   - the fixture's shape as the UI and the daemon saw it: the harness's newest giant,
//     the daemon's and the fixture's are one tree (tree bytes), NEWEST's receipt is the
//     giant's, and each @newest horizon holds the roots the spec puts in it, for both;
//   - the final digests equal the expected file (PINNED: logical TOTAL, strict_noid
//     TOTAL_R, sourceSetDigest, ranked ids, UI hit digest, roots, ready sessions, counts),
//     and so does the fixture-vocabulary ranked digest (fixture-search.mjs), which must
//     find something: the R1 workload and reader-v1's queries find nothing here, so
//     their pinned digests are those of empty hit lists.
// Recorded, not asserted: the regime; H7's invariant (selection jumps, aim misses; on the
// R1 snapshot reader-v1 misses at the end-of-import reorder, ux:X9); how much each I1
// check examined.

/** I1 checks that must examine something on the fixture, per script. */
export const EXERCISED = {
  'reader-v1': ['c2NeverReverts', 'c3SelectionOnlyByInput'],
  'passive-v1': ['c3SelectionOnlyByInput'],
};
/** Why the other I1 checks are not asserted to examine something (they must still pass). */
export const NOT_ASSERTED = {
  c1ReadyBeforeReceipt: 'its breach class (a Pending row painted ready, or a row painted ready before its receipt) needs a row '
    + 'drawn while still Pending; in this regime every flip is received before the first paint, so its count is of rows first '
    + 'drawn already ready (a planted Pending-painted-ready breach passes here); passive-v1 draws no session row at all',
  c2NeverReverts: 'passive-v1 selects no project, so no session row is drawn (reader-v1: asserted; a row drawn ready can still '
    + 'revert after the first paint, though not during the import, which is over by then)',
  c4NoFlashOfEmpty: 'counts opens before the status says live; the import is live before the reader\'s first open',
  c5EndState: 'asserted through i1-pass: its count is 0 exactly when its evidence is missing, which it already counts as a violation',
  c6Privacy: 'run_ui sets its count to 1 whenever the gate ran, and a document exists only if the gate passed; what the scan had '
    + 'to look for is asserted by privacy-scan-input',
  c7PendingFaceCourse: 'needs a row still not imported when the reader opens one; every root is readable before the columns are up',
};
/** Orderings whose missing end the script declares: id → the `reached` key that must say not-applicable. */
export const ORDER_NOT_APPLICABLE = {
  'passive-v1': { 'order:H2-painted>=H2-receipt': 'H2.ms' },
};
/** Values the fixture makes happen whatever the timing (metric.reached keys), per script:
 * NEWEST, both horizons and every root become ready, and reader-v1 selects NEWEST's
 * project and opens ready rows. H5 and the opened-while-Pending face need a flip while
 * the row is drawn, which this regime does not leave room for. */
export const MUST_REACH = {
  'reader-v1': ['H1.ms', 'H2.ms', 'H2.receiptMs', 'H3.h24.receiptMs', 'H3.h24.paintedSelMs', 'H3.h7d.receiptMs', 'H3.h7d.paintedSelMs',
    'Hall.receiptMs', 'Hall.paintedSelMs', 'H4.samples'],
  'passive-v1': ['H1.ms', 'H2.receiptMs', 'H3.h24.receiptMs', 'H3.h7d.receiptMs', 'Hall.receiptMs'],
};
/** The equality fields the expected file pins (counts compared whole). strict_noid is
 * pinned as TOTAL_R, without its ingest_cursor_file_evidence component: that component
 * holds each cursor's tail checkpoint, which encodes the file's device and inode
 * (capture FileIdentity::Unix), so it changes whenever the home is regenerated, at any
 * location. Every other strict_noid component, cursor paths included, is pinned. */
export const PINNED = ['logicalTotal', 'strictNoidRTotal', 'sourceSetDigest', 'rankedIdsSha256', 'hitDigest', 'finalRoots', 'finalReadySessions'];
/** The regime the docs describe (README, "Fixture test"); fixture-run.mjs flags any other. */
export const DOCUMENTED_REGIME = 'import-done-before-first-paint';

const isNum = (x) => typeof x === 'number' && Number.isFinite(x);
const get = (o, dotted) => dotted.split('.').reduce((v, k) => (v == null ? undefined : v[k]), o);

/** The value a metric's `reached` key speaks for. */
function reachedValue(metric, key) {
  if (key === 'samples') return metric.p50Ms; // H4 and H5: their distribution's p50
  if (key === 'pendingFace') return metric.pendingFace?.p50Ms;
  return get(metric, key);
}
/** The `reached` state of a dotted metric key ('H2.ms', 'H3.h24.receiptMs'). */
function reachedOf(m, dotted) {
  const [name, ...rest] = dotted.split('.');
  return m[name]?.reached?.[rest.join('.')];
}

/** The digests a run yields, in the expected file's shape. */
export function digestsOf(doc) {
  const e = doc.equality || {};
  const out = {};
  for (const k of PINNED) out[k] = e[k] ?? null;
  out.counts = e.counts ?? null;
  return out;
}

/** Per-generation digests: equal only between runs over one generated home (same inodes). */
export function sameHomeDigestsOf(doc) {
  return { strictNoidTotal: doc.equality?.strictNoidTotal ?? null };
}

/**
 * The regime a run ran in, from its document: `pending-painted` when a session row was
 * ever drawn not imported (or NEWEST was first drawn so); otherwise
 * `import-done-before-first-paint` when canonical readiness came no later than H1, or
 * `flips-done-before-first-paint` when it came later; `unknown` without both values.
 */
export function regimeOf(doc) {
  const m = doc.metrics || {};
  const cr = doc.daemon?.milestonesMs?.['canonical-ready'] ?? null;
  const h1 = m.H1?.ms ?? null;
  const drawn = (m.D2?.notImportedSelected || []).map((x) => x?.[1]).filter(isNum);
  const notImportedDrawnMax = drawn.length ? Math.max(...drawn) : 0;
  const first = m.H2?.firstDrawStanding ?? 'none';
  const pendingPainted = notImportedDrawnMax > 0 || first === 'not-imported';
  const name = pendingPainted ? 'pending-painted'
    : !isNum(cr) || !isNum(h1) ? 'unknown'
      : cr <= h1 ? 'import-done-before-first-paint' : 'flips-done-before-first-paint';
  const phase = m.H4?.byPhase || {};
  return { name, canonicalReadyMs: cr, h1Ms: h1, newestFirstDrawStanding: first, flippedBeforeDrawn: m.H2?.flippedBeforeDrawn ?? null,
    notImportedDrawnMax, readyOpensBeforeCanonical: (phase.reading?.n ?? 0) + (phase.finalizing?.n ?? 0),
    pendingFaceOpens: m.H4?.pendingFace?.n ?? null, h5n: m.H5?.n ?? null };
}

const sortKeys = (o) => (o && typeof o === 'object' && !Array.isArray(o) ? Object.fromEntries(Object.keys(o).sort().map((k) => [k, sortKeys(o[k])])) : o);
const same = (a, b) => JSON.stringify(sortKeys(a)) === JSON.stringify(sortKeys(b));

/**
 * checkRun(doc, { script, manifest, expected, newestFlipMs, lastRootFlipMs, search }) → { checks, recorded }.
 * `newestFlipMs`: the daemon's flip of NEWEST on the import clock (the publishedS of the
 * tree-admission batch that made NEWEST, the newest giant, readable); `lastRootFlipMs`:
 * the publishedS of the last batch that flipped a root; null when unknown (the orderings
 * that need them then fail). `search`: fixture-search.mjs's result over this run's db
 * (undefined: not checked). Each check: { id, ok, detail }; ok null only for an ordering
 * the script declares not applicable (ORDER_NOT_APPLICABLE).
 */
export function checkRun(doc, { script, manifest, expected, newestFlipMs = null, lastRootFlipMs = null, search = undefined }) {
  const checks = [];
  const add = (id, ok, detail = '') => checks.push({ id, ok, detail });
  const m = doc.metrics || {};
  const r = doc.run || {};
  const counts = doc.equality?.counts || {};

  add('outcome-complete', r.outcome === 'complete', `outcome ${r.outcome}${(r.outcomeReasons || []).length ? ` (${r.outcomeReasons.join(', ')})` : ''}`);
  add('self-test', r.selfTestPass === true && doc.selfTest?.pass === true);
  add('privacy', doc.privacy?.schema === 'pass' && doc.privacy?.forbiddenScan === 'pass' && doc.privacy?.forbiddenSource === 'run-db',
    `schema ${doc.privacy?.schema}, scan ${doc.privacy?.forbiddenScan}, list from ${doc.privacy?.forbiddenSource}`);
  // The scan had real values to look for: at least each session's id and native id and
  // each project's id from the run's db (the list holds more: cwds, titles, timeline ids).
  const minForbidden = isNum(counts.sessions) && isNum(counts.projects) ? 2 * counts.sessions + counts.projects : null;
  add('privacy-scan-input', minForbidden !== null && minForbidden > 0 && isNum(doc.privacy?.forbiddenValues) && doc.privacy.forbiddenValues >= minForbidden,
    `${doc.privacy?.forbiddenValues} forbidden values scanned, at least ${minForbidden} (2 × ${counts.sessions} sessions + ${counts.projects} projects) needed`);
  add('fixture-identity', doc.identity?.snapshot === null && doc.identity?.fixture?.treeSha256 === manifest.tree.sha256
    && doc.identity?.fixture?.specSha256 === manifest.specSha256, 'the run read this fixture, verified');

  // I1 and how much each check examined.
  const i1 = m.I1?.checks || {};
  const failing = Object.entries(i1).filter(([, c]) => c.pass !== true).map(([k, c]) => `${k}: ${c.violations} violation(s)`);
  add('i1-pass', m.I1?.pass === true && failing.length === 0, failing.join('; ') || 'every check 0 violations');
  for (const k of EXERCISED[script] || []) add(`i1-exercised:${k}`, isNum(i1[k]?.exercised) && i1[k].exercised > 0, `exercised ${i1[k]?.exercised}`);

  // reached ↔ value, H1–H5 and H-all.
  for (const name of ['H1', 'H2', 'H3', 'Hall', 'H4', 'H5']) {
    const metric = m[name];
    if (!metric?.reached) { add(`reached:${name}`, false, 'no reached map'); continue; }
    for (const [key, state] of Object.entries(metric.reached)) {
      const v = reachedValue(metric, key);
      const ok = state === 'yes' ? isNum(v) : (state === 'not-reached' || state === 'not-applicable') && v === null;
      add(`reached:${name}.${key}`, ok, `${state} with ${v === null ? 'null' : typeof v === 'number' ? v : typeof v}`);
    }
  }
  for (const k of MUST_REACH[script] || []) add(`must-reach:${k}`, reachedOf(m, k) === 'yes', `reached ${reachedOf(m, k)}`);

  // Orderings. Both ends must exist, unless the script declares the missing one.
  const err = isNum(doc.clocks?.estimatedErrorMs) ? doc.clocks.estimatedErrorMs : null;
  const na = ORDER_NOT_APPLICABLE[script] || {};
  const order = (id, later, earlier, slack = 0) => {
    if (!isNum(later) || !isNum(earlier)) {
      const declared = na[id] !== undefined && reachedOf(m, na[id]) === 'not-applicable';
      add(id, declared ? null : false, `${declared ? `not applicable to ${script} (${na[id]} not-applicable)` : 'an end is missing'}: ${later ?? 'null'} vs ${earlier ?? 'null'}`);
      return;
    }
    add(id, later >= earlier - slack, `${later} >= ${earlier}${slack ? ` - ${slack}` : ''}`);
  };
  add('clock-error-known', err !== null && doc.clocks?.method === 'clock-line', `method ${doc.clocks?.method}, error ${err} ms`);
  order('order:H1>=first-project-frame', m.H1?.ms, m.H1?.metaPushMs);
  order('order:H2-painted>=H2-receipt', m.H2?.ms, m.H2?.receiptMs);
  order('order:H2-receipt>=daemon-flip-of-NEWEST', m.H2?.receiptMs, newestFlipMs, err ?? 0);
  order('order:H3-24h-receipt>=recent-ready-24h@newest', m.H3?.h24?.receiptMs, m.H3?.h24?.daemonMs, err ?? 0);
  order('order:H3-7d-receipt>=recent-ready-7d@newest', m.H3?.h7d?.receiptMs, m.H3?.h7d?.daemonMs, err ?? 0);
  order('order:Hall-receipt>=daemon-last-root-flip', m.Hall?.receiptMs, lastRootFlipMs, err ?? 0);
  // The daemon's own order: a runtime closes after its last tree flip. 5 ms is half the
  // rounding step of `runtime-readable`'s text stamp (no JSON twin), plus its r1.
  order('order:last-runtime-readable>=daemon-last-root-flip', m.Hall?.daemonMs, lastRootFlipMs, 5.1);
  // Every H5 sample, from the probe's own [receipt, paint] pairs in the document. The
  // probe records a pair only when receipt ≤ paint (probe.js, I1(1)); a paint before its
  // receipt is c1's violation instead, so this guards the derivation, and holds vacuously
  // while H5 is not reached (n 0, as in this regime).
  const pairs = doc.probe?.h5;
  const bad = Array.isArray(pairs) ? pairs.filter((p) => !Array.isArray(p) || !isNum(p[0]) || !isNum(p[1]) || p[1] - p[0] < 0).length : null;
  add('order:H5-samples>=0', Array.isArray(pairs) && bad === 0 && pairs.length === m.H5?.n,
    Array.isArray(pairs) ? `${pairs.length} sample(s), ${bad} negative or malformed; H5 n ${m.H5?.n}` : 'no probe H5 samples in the document');

  // The fixture's shape as both sides saw it.
  const shape = manifest.shape;
  add('shape:giant-identity', r.newestGiantKnown === true && isNum(shape.giantBytes) && r.newestGiantTreeBytes === shape.giantBytes
    && doc.daemon?.newestGiantTreeBytes === shape.giantBytes,
  `fixture giant ${shape.giantBytes} bytes, harness's newest giant ${r.newestGiantTreeBytes}, daemon's newest-giant-ready ${doc.daemon?.newestGiantTreeBytes}`);
  add('shape:newest-is-the-giant', m.D2?.newestGiant?.known === true && isNum(m.H2?.receiptMs) && m.D2.newestGiant.receiptMs === m.H2.receiptMs,
    `giant known ${m.D2?.newestGiant?.known}, giant receipt ${m.D2?.newestGiant?.receiptMs}, NEWEST receipt ${m.H2?.receiptMs}`);
  for (const [h, n] of [['h24', shape.h24], ['h7d', shape.h7d]]) {
    const z = m.H3?.[h] || {};
    add(`shape:${h}-members`, z.roots === n && z.daemonHorizonSessions === n && z.neverRosteredExcluded === 0,
      `spec ${n}, UI ${z.roots}, daemon ${z.daemonHorizonSessions}, excluded ${z.neverRosteredExcluded}`);
  }
  add('shape:roots', doc.equality?.finalRoots === shape.roots && counts.sessions === shape.roots,
    `spec ${shape.roots}, UI ${doc.equality?.finalRoots}, db ${counts.sessions}`);

  // Digests against the expected file.
  const got = digestsOf(doc);
  add('digests-present', doc.equality?.ledgerVerifyOk === true && doc.equality?.rankedWarmExit === 0
    && ['logicalTotal', 'strictNoidRTotal', 'sourceSetDigest', 'rankedIdsSha256'].every((k) => typeof got[k] === 'string'),
    `ledger verify ${doc.equality?.ledgerVerifyOk}, ranked warm exit ${doc.equality?.rankedWarmExit}`);
  if (!expected) add('expected-file', false, 'no expected digests');
  else {
    add('expected:fixture', expected.fixture?.specSha256 === manifest.specSha256 && expected.fixture?.treeSha256 === manifest.tree.sha256,
      'the expected file describes this fixture (spec and generated tree)');
    const want = { ...expected.equality, hitDigest: script === 'reader-v1' ? expected.equality?.hitDigest : null };
    for (const k of PINNED) add(`expected:${k}`, got[k] === want[k], got[k] === want[k] ? String(got[k]).slice(0, 12) : `got ${got[k]}, expected ${want[k]}`);
    const diff = Object.keys({ ...got.counts, ...want.counts }).filter((k) => got.counts?.[k] !== want.counts?.[k]);
    add('expected:counts', same(got.counts, want.counts), diff.length ? diff.map((k) => `${k} ${got.counts?.[k]} != ${want.counts?.[k]}`).join('; ') : 'equal');
  }
  if (search !== undefined) {
    const hits = (search.perCase || []).map((c) => c.hits);
    add('search:fixture-workload', !search.error && search.warmExit === 0 && hits.some((n) => n > 0),
      search.error ?? `hits per case ${hits.join('/')}, warm exit ${search.warmExit}`);
    if (expected) {
      add('expected:fixtureSearch', search.rankedIdsSha256 === expected.search?.rankedIdsSha256 && same(search.perCase, expected.search?.perCase),
        search.rankedIdsSha256 === expected.search?.rankedIdsSha256 ? String(search.rankedIdsSha256).slice(0, 12) : `got ${search.rankedIdsSha256}, expected ${expected.search?.rankedIdsSha256}`);
    }
  }

  const recorded = {
    exitPolicy: 'run_ui exit code is not the verdict (H7 aim misses are a product finding)',
    regime: regimeOf(doc),
    h7Invariant: m.H7?.invariant ?? null,
    i1Exercised: Object.fromEntries(Object.entries(i1).map(([k, c]) => [k, c.exercised])),
    h4: { n: m.H4?.n ?? null, pendingFace: m.H4?.pendingFace?.n ?? null }, h5n: m.H5?.n ?? null,
    flippedBeforeDrawn: m.H2?.flippedBeforeDrawn ?? null,
    hallLagVsRuntimeReadableMs: m.Hall?.lagVsDaemonMs ?? null,
  };
  return { checks, recorded };
}
