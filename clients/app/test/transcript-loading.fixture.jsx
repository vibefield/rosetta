// Production hook, column, folds and scroll controller; all data is synthetic.
import { useState } from 'react';
import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { TranscriptColumn } from '../renderer/src/components/Transcript';
import { useTimeline } from '../renderer/src/lib/hooks';
import { RoomProvider, useRoom } from '../renderer/src/room-context';
import { client, ports } from './support/room-ports.mjs';
import { entry, inline, shell } from './support/ledger-fixtures.mjs';
import '../renderer/src/styles/tokens.css';
import '../renderer/src/styles/app.css';
import '../renderer/src/styles/room.css';
import '../renderer/src/styles/transcript.css';

const now = Date.now() - 1_300_000;
const nothing = () => {};
const sessions = ['alpha', 'beta', 'empty', 'team', 'failure'].map((id, i) => ({
  id, native_session_id: id, runtime_id: 'claude', project_id: 'p1', cwd: '/demo',
  parent_session_id: null, delegation: null, root_session_id: id, role: 'main',
  title: ['Refine the reading experience', 'Review the image pipeline', 'An empty conversation', 'Coordinate the team', 'Connection recovery'][i],
  summary: null, started_at: now, updated_at: now + 24000,
  stream_state: 'indexed', activity: { first_at: now, last_at: now + 24000 },
}));
const worker = { ...sessions[3], id: 'worker', title: 'Reviewer', parent_session_id: 'team', role: 'team' };
function rowsFor(id) {
  if (id === 'empty') return [];
  return Array.from({ length: 24 }, (_, i) => {
    const over = { timeline_id: `${id}-${i}`, ordinal: i, at: now + i * 1000,
      group: { id: `turn-${Math.floor(i / 3)}`, kind: i % 3 === 0 ? 'human_turn' : 'agent_cycle', derivation: 'native' } };
    const row = i % 3 === 1 ? shell('pnpm typecheck', over)
      : entry({ type: i % 3 === 0 ? 'user' : 'agent', attachments: [], citations: [], text: inline(
        i % 3 === 0 ? 'Keep the conversation steady while its messages are loading.'
          : i === 23 ? `Latest reply for ${id}. The conversation is ready to read.`
          : 'The first messages and the team’s contributions appear together, with your place already set.\n\n'.repeat(i === 20 ? 100 : 2)) }, over);
    return { ...row, session_id: id };
  });
}
const subscriptions = [];
const teams = new Map();
client.subscribeSession = (id, handlers) => {
  const record = { id, handlers, closed: false, rows: [], anchorReads: 0 };
  subscriptions.push(record);
  return {
    entries: () => record.rows,
    close: () => { record.closed = true; },
    fetchPage: async () => ({ entries: [], cursor: null }), reveal: nothing,
    loadEntry: async timelineId => { record.anchorReads += 1; return record.rows.some(row => row.timeline_id === timelineId); },
  };
};
client.subscribeCollaboration = (id, handlers) => {
  teams.set(id, handlers);
  return { close: nothing, restart: () => handlers.onReset() };
};
const active = id => subscriptions.findLast(record => record.id === id && !record.closed);
function release(id) {
  const record = active(id);
  record.rows = rowsFor(id);
  record.handlers.onStatus('live');
  record.handlers.onSnapshot({ entries: record.rows, cursor: null });
  record.handlers.onChange(record.rows);
  record.handlers.onTimelineMetaChange({ tasks: [], workflows: [], plans: [], artifacts: [] });
}
function releaseTeam(id = 'worker') {
  const row = { ...entry({ type: 'collaboration', event: {
    kind: 'message', channel: 'team', sender: 'Reviewer', recipient: 'lead', team: 'review', color: null,
    summary: null, body: inline('The team review is complete. All checks passed.'), attachments: [], task_id: null,
    delivery_id: null, status: null, authored_at: now + 22500, source_session_id: null, source_response_id: null,
    work_start_timeline_id: null, work_end_timeline_id: null,
  } }, { timeline_id: `${id}-report`, ordinal: 1, at: now + 22500 }), session_id: id };
  teams.get(id).onChange({ entries: [row], response_runtimes: [] });
}
function Fixture() {
  const [selected, setSelected] = useState('alpha');
  const [roster, setRoster] = useState([...sessions, worker]);
  const [focus, setFocus] = useState(null);
  const feed = useTimeline(useRoom().sessions, selected);
  window.loadingFixture = {
    feed, subscriptions, teams, flush: flushSync, release, releaseTeam,
    select(id, seek = false) { setSelected(id); setFocus(seek ? { sessionId: id, timelineId: `${id}-3` } : null); },
    reset(id) { active(id).handlers.onReset(); },
    reconnect(id) { active(id).handlers.onStatus('connecting'); },
    fail(id) { active(id).handlers.onError({ code: 'internal', message: 'The message service is unavailable. Please try again.' }); active(id).handlers.onStatus('closed'); },
    addTeam() { setRoster(previous => [...previous, { ...worker, id: 'second-worker' }]); },
  };
  return <div className="ah-frame"><div className="ah-room">
    <div className="ah-cols" style={{ gridTemplateColumns: '220px minmax(0, 1fr)' }}>
      <div className="ah-col"><header><span className="ah-eyebrow">Sessions</span></header>
        <div style={{ padding: 10, display: 'grid', gap: 6 }}>{sessions.map(session => <button key={session.id}
          onClick={() => { setSelected(session.id); setFocus(null); }}
          style={{ padding: '12px 10px', textAlign: 'left', borderRadius: 8, border: 0,
            background: selected === session.id ? 'var(--vf-fill)' : 'transparent', color: 'var(--vf-text-secondary)' }}>
          {session.title}
        </button>)}</div>
      </div>
      <TranscriptColumn session={sessions.find(session => session.id === selected)} sessions={roster} feed={feed}
        duringBackfill={false} project={null} focus={focus} onFocusConsumed={() => setFocus(null)} onReceipt={nothing}
        stepper={null} onStepMatch={nothing} onDismissStepper={nothing} getRuntimeKind={() => 'claude-code'} homeDirectory={null} />
    </div>
  </div></div>;
}
createRoot(document.getElementById('root')).render(<RoomProvider ports={ports}><Fixture /></RoomProvider>);
