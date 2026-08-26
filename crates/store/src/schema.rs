//! The projection DDL (docs/canonical-model.html, "The SQLite projection").
//! Stamped with a schema version; on mismatch the projection REBUILDS from
//! the ledger instead of migrating — rule 4 as code, migrations never exist.

use rusqlite::Connection;

use crate::StoreError;

/// Bump on any DDL change; openers with a different stamp drop and recreate.
pub const SCHEMA_VERSION: i64 = 1;

pub const DDL: &str = r#"
-- ————— phase 1 · discovery ——————————————————————————————————————
CREATE TABLE IF NOT EXISTS agent_runtime (
  id            TEXT PRIMARY KEY,
  kind          TEXT NOT NULL,
  data_root     TEXT NOT NULL,
  enabled       INTEGER NOT NULL DEFAULT 0,
  account       TEXT,
  config        TEXT,
  skills        TEXT,
  versions_seen TEXT,
  discovered_at INTEGER NOT NULL
);

-- ————— phase 2 · meta ingest ————————————————————————————————————
CREATE TABLE IF NOT EXISTS project (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  root_paths TEXT NOT NULL,
  git_repo   TEXT
);

CREATE TABLE IF NOT EXISTS session (
  id                TEXT PRIMARY KEY,
  native_session_id TEXT NOT NULL,
  runtime_id        TEXT NOT NULL REFERENCES agent_runtime(id),
  project_id        TEXT REFERENCES project(id),
  parent_session_id TEXT REFERENCES session(id),
  root_session_id   TEXT NOT NULL REFERENCES session(id),
  role              TEXT NOT NULL DEFAULT 'main',
  title             TEXT,
  summary           TEXT,
  started_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL,
  stream_state      TEXT NOT NULL DEFAULT 'pending'
);
CREATE INDEX IF NOT EXISTS session_project ON session(project_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS session_runtime ON session(runtime_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS session_parent  ON session(parent_session_id);

CREATE TABLE IF NOT EXISTS workspace_obs (
  session_id TEXT NOT NULL REFERENCES session(id),
  at         INTEGER NOT NULL,
  cwd        TEXT NOT NULL,
  git_branch TEXT,
  git_repo   TEXT
);
CREATE INDEX IF NOT EXISTS workspace_obs_session ON workspace_obs(session_id, at);

CREATE TABLE IF NOT EXISTS plan (
  session_id TEXT NOT NULL REFERENCES session(id),
  revision   INTEGER NOT NULL,
  content    TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (session_id, revision)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS task (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES session(id),
  status     TEXT NOT NULL,
  content    TEXT NOT NULL,
  created_at INTEGER,
  updated_at INTEGER
);
CREATE INDEX IF NOT EXISTS task_session ON task(session_id);

-- ————— phase 3 · stream —————————————————————————————————————————
CREATE TABLE IF NOT EXISTS ingest_cursor (
  id          INTEGER PRIMARY KEY,
  runtime_id  TEXT NOT NULL REFERENCES agent_runtime(id),
  stream      TEXT NOT NULL,
  path        TEXT NOT NULL,
  checkpoint  TEXT NOT NULL,
  updated_at  INTEGER,
  UNIQUE (runtime_id, path)
);

CREATE TABLE IF NOT EXISTS message (
  id             INTEGER PRIMARY KEY,
  timeline_id    TEXT NOT NULL UNIQUE,
  session_id     TEXT NOT NULL REFERENCES session(id),
  ordinal        INTEGER NOT NULL,
  type           TEXT NOT NULL,
  subtype        TEXT,
  uuid           TEXT,
  parent_uuid    TEXT,
  ts             INTEGER,
  model          TEXT,
  effort         TEXT,
  content        TEXT,
  tool_id        TEXT,
  tool_name      TEXT,
  tool_input     TEXT,
  tool_result    TEXT,
  tool_payload   TEXT,
  tool_is_error  INTEGER,
  branch_tool_id TEXT,
  in_tokens      INTEGER,
  out_tokens     INTEGER,
  cache_r_tokens INTEGER,
  cache_w_tokens INTEGER,
  think_tokens   INTEGER,
  agent_version  TEXT,
  src_cursor     INTEGER REFERENCES ingest_cursor(id),
  src_offset     INTEGER,
  UNIQUE (session_id, ordinal)
);
CREATE INDEX IF NOT EXISTS message_tool ON message(session_id, tool_id) WHERE tool_id IS NOT NULL;

CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
  content, tool_input, tool_result,
  content='message', content_rowid='id',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE IF NOT EXISTS file_edit (
  session_id   TEXT NOT NULL REFERENCES session(id),
  message_id   INTEGER REFERENCES message(id),
  path         TEXT NOT NULL,
  kind         TEXT NOT NULL,
  snapshot_ref TEXT,
  lines_added  INTEGER,
  lines_removed INTEGER,
  at           INTEGER
);
CREATE INDEX IF NOT EXISTS file_edit_session ON file_edit(session_id, at);
CREATE INDEX IF NOT EXISTS file_edit_path    ON file_edit(path);

CREATE TABLE IF NOT EXISTS git_commit (
  commit_id  TEXT NOT NULL,
  session_id TEXT NOT NULL REFERENCES session(id),
  branch     TEXT,
  at         INTEGER,
  PRIMARY KEY (commit_id, session_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS drift_agg (
  runtime_id    TEXT NOT NULL REFERENCES agent_runtime(id),
  agent_version TEXT NOT NULL,
  stream        TEXT NOT NULL,
  record_kind   TEXT NOT NULL,
  error_code    TEXT NOT NULL,
  shape_hash    TEXT NOT NULL,
  count         INTEGER NOT NULL DEFAULT 0,
  first_seen    INTEGER,
  last_seen     INTEGER,
  sample_cursor INTEGER REFERENCES ingest_cursor(id),
  sample_offset INTEGER,
  PRIMARY KEY (runtime_id, agent_version, stream, record_kind, error_code, shape_hash)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS usage_bucket (
  session_id      TEXT NOT NULL REFERENCES session(id),
  hour_ts         INTEGER NOT NULL,
  model           TEXT NOT NULL,
  in_tokens       INTEGER,
  out_tokens      INTEGER,
  cache_r_tokens  INTEGER,
  cache_w_tokens  INTEGER,
  think_tokens    INTEGER,
  message_count   INTEGER NOT NULL DEFAULT 0,
  tool_call_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, hour_ts, model)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS usage_bucket_hour ON usage_bucket(hour_ts);

CREATE VIEW IF NOT EXISTS session_contribution AS
SELECT r.id AS session_id, r.project_id,
       COALESCE(u.tokens, 0)  AS tokens,
       COALESCE(g.commits, 0) AS commits,
       COALESCE(f.edits, 0)   AS edits,
       COALESCE(f.loc, 0)     AS loc
FROM session r
LEFT JOIN (SELECT s.root_session_id rid,
                  SUM(COALESCE(b.in_tokens,0) + COALESCE(b.out_tokens,0)
                      + COALESCE(b.cache_r_tokens,0) + COALESCE(b.cache_w_tokens,0)
                      + COALESCE(b.think_tokens,0)) tokens
           FROM usage_bucket b JOIN session s ON s.id = b.session_id
           GROUP BY rid) u ON u.rid = r.id
LEFT JOIN (SELECT s.root_session_id rid, COUNT(*) commits
           FROM git_commit g JOIN session s ON s.id = g.session_id
           GROUP BY rid) g ON g.rid = r.id
LEFT JOIN (SELECT s.root_session_id rid, COUNT(*) edits,
                  SUM(COALESCE(f.lines_added, 0) + COALESCE(f.lines_removed, 0)) loc
           FROM file_edit f JOIN session s ON s.id = f.session_id
           GROUP BY rid) f ON f.rid = r.id
WHERE r.id = r.root_session_id;
"#;

const DROP_ALL: &str = r#"
DROP VIEW IF EXISTS session_contribution;
DROP TABLE IF EXISTS usage_bucket;
DROP TABLE IF EXISTS drift_agg;
DROP TABLE IF EXISTS git_commit;
DROP TABLE IF EXISTS file_edit;
DROP TABLE IF EXISTS message_fts;
DROP TABLE IF EXISTS message;
DROP TABLE IF EXISTS ingest_cursor;
DROP TABLE IF EXISTS task;
DROP TABLE IF EXISTS plan;
DROP TABLE IF EXISTS workspace_obs;
DROP TABLE IF EXISTS session;
DROP TABLE IF EXISTS project;
DROP TABLE IF EXISTS agent_runtime;
"#;

pub fn init(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    conn.pragma_update(None, "foreign_keys", true)?;

    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != 0 && version != SCHEMA_VERSION {
        // Disposable projection: a stamp mismatch means drop and let the
        // pipeline rebuild everything from the ledger. Never migrate.
        conn.execute_batch(DROP_ALL)?;
    }
    conn.execute_batch(DDL)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}
