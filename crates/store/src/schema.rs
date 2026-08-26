//! The projection DDL (docs/canonical-model.html, "The SQLite projection").
//! Stamped with a schema version. A mismatch fails closed on ordinary open;
//! replacement happens only through an explicit, validated raw-ledger rebuild.

use rusqlite::Connection;

use crate::StoreError;

/// Bump on any DDL change. Existing nonzero versions fail closed.
pub const SCHEMA_VERSION: i64 = 4;

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

CREATE TABLE IF NOT EXISTS source_audit (
  runtime_id TEXT PRIMARY KEY REFERENCES agent_runtime(id),
  last_path  TEXT NOT NULL
) WITHOUT ROWID;

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
  stream_state      TEXT NOT NULL DEFAULT 'pending',
  -- Internal provenance for the canonical "project at session start" rule.
  -- Earlier observations may refine the assignment; later cwd changes may not.
  project_assigned_at INTEGER
);
CREATE INDEX IF NOT EXISTS session_project ON session(project_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS session_runtime ON session(runtime_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS session_parent  ON session(parent_session_id);

CREATE TABLE IF NOT EXISTS workspace_obs (
  session_id TEXT NOT NULL REFERENCES session(id),
  at         INTEGER NOT NULL,
  cwd        TEXT NOT NULL,
  git_branch TEXT,
  git_repo   TEXT,
  src_cursor INTEGER REFERENCES ingest_cursor(id)
);
CREATE INDEX IF NOT EXISTS workspace_obs_src ON workspace_obs(src_cursor);
CREATE INDEX IF NOT EXISTS workspace_obs_session ON workspace_obs(session_id, at);
CREATE UNIQUE INDEX IF NOT EXISTS workspace_obs_identity ON workspace_obs(
  session_id, at, cwd, COALESCE(git_branch,''), COALESCE(git_repo,'')
);

CREATE TABLE IF NOT EXISTS plan (
  session_id TEXT NOT NULL REFERENCES session(id),
  revision   INTEGER NOT NULL,
  content    TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  src_cursor INTEGER REFERENCES ingest_cursor(id),
  PRIMARY KEY (session_id, revision)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS task (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES session(id),
  status     TEXT NOT NULL,
  content    TEXT NOT NULL,
  created_at INTEGER,
  updated_at INTEGER,
  src_cursor INTEGER REFERENCES ingest_cursor(id)
);
CREATE INDEX IF NOT EXISTS task_session ON task(session_id);

-- ————— phase 3 · stream —————————————————————————————————————————
CREATE TABLE IF NOT EXISTS ingest_cursor (
  id          INTEGER PRIMARY KEY,
  runtime_id  TEXT NOT NULL REFERENCES agent_runtime(id),
  stream      TEXT NOT NULL,
  path        TEXT NOT NULL,
  session_id  TEXT NOT NULL REFERENCES session(id),
  active_generation INTEGER NOT NULL DEFAULT 0,
  -- Monotonic even when an unclean staged generation is abandoned. Ledger
  -- generation ids therefore never alias after a retry or process restart.
  highest_generation INTEGER NOT NULL DEFAULT 0,
  checkpoint  TEXT NOT NULL,
  updated_at  INTEGER,
  UNIQUE (runtime_id, path)
);

-- STAGING BUFFER, not a representation (rule 4): holds only not-yet-active
-- staged generations of rewritten files and is pruned at activation. The raw
-- ledger (rosetta-capture) is the durable replay truth; a source's old
-- contribution is swept by the src_cursor provenance columns instead of a
-- permanent op journal. Empty in steady state.
CREATE TABLE IF NOT EXISTS source_op (
  cursor_id   INTEGER NOT NULL REFERENCES ingest_cursor(id),
  generation INTEGER NOT NULL,
  sequence   INTEGER NOT NULL,
  op_json    TEXT NOT NULL,
  PRIMARY KEY (cursor_id, generation, sequence)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS source_op_cursor_generation
  ON source_op(cursor_id, generation, sequence);

-- Out-of-order enrichments wait here until their message/tool call exists.
-- This is derived projection state, not a replay representation; generation
-- sweeps remove pending rows by provenance just like the other small tables.
CREATE TABLE IF NOT EXISTS pending_merge (
  kind       TEXT NOT NULL,
  session_id TEXT NOT NULL REFERENCES session(id),
  target     TEXT NOT NULL,
  op_json    TEXT NOT NULL,
  src_cursor INTEGER REFERENCES ingest_cursor(id),
  PRIMARY KEY (kind, session_id, target)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS pending_merge_src ON pending_merge(src_cursor);

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
CREATE INDEX IF NOT EXISTS message_bucket_key ON message(session_id, ts, model);

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
  at           INTEGER,
  src_cursor   INTEGER REFERENCES ingest_cursor(id)
);
CREATE INDEX IF NOT EXISTS file_edit_src ON file_edit(src_cursor);
CREATE INDEX IF NOT EXISTS file_edit_session ON file_edit(session_id, at);
CREATE INDEX IF NOT EXISTS file_edit_path    ON file_edit(path);
CREATE UNIQUE INDEX IF NOT EXISTS file_edit_identity ON file_edit(
  session_id, COALESCE(message_id,-1), path, kind,
  COALESCE(snapshot_ref,''), COALESCE(at,-1)
);

CREATE TABLE IF NOT EXISTS git_commit (
  commit_id  TEXT NOT NULL,
  session_id TEXT NOT NULL REFERENCES session(id),
  branch     TEXT,
  at         INTEGER,
  src_cursor INTEGER REFERENCES ingest_cursor(id),
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

-- Exact normalized occurrences make aggregate application idempotent. The
-- source-op JSON is deterministic, so byte-identical fact JSON is a stable
-- replay identity even for meta/direct batches outside the source journal.
CREATE TABLE IF NOT EXISTS drift_seen (
  event_key TEXT PRIMARY KEY
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

pub fn init(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    conn.pragma_update(None, "foreign_keys", true)?;

    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != 0 && version != SCHEMA_VERSION {
        return Err(StoreError::SchemaVersionMismatch {
            found: version,
            expected: SCHEMA_VERSION,
        });
    }
    conn.execute_batch(DDL)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}
