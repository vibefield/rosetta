const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { DatabaseSync } = require('node:sqlite');

const { ProjectionDatabase, quoteIdentifier, safeValue } = require('../src/lib/database');

function fixture() {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'rosetta-ingest-bench-'));
  const file = path.join(directory, 'projection.sqlite');
  const db = new DatabaseSync(file);
  db.exec(`
    PRAGMA user_version = 7;
    CREATE TABLE message(id INTEGER PRIMARY KEY, type TEXT, content TEXT);
    INSERT INTO message(type, content) VALUES ('user', 'hello'), ('agent', 'world');
    CREATE TABLE agent_runtime(
      id TEXT PRIMARY KEY, kind TEXT, data_root TEXT, enabled INTEGER, versions_seen TEXT
    );
    INSERT INTO agent_runtime VALUES ('runtime-1', 'codex', '/tmp/.codex', 1, '["0.149.1"]');
    CREATE TABLE project(id TEXT PRIMARY KEY, name TEXT, root_paths TEXT, git_repo TEXT);
    INSERT INTO project VALUES ('project-1', 'rosetta', '["/work/rosetta"]', 'vibefield/rosetta');
    CREATE TABLE session(
      id TEXT PRIMARY KEY, native_session_id TEXT, runtime_id TEXT, project_id TEXT,
      parent_session_id TEXT, root_session_id TEXT, role TEXT, title TEXT, summary TEXT,
      started_at INTEGER, updated_at INTEGER, stream_state TEXT, project_assigned_at INTEGER
    );
    INSERT INTO session VALUES
      ('session-1', 'native-1', 'runtime-1', 'project-1', NULL, 'session-1', 'main',
       'Review Rosetta', 'Inspect the ingest bench', 1000, 2000, 'pending', 1000),
      ('session-2', 'native-2', 'runtime-1', NULL, 'session-1', 'session-1', 'subagent',
       NULL, NULL, 1500, 3000, 'indexed', NULL);
    CREATE TABLE workspace_obs(session_id TEXT, at INTEGER, cwd TEXT, git_branch TEXT, git_repo TEXT);
    INSERT INTO workspace_obs VALUES ('session-1', 1000, '/work/rosetta', 'main', 'vibefield/rosetta');
    CREATE TABLE ingest_cursor(id INTEGER, session_id TEXT);
    INSERT INTO ingest_cursor VALUES (1, 'session-1');
    CREATE TABLE file_edit(id INTEGER);
    CREATE TABLE git_commit(id INTEGER);
    CREATE TABLE drift_agg(id INTEGER);
    CREATE TABLE pending_merge(id INTEGER);
    CREATE TABLE source_op(id INTEGER);
  `);
  db.close();
  return { directory, file };
}

test('projection opens read-only and exposes schema, counts, and pages', (t) => {
  const { directory, file } = fixture();
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const projection = new ProjectionDatabase();
  t.after(() => projection.close());

  assert.equal(projection.open(file).schemaVersion, 7);
  const overview = projection.overview();
  assert.equal(overview.counts.message, 2);
  assert.equal(overview.counts.workspace_obs, 1);
  assert.equal(overview.counts.ingest_cursor, 1);
  assert.ok(overview.objects.some((object) => object.name === 'message'));

  const page = projection.table('message', { limit: 1, offset: 1 });
  assert.equal(page.total, 2);
  assert.equal(page.rows[0].content, 'world');

  const query = projection.query('SELECT type, count(*) AS rows FROM message GROUP BY type ORDER BY type');
  assert.deepEqual(query.rows, [{ type: 'agent', rows: 1 }, { type: 'user', rows: 1 }]);
  assert.throws(() => projection.query("DELETE FROM message"), /only SELECT/);
});

test('meta data joins projects and sessions to their owning agent and workspace', (t) => {
  const { directory, file } = fixture();
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const projection = new ProjectionDatabase();
  t.after(() => projection.close());
  projection.open(file);

  const data = projection.metaData();
  assert.deepEqual(data.projects, [{
    id: 'project-1',
    name: 'rosetta',
    rootPaths: ['/work/rosetta'],
    gitRepo: 'vibefield/rosetta',
    sessionCount: 1,
    mainSessionCount: 1,
    childSessionCount: 0,
    latestUpdatedAt: 2000,
    agentKinds: ['codex'],
  }]);
  const session = data.sessions.find((row) => row.id === 'session-1');
  assert.equal(session.title, 'Review Rosetta');
  assert.equal(session.agentKind, 'codex');
  assert.equal(session.projectName, 'rosetta');
  assert.equal(session.cwd, '/work/rosetta');
  assert.equal(session.sourceCount, 1);
  assert.deepEqual(session.versionsSeen, ['0.149.1']);
  assert.equal(data.sessions[0].id, 'session-2', 'sessions are newest first');
});

test('identifier quoting and values are safe for IPC', () => {
  assert.equal(quoteIdentifier('odd"name'), '"odd""name"');
  assert.equal(safeValue(42n), 42);
  assert.equal(safeValue(BigInt(Number.MAX_SAFE_INTEGER) + 1n), '9007199254740992');
  assert.deepEqual(safeValue(Buffer.from([0xde, 0xad])), {
    $type: 'blob', bytes: 2, hex: 'dead', truncated: false,
  });
});
