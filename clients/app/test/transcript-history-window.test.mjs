import assert from 'node:assert/strict';
import { describe, it } from 'node:test';
import { conversationStart, readConversationPage, withinHistoryWindow } from '../../room/src/lib/transcript/history-window.ts';
import { documentTurns, sittings, teamChronology } from '../../room/src/lib/transcript/index.ts';
import { documentPlan } from '../../room/src/components/transcript/document-plan.ts';
import { activityPlan, activityCuts } from '../../room/src/components/transcript/activity-plan.ts';
import { retainReadingIdentity } from '../../room/src/components/transcript/reading-identity.ts';
import { entry, inline, shell } from './support/ledger-fixtures.mjs';

const now = new Date(2026, 8, 28, 12).getTime();
const row = (body, ordinal, over = {}) => entry(body, { timeline_id: `p${ordinal}`, ordinal, at: now + ordinal * 1000, ...over });
const chat = n => row({ type: 'agent', text: inline(`Reply ${n}`), attachments: [], citations: [] }, n);
const action = n => shell('pwd', { timeline_id: `p${n}`, ordinal: n, at: now + n * 1000 });
const team = (id, at) => ({ ...row({ type: 'collaboration', event: {
  kind: 'message', channel: 'team', sender: 'worker', recipient: 'lead', team: 'team', color: null,
  summary: null, body: inline('Worker update'), task_id: null, delivery_id: null, status: null,
  authored_at: at, source_session_id: null, source_response_id: null,
  work_start_timeline_id: null, work_end_timeline_id: null,
} }, 1, { timeline_id: id, at }), session_id: 'worker' });
function project(parent, shared, hasEarlier) {
  const rows = teamChronology(parent, shared, { hasEarlier, ready: true }).rows;
  const cuts = activityCuts(parent, null, rows);
  const days = sittings(rows, now);
  const turns = documentTurns(rows, { sharedCollaborationSessionIds: new Set(shared.keys()),
    cutBefore: new Set([...cuts.cutBefore, ...days.slice(1).map(day => day.rows[0].timeline_id)]) });
  return activityPlan(documentPlan(turns, days), { ...cuts, partial: hasEarlier });
}
const groups = plan => plan.flatMap(s => s.blocks).filter(b => b.kind === 'activity');

describe('one covered conversation window', () => {
  it('does not expose earlier team-only folds, and keeps complete groups unchanged while paging', () => {
    const parent = [chat(1), action(2), chat(3), action(4), chat(5), action(6), chat(7), action(8)];
    const shared = new Map([['worker', [team('old', now + 2500), team('middle', now + 4500), team('recent', now + 6500)]]]);
    const tail = project(parent.slice(4), shared, true);
    assert.deepEqual(groups(tail).map(g => g.rows.map(r => r.timeline_id)), [['p6', 'recent'], ['p8']]);
    const more = retainReadingIdentity(tail, project(parent.slice(2), shared, true));
    const all = retainReadingIdentity(more, project(parent, shared, false));
    for (const old of groups(tail)) {
      const current = groups(all).find(g => g.key === old.key);
      assert.deepEqual(current.rows.map(r => r.timeline_id), old.rows.map(r => r.timeline_id));
      assert.equal(current.partial, false);
    }
    assert.equal(groups(all).length, 4);
  });

  it('waits for the parent snapshot and excludes search gaps instead of filling them with team-only history', () => {
    const parent = [chat(1), chat(100), action(101)];
    const events = [team('before', now), team('gap', now + 50000), team('covered', now + 100500)];
    const shared = new Map([['worker', events]]);
    assert.deepEqual(teamChronology([], shared, { hasEarlier: true, ready: false }).rows, []);
    assert.deepEqual(teamChronology(parent, shared, { hasEarlier: true, ready: true }).rows.map(r => r.timeline_id),
      ['p1', 'p100', 'covered', 'p101']);
    assert.equal(withinHistoryWindow(null, parent, false), false);
    assert.equal(withinHistoryWindow(null, [chat(1), action(2)], false), true);
    assert.equal(withinHistoryWindow(null, [chat(1), action(2)], true), false);
  });

  it('uses authored time, retains excluded deliveries in their source, and admits them when history reaches them', () => {
    const early = team('old', now + 2000);
    early.at = now + 100000;
    const shared = new Map([['worker', [early]]]);
    assert.equal(teamChronology([chat(100)], shared, { hasEarlier: true, ready: true }).rows.length, 1);
    assert.equal(shared.get('worker').length, 1);
    assert.deepEqual(teamChronology([chat(1), action(2), chat(3)], shared, { hasEarlier: false, ready: true }).rows.map(r => r.timeline_id),
      ['p1', 'p2', 'old', 'p3']);
  });

  it('starts at real speech, including image-only chat; keeps activity-only and complete histories readable', () => {
    const rows = [action(20), action(21), chat(22), action(23)];
    assert.equal(conversationStart(rows, true), 22);
    assert.equal(conversationStart(rows, false), 20);
    assert.equal(conversationStart(rows.slice(0, 2), true), 20);
    const image = row({ type: 'agent', text: null, citations: [], attachments: [{ id: 'image', kind: 'image', name: null,
      media_type: 'image/png', byte_len: null, dimensions: null, source: { type: 'unavailable', reason: 'omitted' } }] }, 21);
    assert.equal(conversationStart([action(20), image, chat(22)], true), 21);
    const assignment = row({ type: 'user', text: inline('Worker assignment'), attachments: [] }, 21,
      { group: { id: 'assignment', kind: 'parent_invocation', derivation: 'native' } });
    assert.equal(conversationStart([action(20), assignment, chat(22)], true), 22);
  });
});

describe('bounded conversation paging', () => {
  it('reads past a dense activity page to useful speech, then returns one ordered held section', async () => {
    const calls = [];
    const pages = {
      tail: { entries: Array.from({ length: 200 }, (_, i) => action(401 + i)), cursor: 'older', watermark: 'g1' },
      older: { entries: Array.from({ length: 200 }, (_, i) => i % 20 === 0 ? chat(201 + i) : action(201 + i)), cursor: 'start', watermark: 'g1' },
    };
    const result = await readConversationPage(async (cursor, limit) => { calls.push([cursor, limit]); return pages[cursor]; }, 'tail');
    assert.deepEqual(calls, [['tail', 200], ['older', 200]]);
    assert.equal(result.entries.length, 400);
    assert.equal(result.entries[0].ordinal, 201);
    assert.equal(result.cursor, 'start');
  });

  it('caps huge activity runs and stops at the true start', async () => {
    let reads = 0;
    const result = await readConversationPage(async cursor => {
      reads += 1;
      const end = Number(cursor);
      return { entries: Array.from({ length: 200 }, (_, i) => action(end - 199 + i)), cursor: String(end - 200), watermark: 'g1' };
    }, '2000');
    assert.equal(reads, 5);
    assert.equal(result.entries.length, 1000);
    assert.equal(result.cursor, '1000');
    const complete = await readConversationPage(async () => ({ entries: [action(1)], cursor: null, watermark: 'g1' }), 'start');
    assert.equal(complete.cursor, null);
  });

  it('does not expose a half-read page on failure or cancellation and refuses a stuck cursor', async () => {
    let calls = 0;
    await assert.rejects(readConversationPage(async () => {
      if (++calls === 2) throw new Error('offline');
      return { entries: [action(5)], cursor: 'older', watermark: 'g1' };
    }, 'tail'), /offline/);
    await assert.rejects(readConversationPage(async () => { throw new DOMException('Changed session', 'AbortError'); }, 'tail'), { name: 'AbortError' });
    await assert.rejects(readConversationPage(async () => ({ entries: [], cursor: 'tail', watermark: 'g1' }), 'tail'), /did not advance/);
  });
});

describe('stable rendered identity', () => {
  it('preserves a partial fold and its sitting on prepend and append, without freezing the day anchor', () => {
    const before = project([action(20), action(21)], new Map(), true);
    const prepended = retainReadingIdentity(before, project([action(19), action(20), action(21)], new Map(), true));
    const appended = retainReadingIdentity(prepended, project([action(19), action(20), action(21), action(22)], new Map(), true));
    assert.equal(appended[0].renderKey, before[0].sitting.key);
    assert.equal(appended[0].sitting.key, 'p19');
    assert.equal(groups(appended)[0].key, groups(before)[0].key);
  });

  it('does not duplicate React keys when a group splits and later merges', () => {
    const before = project([action(1), action(2), action(4), action(5)], new Map(), true);
    const split = retainReadingIdentity(before, project([action(1), action(2), chat(3), action(4), action(5)], new Map(), true));
    const keys = groups(split).map(g => g.key);
    assert.equal(new Set(keys).size, keys.length);
    assert.ok(keys.includes(groups(before)[0].key));
    const merged = retainReadingIdentity(split, before);
    assert.ok(keys.includes(groups(merged)[0].key));
  });
});
