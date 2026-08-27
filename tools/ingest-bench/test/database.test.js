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
    CREATE TABLE agent_runtime(id TEXT PRIMARY KEY);
    CREATE TABLE project(id TEXT PRIMARY KEY);
    CREATE TABLE session(id TEXT PRIMARY KEY);
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
  assert.ok(overview.objects.some((object) => object.name === 'message'));

  const page = projection.table('message', { limit: 1, offset: 1 });
  assert.equal(page.total, 2);
  assert.equal(page.rows[0].content, 'world');

  const query = projection.query('SELECT type, count(*) AS rows FROM message GROUP BY type ORDER BY type');
  assert.deepEqual(query.rows, [{ type: 'agent', rows: 1 }, { type: 'user', rows: 1 }]);
  assert.throws(() => projection.query("DELETE FROM message"), /only SELECT/);
});

test('identifier quoting and values are safe for IPC', () => {
  assert.equal(quoteIdentifier('odd"name'), '"odd""name"');
  assert.equal(safeValue(42n), 42);
  assert.equal(safeValue(BigInt(Number.MAX_SAFE_INTEGER) + 1n), '9007199254740992');
  assert.deepEqual(safeValue(Buffer.from([0xde, 0xad])), {
    $type: 'blob', bytes: 2, hex: 'dead', truncated: false,
  });
});
