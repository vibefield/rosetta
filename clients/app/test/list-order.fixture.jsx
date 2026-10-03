// Production metadata hook, ranking, list controls and layout; synthetic data only.
import { StrictMode, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { ProjectsColumn } from '../renderer/src/components/projects/ProjectsColumn';
import { SessionList } from '../renderer/src/components/sessions/SessionList';
import { useMeta } from '../renderer/src/lib/hooks';
import { useActivityOrder } from '../renderer/src/lib/use-activity-order';
import { compareOrderedSessions } from '../renderer/src/lib/activity-order';
import { buildProjectViewModel } from '../renderer/src/lib/model';
import { RoomProvider } from '../renderer/src/room-context';
import { client, ports } from './support/room-ports.mjs';
import '../renderer/src/styles/tokens.css';
import '../renderer/src/styles/room.css';
import '../renderer/src/styles/ingest.css';

const start = new Date(2026, 9, 2, 12).getTime();
const nothing = () => {};
let current = start;
let handlers;
Date.now = () => current;
client.subscribeMeta = next => {
  handlers = next;
  return { close() { if (handlers === next) handlers = null; } };
};
const projects = ['rosetta', 'website', 'tools'].map(id => ({ id, name: id, root_paths: [`/demo/${id}`], git_repo: null }));
function session(id, project, at) {
  return {
    id, native_session_id: id, runtime_id: 'codex', project_id: project,
    parent_session_id: null, root_session_id: id, delegation: null, role: 'main',
    cwd: `/demo/${project}`, title: id, summary: null, started_at: at, updated_at: at,
    activity: { first_at: at, last_at: at }, stream_state: 'indexed',
  };
}
let rows;
function resetRows() {
  current = start;
  rows = [
    session('parser', 'rosetta', start), session('sidebar', 'rosetta', start - 1000),
    session('deploy', 'website', start - 2000), session('exports', 'tools', start - 86400000),
    ...Array.from({ length: 18 }, (_, i) => session(`older-${i}`, 'rosetta', start - 86400000 - i * 60_000)),
  ];
}
resetRows();
const publish = source => {
  handlers.onStatus('live');
  handlers.onChange({ projects, sessions: [...rows], runtimes: [{ id: 'codex', kind: 'codex' }] }, source);
};
function Fixture() {
  const { meta, snapshotVersion, status } = useMeta(ports.catalog);
  const [clock, setClock] = useState(start);
  const [selected, select] = useState('parser');
  const [projectId, setProject] = useState('rosetta');
  const [sort, setSort] = useState('recent');
  const [bootstrap, setBootstrap] = useState(false);
  const order = useActivityOrder(meta, snapshotVersion, status === 'live' && !bootstrap, bootstrap, clock);
  const sessions = (meta?.sessions ?? []).filter(row => row.project_id === projectId)
    .sort((a, b) => compareOrderedSessions(a, b, order));
  const projectModels = (meta?.projects ?? []).map(project =>
    buildProjectViewModel(project, meta.sessions, undefined, () => 'codex', clock));
  window.listOrderFixture = {
    get ready() { return Boolean(handlers); }, order, snapshotVersion, flush: flushSync,
    initial() { publish('snapshot'); },
    update(id, advance = 1000) {
      current += advance;
      rows = rows.map(row => row.id === id ? {
        ...row, title: `${id} · updated`, activity: { ...row.activity, last_at: current },
      } : row);
      setClock(current); publish('mutation');
    },
    tick(ms) { current += ms; setClock(current); },
    snapshot(id, advance = 600_000) {
      current += advance;
      rows = rows.map(row => row.id === id ? { ...row, activity: { ...row.activity, last_at: current } } : row);
      // All callbacks may be batched into a single React commit.
      handlers.onStatus('connecting'); handlers.onStatus('syncing'); publish('snapshot');
      setClock(current);
    },
    replaceAndUpdate(id) {
      this.snapshot(id);
      this.update(id);
    },
    add(id, at = current) { rows = [...rows, session(id, 'rosetta', at)]; publish('mutation'); },
    remove(id) { rows = rows.filter(row => row.id !== id); publish('snapshot'); },
    project: setProject, sort: setSort, bootstrap: setBootstrap,
    title(id) { return rows.find(row => row.id === id)?.title; },
  };
  return <div className="ah-frame"><div className="ah-room" style={{ display: 'flex', flexDirection: 'column' }}>
    <button type="button" data-outside onClick={nothing} style={{ alignSelf: 'flex-start', padding: 10 }}>Outside the lists</button>
    <div className="ah-cols" style={{ gridTemplateColumns: '260px 300px minmax(0, 1fr)', flex: 1, minHeight: 0 }}>
      <ProjectsColumn projects={projectModels} activityOrder={order} selectedProjectId={projectId}
        sort={sort} onSortChange={setSort} homeDirectory="/demo" currentTimestamp={clock}
        ingest={null} activity={null} onProjectSelect={setProject} />
      <div className="ah-col" data-sessions><header>Sessions</header>
        <SessionList sessions={sessions} selectedSessionId={selected} factsBySessionId={new Map()}
          getRuntimeKind={() => 'codex'} currentTimestamp={clock} duringBackfill={false} pending={false}
          onSessionSelect={row => select(row.id)} />
      </div>
      <div className="ah-col" style={{ padding: 20 }}>Selected session: <span data-selected>{selected}</span></div>
    </div>
  </div></div>;
}
createRoot(document.getElementById('root')).render(<RoomProvider ports={ports}><StrictMode><Fixture /></StrictMode></RoomProvider>);
