// Real timeline hook and document over an in-memory canonical service.
import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { TranscriptColumn } from '../../room/src/components/Transcript';
import { useTimeline } from '../../room/src/lib/hooks';
import { RoomProvider, useRoom } from '../../room/src/room-context';
import { client, ports } from './support/room-ports.mjs';
import { entry, inline, shell } from './support/ledger-fixtures.mjs';
import '../renderer/src/styles/tokens.css';
import '../renderer/src/styles/app.css';
import '../../room/src/styles/room.css';
import '../../room/src/styles/transcript.css';
const now = Date.now() - 1_300_000;
const nothing = () => {};
const parent = Array.from({ length: 1200 }, (_, i) => {
  const ordinal = i + 1;
  const over = { timeline_id: `p${ordinal}`, ordinal, at: now + ordinal * 1000 };
  return ordinal <= 400 && ordinal % 20 === 1
    ? entry({ type: 'agent', text: inline(`Main reply ${ordinal}`), attachments: [], citations: [] }, over)
    : shell('pwd', over);
});
const attachments = Array.from({ length: 8 }, (_, i) => ({ id: `image-${i}`, kind: 'image', name: `Image ${i + 1}`,
  media_type: 'image/png', byte_len: null, dimensions: { width: 400, height: 240 },
  source: { type: 'unavailable', reason: 'omitted' } }));
const peer = (id, ordinal, images = []) => ({ ...entry({ type: 'collaboration', event: {
  kind: 'message', channel: 'team', sender: 'worker', recipient: 'lead', team: 'fixture-team', color: null,
  summary: null, body: inline(`Team report ${id}`), attachments: images, task_id: null, delivery_id: null, status: null,
  authored_at: now + ordinal * 1000 + 500, source_session_id: null, source_response_id: null,
  work_start_timeline_id: null, work_end_timeline_id: null,
}}, { timeline_id: id, ordinal, at: now + ordinal * 1000 + 500 }), session_id: 'worker' });
const team = [peer('old-team', 100), peer('recent-team', 1190, attachments)];
team[0].session_id = 'earlier-worker';
const session = { id: 's1', native_session_id: 'history-fixture', runtime_id: 'claude', project_id: 'p1',
  cwd: '/demo', parent_session_id: null, delegation: null, root_session_id: 's1', role: 'main',
  title: 'History window', summary: null, started_at: now, updated_at: now + 1_200_000,
  stream_state: 'indexed', activity: { first_at: now, last_at: now + 1_200_000 } };
const worker = { ...session, id: 'worker', parent_session_id: 's1', role: 'team', title: 'worker' };
const earlierWorker = { ...worker, id: 'earlier-worker', title: 'earlier-worker' };
const oldStandalone = { ...worker, id: 'old-standalone', role: 'subagent', title: 'Older standalone worker',
  activity: { first_at: now + 100000, last_at: now + 120000 } };
const teamHandlers = new Map();
let handlers, visible = [], closed = false, failReads = 0, generation = 0;
const requests = [];
function snapshot() {
  generation += 1;
  visible = parent.slice(-200);
  handlers.onReset();
  handlers.onSnapshot({ entries: visible, cursor: '1000', watermark: String(generation) });
  handlers.onChange(visible);
}
client.subscribeSession = (_id, h) => {
  handlers = h; closed = false;
  queueMicrotask(() => { h.onStatus('live'); snapshot(); });
  return {
    entries: () => visible,
    async fetchPage(cursor, limit) {
      const current = generation;
      requests.push({ cursor, limit });
      await new Promise(resolve => setTimeout(resolve, 25));
      if (closed || generation !== current) throw new DOMException('Session changed', 'AbortError');
      if (failReads > 0) { failReads -= 1; throw Error('Offline'); }
      const end = Number(cursor), start = Math.max(0, end - limit);
      return { entries: parent.slice(start, end), cursor: start === 0 ? null : String(start), watermark: String(generation) };
    },
    reveal(rows) { visible = [...new Map([...rows, ...visible].map(row => [row.timeline_id, row])).values()].sort((a, b) => a.ordinal - b.ordinal); h.onChange(visible); },
    async loadEntry(id) { const row = parent.find(row => row.timeline_id === id); if (!row) return false; this.reveal([row]); return true; },
    close() { closed = true; },
  };
};
client.subscribeCollaboration = (id, h) => {
  teamHandlers.set(id, h);
  return { close: nothing, restart: () => { h.onReset(); h.onChange({ entries: team.filter(row => row.session_id === id), response_runtimes: [] }); } };
};
window.historyFixture = {
  requests,
  releaseTeam: () => { for (const [id, h] of teamHandlers) h.onChange({ entries: team.filter(row => row.session_id === id), response_runtimes: [] }); },
  resetFailure: () => { failReads = 2; requests.length = 0; snapshot(); },
};
function Fixture() {
  const feed = useTimeline(useRoom().sessions, 's1');
  const [focus, setFocus] = useState(null);
  Object.assign(window.historyFixture, { feed, seek: id => setFocus({ sessionId: id === 'old-team' ? 'earlier-worker' : 'worker', timelineId: id }) });
  return <div className="ah-frame" style={{ display: 'flex', flexDirection: 'column' }}>
    <div className="ah-room" style={{ position: 'relative', inset: 'auto', flex: 1, minHeight: 0 }}>
      <div className="ah-cols" style={{ display: 'grid', gridTemplateColumns: 'minmax(0, 1fr)' }}>
        <TranscriptColumn session={session} sessions={[session, worker, earlierWorker, oldStandalone]} feed={feed} duringBackfill={false}
          project={null} focus={focus} onFocusConsumed={() => setFocus(null)} onReceipt={nothing}
          stepper={null} onStepMatch={nothing} onDismissStepper={nothing}
          getRuntimeKind={() => 'claude-code'} homeDirectory={null} />
      </div>
    </div>
  </div>;
}
createRoot(document.getElementById('root')).render(<RoomProvider ports={ports}><Fixture /></RoomProvider>);
