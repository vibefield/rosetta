// probe-worker.js, loaded into a bare vm context as a classic worker script: the
// per-id flip tracker (no phantom flips across a resync and its fresh snapshot), the
// NEWEST/RECENT freeze (metadata-only placeholders excluded), the horizons, and the
// end-state counts I1(5) reads.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';

const SRC = readFileSync(new URL('../probe-worker.js', import.meta.url), 'utf8');
const HOUR = 3_600_000;
const T = 1_790_000_000_000;

function load() {
  const out = [];
  const ctx = { postMessage: (m) => out.push(JSON.parse(JSON.stringify(m))), performance, JSON, Map, Set, Math, Number, Array, Object };
  vm.createContext(ctx);
  vm.runInContext(SRC, ctx);
  const send = (m) => ctx.onmessage({ data: m });
  return { out, send, last: (k) => out.filter((m) => m.k === k).pop() };
}
const row = (id, over = {}) => ({ id, native_session_id: `n-${id}`, runtime_id: 'rt', project_id: 'P1', cwd: '/w', parent_session_id: null, delegation: null,
  root_session_id: id, role: 'main', title: 't', summary: null, started_at: 0, updated_at: T, activity: null, stream_state: 'pending', ...over });
const meta = (t, frame) => ({ k: 'meta', t, d: JSON.stringify(frame) });
const mut = (t, r) => meta(t, { frame: 'mutation', stamp: null, mutation: { type: 'session', ...r } });
const status = (t, over = {}) => ({ k: 'status', t, d: JSON.stringify({ canonical_ready: false, search: { readiness: 'building' },
  daemon: { runtimes: [{ kind: 'codex' }, { kind: 'claude-code' }], ingest: { phase: 'backfilling', metadata_ready_at: 123, runtimes: [{ kind: 'claude-code' }],
    projects: [{ sessions_done: 1, sessions_total: 3 }, { sessions_done: 2, sessions_total: 2 }], run: { state: 'running' } } }, ...over }) });

const ROWS = [
  row('A', { updated_at: T }),
  row('B', { updated_at: T - HOUR }),
  row('C', { project_id: 'P2', updated_at: T - 2 * 24 * HOUR }),
  row('E', { project_id: null, updated_at: T - 30 * 24 * HOUR }),
  // A metadata-only placeholder: Pending, nothing of its own, with a child. Newer than A.
  row('M', { updated_at: T + HOUR, cwd: null, title: null }),
  row('D', { parent_session_id: 'M', root_session_id: 'M', role: 'subagent', updated_at: T - 5 }),
];

test('freeze excludes metadata-only rows; flips are first-time only, across a resync', () => {
  const w = load();
  w.send(meta(1, { frame: 'hello', protocol_version: 29, stamp: null }));
  w.send(meta(2, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: [] }));
  for (const [i, r] of ROWS.entries()) w.send(mut(10 + i, r));
  w.send(status(20, { daemon: { ingest: { phase: 'awaiting_consent', metadata_ready_at: null, projects: [] }, runtimes: [] } }));
  assert.equal(w.last('frozen'), undefined);
  w.send(status(30));
  const f = w.last('frozen');
  assert.equal(f.newest, 'A');
  assert.equal(f.newestProject, 'P1');
  assert.deepEqual([...f.r24].sort(), ['A', 'B']);
  assert.deepEqual([...f.r7].sort(), ['A', 'B', 'C']);
  assert.equal(f.roots, 4);
  assert.equal(f.metaOnly, 1);
  // Every root the list draws in that project, the placeholder (data-standing=unavailable) included.
  assert.deepEqual(f.projectRoots, { A: T, B: T - HOUR, M: T + HOUR });
  const s = w.last('status');
  assert.equal(s.done, 3);
  assert.equal(s.total, 5);
  assert.deepEqual(s.kinds, ['claude-code', 'codex']);

  w.send(mut(40, row('A', { stream_state: 'streaming' })));
  let m = w.last('meta');
  assert.deepEqual(m.flips, ['A']);
  assert.deepEqual(m.projFlips, ['P1']);
  assert.deepEqual([m.readyRoots, m.totalRoots], [1, 4]);
  // The hub's queue overflowed: a resync barrier, then a fresh full snapshot.
  w.send(meta(50, { frame: 'resync' }));
  w.send(meta(51, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: ROWS.map((r) => (r.id === 'A' ? { ...r, stream_state: 'indexed' } : r)) }));
  m = w.last('meta');
  assert.equal(m.kind, 'snapshot');
  assert.deepEqual(m.flips, [], 'no phantom flip for A');
  assert.equal(m.retract, 0);
  assert.equal(w.out.filter((x) => x.k === 'horizon').length, 0);
  w.send(mut(60, row('B', { updated_at: T - HOUR, stream_state: 'indexed' })));
  const h24 = w.out.find((x) => x.k === 'horizon' && x.name === 'h24');
  assert.equal(h24.at, 60);
  assert.equal(h24.n, 2);
  w.send(mut(70, row('C', { project_id: 'P2', updated_at: T - 2 * 24 * HOUR, stream_state: 'indexed' })));
  assert.equal(w.out.find((x) => x.k === 'horizon' && x.name === 'h7d').at, 70);
});

test('a retraction is counted; end state compares the UI rows with the daemon roots', () => {
  const w = load();
  w.send(meta(2, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: ROWS }));
  w.send(status(3));
  for (const id of ['A', 'B', 'C', 'E', 'D']) w.send(mut(10, { ...ROWS.find((r) => r.id === id), stream_state: 'indexed' }));
  w.send(mut(20, row('A', { stream_state: 'pending' })));
  assert.equal(w.last('meta').retract, 1);
  w.send(mut(21, row('A', { stream_state: 'indexed' })));
  assert.deepEqual(w.last('meta').flips, [], 'A flipped once already');
  w.send({ k: 'end', id: 7, t: 30, daemon: { roots: [['A', 'P1', 'indexed'], ['B', 'P1', 'indexed'], ['C', 'P2', 'indexed'], ['E', '__rosetta_unassigned__', 'indexed'],
    ['M', 'P1', 'pending']], truncated: false } });
  const e = w.last('end');
  assert.equal(e.id, 7);
  assert.equal(e.rootsSeen, 5);
  assert.equal(e.metaOnlyRoots, 1);
  assert.equal(e.pendingRoots, 0);
  assert.equal(e.neverRostered, 0);
  assert.equal(e.readyAll, 5);
  assert.equal(e.daemonPending, 1);
  assert.equal(e.projectMismatches, 0);
  assert.equal(e.newestProjectRoots, 3);
  assert.equal(e.membershipStable, true);
  assert.equal(e.counts.retractions, 1);
});

test('the private giant id resolves by native id, and a bad frame is survived', () => {
  const w = load();
  w.send({ k: 'cfg', giantNative: 'n-C' });
  w.send(meta(2, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: ROWS }));
  assert.equal(w.last('meta').giantId, 'C');
  w.send({ k: 'meta', t: 3, d: '{not json' });
  assert.equal(w.last('error').k, 'error');
  w.send({ k: 'flush', id: 9 });
  assert.equal(w.last('flushed').id, 9);
});

test('probe.js and its worker are classic scripts (no module syntax)', () => {
  for (const name of ['../probe.js', '../probe-worker.js']) {
    const src = readFileSync(new URL(name, import.meta.url), 'utf8');
    assert.doesNotThrow(() => new vm.Script(src), name);
    assert.doesNotMatch(src, /^\s*(import|export)\s/m, name);
  }
});

test('a never-rostered member: the horizon resolves when capture is over, over the rest', () => {
  const w = load();
  // B is in the 24 h set but is never rostered: it stays Pending to the end.
  w.send(meta(2, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: ROWS }));
  w.send(status(3));
  w.send(mut(10, row('A', { stream_state: 'indexed' })));
  assert.equal(w.out.filter((x) => x.k === 'horizon').length, 0, 'B still Pending: not reached yet');
  for (const id of ['C', 'E', 'D']) w.send(mut(11, { ...ROWS.find((r) => r.id === id), stream_state: 'indexed' }));
  // Every runtime readable: the status leaves backfilling, so what is still Pending
  // never will be.
  w.send(status(20, { daemon: { runtimes: [{ kind: 'codex' }, { kind: 'claude-code' }], ingest: { phase: 'finalizing', metadata_ready_at: 123, runtimes: [], projects: [], run: { state: 'running' } } } }));
  const h24 = w.out.find((x) => x.k === 'horizon' && x.name === 'h24');
  assert.deepEqual({ at: h24.at, n: h24.n, excluded: h24.excluded, via: h24.viaCaptureOver }, { at: 10, n: 1, excluded: 1, via: true });
  const h7 = w.out.find((x) => x.k === 'horizon' && x.name === 'h7d');
  assert.deepEqual({ at: h7.at, n: h7.n, excluded: h7.excluded }, { at: 11, n: 2, excluded: 1 });
  w.send({ k: 'end', id: 1, t: 30, daemon: { roots: [], truncated: false } });
  const e = w.last('end');
  assert.equal(e.neverRostered, 1);
  assert.equal(e.pendingSessions, 2, 'B and the metadata-only placeholder M');
  assert.deepEqual({ h24At: e.eff.h24At, n24: e.eff.n24, excluded24: e.eff.excluded24, newest: e.eff.newest, newestExcluded: e.eff.newestExcluded, excludedProject: e.eff.excludedProject },
    { h24At: 10, n24: 1, excluded24: 1, newest: 'A', newestExcluded: false, excludedProject: 1 });
  assert.deepEqual(e.eff.projectRoots, ['A'], 'neither B (never rostered) nor M (metadata-only)');
});

test('NEWEST never rostered: the end follows the newest root that was', () => {
  const w = load();
  w.send(meta(2, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: ROWS }));
  w.send(status(3));
  assert.equal(w.last('frozen').newest, 'A');
  for (const id of ['B', 'C', 'E', 'D']) w.send(mut(10, { ...ROWS.find((r) => r.id === id), stream_state: 'indexed' }));
  w.send({ k: 'end', id: 1, t: 30, daemon: { roots: [], truncated: false } });
  const e = w.last('end');
  assert.equal(e.eff.newestExcluded, true);
  assert.equal(e.eff.newest, 'B');
  assert.equal(e.eff.newestReceiptAt, 10);
});

test('an empty frozen set has no receipt time', () => {
  const w = load();
  const old = [row('Z', { updated_at: T }), row('Y', { updated_at: T - 30 * 24 * HOUR })];
  w.send(meta(2, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: old }));
  w.send(status(3));
  const f = w.last('frozen');
  assert.deepEqual(f.r24, ['Z']);
  w.send({ k: 'end', id: 1, t: 4, daemon: { roots: [], truncated: false } });
  assert.equal(w.last('end').eff.h24At, null, 'Z never flipped: no receipt');
  const v = load();
  v.send(meta(2, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: [] }));
  v.send(status(3));
  const h = v.out.find((x) => x.k === 'horizon' && x.name === 'h24');
  assert.equal(h.at, null);
  assert.equal(h.n, 0);
});

// D2(b) corpus-wide. P1: A (NEWEST) and B recent, O three days old; P2: C recent, D
// newer than C, E five days old. O, D and E are readable from t=2; B flips at 10, C at
// 15, A at 20; the run ends at 30.
//   foot (every not-imported row at the foot): A, B, C below an older readable root
//     from 2, then A and C from 10, then A from 15: 3·8 + 2·5 + 1·5 = 39.
//   drawn (D1): A and B are newer than every readable root of P1, so they head its
//     list ("Still importing"); C is older than D, so it sits at P2's foot below E:
//     1·(15 − 2) = 13.
// The integral runs from the first frame: the freeze's moment (the page's first
// post-consent status) moves nothing.
test('corpus inversion: the D1 placement and the foot, over the whole import', () => {
  const rows = [
    row('A', { updated_at: T }), row('B', { updated_at: T - HOUR }), row('O', { updated_at: T - 3 * 24 * HOUR }),
    row('C', { project_id: 'P2', updated_at: T - 2 * HOUR }), row('D', { project_id: 'P2', updated_at: T - HOUR / 2 }),
    row('E', { project_id: 'P2', updated_at: T - 5 * 24 * HOUR }),
  ];
  const ready = (id) => ({ ...rows.find((r) => r.id === id), stream_state: 'indexed' });
  const run = (freezeAt) => {
    const w = load();
    w.send(meta(1, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: rows }));
    const at = [[2, 'O'], [2, 'D'], [2, 'E'], [10, 'B'], [15, 'C'], [20, 'A']];
    let frozenYet = false;
    for (const [t, id] of at) {
      if (!frozenYet && t > freezeAt) { w.send(status(freezeAt)); frozenYet = true; }
      w.send(mut(t, ready(id)));
    }
    if (!frozenYet) w.send(status(freezeAt));
    w.send({ k: 'end', id: 1, t: 30, daemon: { roots: [], truncated: false } });
    return w.last('end');
  };
  for (const freezeAt of [1.5, 5, 12, 25]) {
    const end = run(freezeAt);
    assert.equal(end.inversionFootMs, 39, `foot, frozen at ${freezeAt}`);
    assert.equal(end.inversionMs, 13, `drawn, frozen at ${freezeAt}`);
  }
  // Never frozen (no post-consent status): no membership, no inversion.
  const w = load();
  w.send(meta(1, { frame: 'meta_snapshot', runtimes: [], projects: [], sessions: rows }));
  w.send(mut(2, ready('O')));
  w.send({ k: 'end', id: 1, t: 30, daemon: { roots: [], truncated: false } });
  assert.equal(w.last('end').inversionMs, 0);
  assert.equal(w.last('end').inversionFootMs, 0);
});

test('the worker keeps no title, cwd or native id', () => {
  assert.doesNotMatch(SRC, /title:\s*row\.title|cwd:\s*row\.cwd|nat:\s*row\.native_session_id/);
});
