// The ui-run/1 privacy gate: a run document built exactly as run_ui.mjs builds it
// passes; any undeclared key, and any id, title, path or query text in a string,
// is refused.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseDaemonLog } from '../daemon-lines.mjs';
import { derive } from '../metrics.mjs';
import { loadSchema, validateRun } from '../privacy.mjs';
import { DEVIATIONS, HARNESS_FILES, R1_FILES, buildRunDoc } from '../run_ui.mjs';
import { HEX, daemonStdout, mainNotes, probeLog } from './fixtures.mjs';
import { createHash } from 'node:crypto';

const SESSION_ID = '3f9a6c1e8b7d4a2f9e0c1b2a3d4e5f60718293a4b5c6d7e8f9a0b1c2d3e4f5a6';
const TITLE = 'Fix the login redirect loop';
const PATH = '/Users/someone/work/secret-project';
const QUERY = 'foreign_key_check';

function validRun() { return buildRunDoc(validRunParts()); }
function validRunParts() {
  const probe = probeLog();
  const main = mainNotes();
  const daemonLog = parseDaemonLog(daemonStdout());
  const derived = derive({ probe, main, daemon: daemonLog, mainPings: { start: { offsetMs: 0, rttMs: 0.1 }, end: null } });
  const harness = {};
  for (const f of HARNESS_FILES) harness[f] = HEX('1');
  for (const f of R1_FILES) harness[`r1-bench/${f}`] = HEX('2');
  return {
    script: { name: 'reader-v1', sha256: HEX('3') },
    identity: { rosettadSha256: HEX('4'), distSha256: HEX('5'), distFiles: 3, electron: '43.7.9',
      snapshot: { walkInputsDigest: HEX('6'), manifestSha256: HEX('7'), files: 639, bytes: 1778445372 }, harnessSha256: harness,
      machine: { model: 'Mac16,10', os: '26.7.2', cpus: 10, memGb: 16 }, windowPx: '1500x940', dpr: 2, dprMeasured: 2, deviations: DEVIATIONS, enrolled: ['claude-code', 'codex'] },
    run: { startedAt: '2026-11-03T20:14:55.151Z', outcome: 'complete', outcomeReasons: [], wallS: 60.2, daemonExit: { code: 0, signal: null }, electronExit: 0,
      guard: { cpuIdleAtStart: 85, waitedS: 4, proceededOverGuard: false, load1: 2.9, idleMin: 75, swapMbBefore: 3000, swapMbAfter: 3000, freeGbBefore: 44.2 },
      snapshotVerify: { ok: true, files: 639, bytes: 1778445372, wallMs: 3000 }, scriptOk: true, scriptTimeouts: 0, newestGiantKnown: true, newestGiantTreeBytes: 900000000 },
    probe, mainNotes: main, derived, daemonLog, stderr: ['a line'],
    digests: { logicalTotal: HEX('8'), strictTotal: HEX('9'), strictNoidTotal: HEX('a'), strictNoidRTotal: HEX('b'), surrogateTotal: null,
      logicalOutSha256: HEX('c'), cursorOrderOutSha256: HEX('d'), counts: { sessions: 303, projects: 15, 'session_kind:codex': 5 }, sourceSetDigest: HEX('e'),
      sourceDigest: HEX('f'), metaDigest: HEX('0'), entryDigest: HEX('1'), ledgerVerifyOk: true, rankedIdsSha256: HEX('2'), rankedSha256: HEX('3'), rankedWarmExit: 0 },
    forbiddenCount: 4,
  };
}
const forbidden = [SESSION_ID, TITLE, PATH, QUERY, 'n-native-1'];
const errorsOf = (doc, opts) => validateRun(doc, { forbidden, ...opts }).errors;

test('a run built the way run_ui.mjs builds it passes', () => {
  const v = validateRun(validRun(), { forbidden });
  assert.deepEqual(v.errors, []);
  assert.equal(v.ok, true);
  assert.ok(v.freeStrings > 20);
});

test('the run document carries the absolute invariants as one flag', () => {
  assert.equal(validRun().run.invariantsPass, true);
  const probe = probeLog({ h7: { aims: [[1, 'newest-project', 0, -103, 'other-row']] } });
  const derived = derive({ probe, main: mainNotes(), daemon: parseDaemonLog(daemonStdout()), mainPings: { start: { offsetMs: 0, rttMs: 0.1 }, end: null } });
  const doc = buildRunDoc({ ...validRunParts(), probe, derived });
  assert.equal(doc.metrics.I1.pass, true);
  assert.equal(doc.metrics.H7.invariant.pass, false);
  assert.equal(doc.run.invariantsPass, false, 'an aim miss is an invariant failure even when I1 passes');
});

test('every deviation and harness file the driver records is whitelisted', () => {
  const schema = loadSchema();
  const ids = schema.root.obj.identity.obj;
  assert.deepEqual(ids.deviations.arr.enum.slice().sort(), DEVIATIONS.slice().sort());
  for (const f of [...HARNESS_FILES, ...R1_FILES.map((x) => `r1-bench/${x}`)]) assert.ok(ids.harnessSha256.keys.includes(f), f);
});

test('an undeclared key is refused, at the top and deep inside', () => {
  const a = validRun(); a.sessionId = 'x';
  assert.match(errorsOf(a).join('\n'), /^\$: key \(9 chars, sha256 [0-9a-f]{8}\) is not in the schema$/m);
  const b = validRun(); b.probe.h2.title = 'x';
  assert.match(errorsOf(b).join('\n'), /\$\.probe\.h2: key \(5 chars/);
  const c = validRun(); c.identity.harnessSha256['../../secret.txt'] = HEX('1');
  assert.match(errorsOf(c).join('\n'), /not whitelisted/);
});

test('an error never repeats the refused key: it may be the private value itself', () => {
  const r = validRun();
  r.probe[PATH] = 1;
  r.run.stubbed[SESSION_ID] = 1;
  const text = errorsOf(r).join('\n');
  assert.ok(text.length > 0);
  assert.ok(!text.includes(PATH) && !text.includes('secret') && !text.includes(SESSION_ID.slice(0, 12)), text);
});

test('Object.prototype names are not declared keys, at the top or nested', () => {
  const at = [
    (r) => { r.constructor = TITLE; },
    (r) => { r.toString = PATH; },
    (r) => { r.identity.hasOwnProperty = { cwd: PATH, title: TITLE }; },
    (r) => { r.metrics.H1.valueOf = [SESSION_ID, TITLE]; },
    (r) => { r.probe.opens[0].isPrototypeOf = TITLE; },
    (r) => { r.metrics.D3.opens[0].propertyIsEnumerable = SESSION_ID; },
    (r) => { r.metrics.I1.checks.toLocaleString = 1; },
    (r) => { r.probe.h7.projects.constructor = 1; },
  ];
  for (const [i, mutate] of at.entries()) {
    const r = validRun();
    mutate(r);
    assert.equal(validateRun(r, { forbidden }).ok, false, `case ${i}`);
  }
  // An own __proto__ key, as JSON.parse creates it, inside the probe export.
  const r = validRun();
  r.probe = { ...JSON.parse('{"__proto__":{"cwd":"/x"}}'), ...r.probe };
  assert.ok(Object.prototype.hasOwnProperty.call(r.probe, '__proto__'));
  assert.equal(validateRun(r, { forbidden }).ok, false, 'own __proto__');
});

test('the forbidden scan scales to every id of a corpus and still catches one', () => {
  const ids = Array.from({ length: 50_000 }, (_, i) => createHash('sha256').update(`t${i}`).digest('hex'));
  const r = validRun();
  assert.equal(validateRun(r, { forbidden: ids }).ok, true);
  r.equality.hitDigest = ids[31_337];
  assert.equal(validateRun(r, { forbidden: ids }).ok, false);
});

test('ids, titles, paths and query text are refused wherever a string goes', () => {
  const cases = [
    (r) => { r.run.marks.push([1, SESSION_ID]); },
    (r) => { r.probe.faces.push([1, TITLE]); },
    (r) => { r.metrics.D3.opens[0].coverLabel = QUERY; },
    (r) => { r.identity.machine.os = PATH; },
    (r) => { r.identity.windowPx = TITLE; },
    (r) => { r.run.startedAt = PATH; },
    (r) => { r.probe.h8.search[0].digest = QUERY; },
    (r) => { r.equality.logicalTotal = 'not-a-digest'; },
    (r) => { r.metrics.H1.ms = '2050'; },
    (r) => { r.probe.status.kinds = ['claude-code', PATH]; },
  ];
  for (const [i, mutate] of cases.entries()) {
    const r = validRun();
    mutate(r);
    assert.notDeepEqual(errorsOf(r), [], `case ${i}`);
  }
});

test('a 64-hex id cannot pass as a digest: the forbidden scan catches it', () => {
  const r = validRun();
  r.equality.hitDigest = SESSION_ID;
  assert.equal(validateRun(r, { forbidden: [] }).ok, true, 'the schema alone admits any sha256-shaped string');
  assert.match(errorsOf(r).join('\n'), /forbidden private value/);
});

test('an enum that happens to equal a private value is not a leak', () => {
  // The cover label enum "error" is one of the schema's own strings.
  const r = validRun();
  r.metrics.D3.opens[0].coverLabel = 'error';
  assert.equal(validateRun(r, { forbidden: ['error'] }).ok, true);
});
