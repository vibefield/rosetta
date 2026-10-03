import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  ACTIVITY_QUIET_MS as quiet, activityDay, advanceActivityOrder,
  compareActivityRanks, compareOrderedSessions,
} from '../../room/src/lib/activity-order.ts';

const now = new Date(2026, 9, 2, 12).getTime();
function session(id, at, project = 'p', parent = null) {
  return {
    id, project_id: project, parent_session_id: parent,
    activity: at === null ? null : { first_at: at, last_at: at },
    updated_at: now, stream_state: 'indexed',
  };
}
const step = (previous, rows, at = now, options = {}) => advanceActivityOrder(previous, rows, {
  now: at, live: true, bootstrapping: false, ...options,
});
const ids = (rows, order) => [...rows].sort((a, b) => compareOrderedSessions(a, b, order)).map(row => row.id);
const projectIds = order => [...order.projects.keys()].sort((a, b) => compareActivityRanks(
  order.projects.get(a).rankAt, order.projects.get(b).rankAt, a ?? '', b ?? '',
));

test('interleaved output stays in place for an hour while recorded times keep advancing', () => {
  let rows = [session('a', now), session('b', now - 1_000), session('c', now - 2_000, 'other')];
  let order = step(null, rows);
  const initialProjects = projectIds(order);
  const initialSessions = ids(rows, order);
  for (let i = 1; i <= 3_600; i++) {
    const at = now + i * 1_000;
    const target = rows[i % 3];
    rows = rows.map(row => row.id === target.id ? session(row.id, at, row.project_id) : row);
    order = step(order, rows, at);
    assert.deepEqual(ids(rows, order), initialSessions);
    assert.deepEqual(projectIds(order), initialProjects);
  }
  assert.equal(order.sessions.get('a').seenAt, now + 3_600_000);
  assert.equal(order.sessions.get('a').rankAt, now);
});

test('quiet expiry does nothing; the first fresh event at the five-minute boundary promotes once', () => {
  let rows = [session('a', now), session('b', now - 1)];
  let order = step(null, rows);
  const initial = order.sessions.get('b').rankAt;
  order = step(order, rows, now + quiet * 2);
  assert.equal(order.sessions.get('b').rankAt, initial);
  rows = [rows[0], session('b', now - 1 + quiet)];
  order = step(order, rows, now - 1 + quiet);
  assert.deepEqual(ids(rows, order), ['b', 'a']);
  const promoted = order.sessions.get('b').rankAt;
  rows = [rows[0], session('b', now + quiet + 1_000)];
  order = step(order, rows, now + quiet + 1_000);
  assert.equal(order.sessions.get('b').rankAt, promoted);
});

test('an event just before the quiet boundary renews the interval without promotion', () => {
  let order = step(null, [session('a', now)]);
  order = step(order, [session('a', now + quiet - 1)], now + quiet - 1);
  order = step(order, [session('a', now + quiet + 1)], now + quiet + 1);
  assert.equal(order.sessions.get('a').rankAt, now);
});

test('new and resumed sessions do not move a project with another progressing session', () => {
  let rows = [session('a', now, 'p'), session('b', now - quiet * 2, 'p'), session('c', now + 1, 'q')];
  let order = step(null, rows, now + 1);
  rows = [session('a', now + 10_000), session('b', now + 11_000), rows[2], session('new', now + 12_000)];
  order = step(order, rows, now + 12_000);
  assert.equal(order.sessions.get('b').rankAt, now + 11_000);
  assert.equal(order.projects.get('p').rankAt, now);
  assert.deepEqual(projectIds(order), ['q', 'p']);
  rows = rows.map(row => row.id === 'b' ? session('b', now + quiet + 12_000) : row);
  order = step(order, rows, now + quiet + 12_000);
  assert.deepEqual(projectIds(order), ['p', 'q']);
});

test('coalesced project progress is folded chronologically, independent of row order', () => {
  const oldRows = [session('a', now), session('b', now - 1)];
  const initial = step(null, oldRows);
  const rows = [session('a', now + quiet + 1), session('b', now + quiet - 1)];
  for (const input of [rows, [...rows].reverse()]) {
    const order = step(initial, input, now + quiet + 1);
    assert.equal(order.projects.get('p').rankAt, now);
    assert.equal(order.projects.get('p').seenAt, now + quiet + 1);
  }
});

test('replacement snapshots and catch-up baseline existing rows without promotions', () => {
  let rows = [session('a', now), session('b', now - 1, 'q')];
  let order = step(null, rows);
  rows = [rows[0], session('b', now + quiet * 2, 'q')];
  order = step(order, rows, now + quiet * 2, { live: false });
  assert.deepEqual(ids(rows, order), ['a', 'b']);
  assert.deepEqual(projectIds(order), ['p', 'q']);
  rows = [rows[0], session('b', now + quiet * 2 + 1, 'q')];
  order = step(order, rows, now + quiet * 2 + 1);
  assert.deepEqual(ids(rows, order), ['a', 'b']);
});

test('late imports, metadata touches, invalid timestamps and future events cannot promote an existing row', () => {
  const initialRows = [session('a', now), session('b', now - 100 * quiet, 'q')];
  const initial = step(null, initialRows);
  for (const at of [now - quiet, now - quiet - 1, now + 1, NaN, Infinity, null]) {
    const order = step(initial, [initialRows[0], session('b', at, 'q')]);
    assert.equal(order.sessions.get('b').rankAt, initial.sessions.get('b').rankAt);
    assert.equal(order.projects.get('q').rankAt, initial.projects.get('q').rankAt);
  }
  const touched = step(initial, initialRows.map(row => ({ ...row, updated_at: now + quiet, stream_state: 'streaming' })), now + quiet);
  assert.deepEqual(ids(initialRows, touched), ['a', 'b']);
});

test('retraction and replay do not manufacture a new quiet interval', () => {
  let order = step(null, [session('a', now), session('b', now - 1, 'q')]);
  order = step(order, [session('a', now), session('b', now - quiet * 10, 'q')]);
  order = step(order, [session('a', now), session('b', now + 1, 'q')], now + 1);
  assert.equal(order.sessions.get('b').rankAt, now - 1);
  assert.equal(order.projects.get('q').rankAt, now - 1);
});

test('reparenting and reassignment change membership without inventing activity', () => {
  const initial = step(null, [session('a', now - quiet * 2), session('worker', now - quiet * 2, 'q', 'a')]);
  const order = step(initial, [session('a', now - quiet * 2), session('worker', now, 'p')]);
  assert.equal(order.sessions.get('worker').rankAt, now - quiet * 2);
  assert.equal(order.projects.get('p').rankAt, now - quiet * 2);
  assert.equal(order.projects.has('q'), false);
});

test('cold discovery seeds real chronological ranks and prunes deleted identities', () => {
  let order = step(null, [session('a', null), session('b', null)], now, { bootstrapping: true });
  order = step(order, [session('a', now - 10_000), session('b', now - 1_000)], now, { bootstrapping: true });
  assert.deepEqual(ids([session('a', now), session('b', now)], order), ['b', 'a']);
  order = step(order, [session('a', now - 500), session('b', now - 1_000)], now);
  assert.deepEqual(ids([session('a', now), session('b', now)], order), ['a', 'b']);
  order = step(order, [session('b', now)]);
  assert.equal(order.sessions.has('a'), false);
  assert.equal(order.projects.size, 1);
});

test('unknowns sort last, ties are deterministic, and unassigned sessions share one project', () => {
  const rows = [session('unknown', null, null), session('b', now, null), session('a', now, null)];
  const order = step(null, rows);
  assert.deepEqual(ids(rows, order), ['a', 'b', 'unknown']);
  assert.equal(order.projects.size, 1);
  assert.equal(order.projects.get(null).rankAt, now);
});

test('calendar boundaries keep day groups contiguous without re-sorting ongoing peers', () => {
  const late = new Date(2026, 9, 2, 23, 59).getTime();
  const next = late + 2 * 60_000;
  const initial = step(null, [session('a', late), session('b', late - 1)], late);
  const rows = [session('a', late), session('b', next)];
  const order = step(initial, rows, next);
  assert.equal(order.sessions.get('b').rankAt, late - 1);
  assert.deepEqual(ids(rows, order), ['b', 'a']);
  const together = [session('a', next + 1), session('b', next)];
  assert.deepEqual(ids(together, step(order, together, next + 1)), ['a', 'b']);
  assert.notEqual(activityDay(late), activityDay(next));
});
