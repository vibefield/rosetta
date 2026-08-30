//! The projection DDL (docs/canonical-model.html, "The SQLite projection").
//! Stamped with a schema version. A mismatch fails closed on ordinary open;
//! replacement happens only through an explicit, validated raw-ledger rebuild.

use rusqlite::Connection;

use crate::StoreError;

/// Bump on any DDL change. Existing nonzero versions fail closed.
pub const SCHEMA_VERSION: i64 = 8;

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

-- Whole-projection identity plus the canonical/search synchronization
-- watermark. The epoch is regenerated for every disposable projection; a
-- search generation from another epoch is never serveable. `canonical_ready`
-- is durable because an interrupted progressive first run is intentionally
-- public and resumable, but not yet a fully validated projection.
CREATE TABLE IF NOT EXISTS projection_state (
  singleton         INTEGER PRIMARY KEY CHECK (singleton = 1),
  projection_epoch  TEXT NOT NULL,
  search_revision   INTEGER NOT NULL DEFAULT 0 CHECK (search_revision >= 0),
  canonical_ready   INTEGER NOT NULL DEFAULT 0 CHECK (canonical_ready IN (0, 1))
) WITHOUT ROWID;
INSERT OR IGNORE INTO projection_state
  (singleton, projection_epoch, search_revision, canonical_ready)
VALUES
  (1, lower(hex(randomblob(16))), 0, 0);

-- Coalesced repair keys for the detached search projection. This is not an
-- event log: repeated canonical mutations retain only the newest revision for
-- a session. The asynchronous search worker is the sole consumer.
CREATE TABLE IF NOT EXISTS search_dirty (
  kind      TEXT NOT NULL CHECK (kind IN ('session')),
  key       TEXT NOT NULL,
  revision  INTEGER NOT NULL CHECK (revision >= 0),
  PRIMARY KEY (kind, key)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS search_dirty_revision ON search_dirty(revision);

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
  -- Best bounded meta answer. Streamed title singleton rows may refine the
  -- public title; this is their deterministic fallback after a source sweep.
  meta_title        TEXT,
  meta_title_rank   INTEGER NOT NULL DEFAULT 0,
  meta_title_at     INTEGER,
  summary           TEXT,
  started_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL,
  stream_state      TEXT NOT NULL DEFAULT 'pending',
  -- Internal provenance for the canonical "project at session start" rule.
  -- Earlier observations may refine the assignment; later cwd changes may not.
  project_assigned_at INTEGER
);

CREATE TABLE IF NOT EXISTS workspace_obs (
  session_id TEXT NOT NULL REFERENCES session(id),
  at         INTEGER NOT NULL,
  cwd        TEXT NOT NULL,
  git_branch TEXT,
  git_repo   TEXT,
  src_cursor INTEGER REFERENCES ingest_cursor(id)
);
CREATE INDEX IF NOT EXISTS workspace_obs_src ON workspace_obs(src_cursor);
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
CREATE INDEX IF NOT EXISTS plan_src ON plan(src_cursor);

CREATE TABLE IF NOT EXISTS task (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES session(id),
  status     TEXT NOT NULL,
  content    TEXT NOT NULL,
  created_at INTEGER,
  updated_at INTEGER,
  src_cursor INTEGER REFERENCES ingest_cursor(id)
);
CREATE INDEX IF NOT EXISTS task_src ON task(src_cursor);

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
-- replacement generations and is pruned at activation. A source's first
-- generation projects directly when building a private cold database; there
-- is no old contribution to protect and the database cannot be published
-- before activation and validation. The raw ledger (rosetta-capture) is the
-- durable replay truth; replacements sweep old contributions by src_cursor
-- provenance instead of retaining a permanent op journal. Empty in steady state.
CREATE TABLE IF NOT EXISTS source_op (
  cursor_id   INTEGER NOT NULL REFERENCES ingest_cursor(id),
  generation INTEGER NOT NULL,
  sequence   INTEGER NOT NULL,
  op_json    TEXT NOT NULL,
  PRIMARY KEY (cursor_id, generation, sequence)
) WITHOUT ROWID;
-- The WITHOUT ROWID primary key already is the cursor/generation/sequence
-- index. A duplicate secondary index doubles staged-generation write work.

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
  tool_correlation_key TEXT,
  tool_correlation_kind TEXT CHECK (tool_correlation_kind IS NULL OR tool_correlation_kind IN
                    ('native_id','parent_linked','positional','synthetic')),
  tool_native_id TEXT,
  tool_namespace TEXT,
  tool_provider  TEXT,
  tool_name      TEXT,
  tool_kind      TEXT,
  tool_status    TEXT CHECK (tool_status IS NULL OR tool_status IN
                    ('pending','running','succeeded','failed','denied','cancelled',
                     'timed_out','interrupted','unknown')),
  tool_status_basis TEXT CHECK (tool_status_basis IS NULL OR tool_status_basis IN
                    ('request_observed','native_state','native_terminal_record',
                     'ground_truth_protocol','source_finalization')),
  tool_detail    TEXT CHECK (tool_detail IS NULL OR json_valid(tool_detail)),
  -- Private closed opener used to replay active typed facts when a sibling
  -- source generation is replaced. Never exposed by queries or search.
  tool_open      TEXT CHECK (tool_open IS NULL OR json_valid(tool_open)),
  tool_merge_state TEXT CHECK (tool_merge_state IS NULL OR json_valid(tool_merge_state)),
  tool_error_kind TEXT CHECK (tool_error_kind IS NULL OR tool_error_kind IN
                    ('permission','not_found','invalid_input','exit_nonzero',
                     'timeout','cancelled','provider','unknown')),
  tool_error_summary TEXT,
  tool_diagnostic TEXT,
  tool_started_at INTEGER,
  tool_completed_at INTEGER,
  tool_duration_ms INTEGER CHECK (tool_duration_ms IS NULL OR tool_duration_ms >= 0),
  branch_tool_id TEXT,
  in_tokens      INTEGER,
  out_tokens     INTEGER,
  cache_r_tokens INTEGER,
  cache_w_tokens INTEGER,
  think_tokens   INTEGER,
  agent_version  TEXT,
  src_cursor     INTEGER REFERENCES ingest_cursor(id),
  src_offset     INTEGER CHECK (src_offset IS NULL OR src_offset >= 0),
  UNIQUE (session_id, ordinal),
  CHECK ((type = 'tool_use' AND content IS NULL
          AND tool_correlation_key IS NOT NULL AND tool_correlation_kind IS NOT NULL
          AND tool_name IS NOT NULL AND tool_kind IS NOT NULL
          AND tool_status IS NOT NULL AND tool_status_basis IS NOT NULL
          AND tool_detail IS NOT NULL AND tool_open IS NOT NULL
          AND tool_merge_state IS NOT NULL
          AND json_extract(tool_detail, '$.type') = tool_kind)
      OR (type != 'tool_use' AND tool_correlation_key IS NULL
          AND tool_correlation_kind IS NULL AND tool_native_id IS NULL
          AND tool_namespace IS NULL AND tool_provider IS NULL AND tool_name IS NULL
          AND tool_kind IS NULL AND tool_status IS NULL AND tool_status_basis IS NULL
          AND tool_detail IS NULL AND tool_open IS NULL AND tool_merge_state IS NULL
          AND tool_error_kind IS NULL AND tool_error_summary IS NULL
          AND tool_diagnostic IS NULL AND tool_started_at IS NULL
          AND tool_completed_at IS NULL AND tool_duration_ms IS NULL)),
  CHECK (tool_status_basis != 'source_finalization' OR tool_status = 'unknown'),
  CHECK (tool_error_summary IS NULL OR tool_error_kind IS NOT NULL),
  CHECK (tool_diagnostic IS NULL OR tool_error_kind IS NOT NULL),
  CHECK (tool_completed_at IS NULL OR tool_started_at IS NULL
      OR tool_completed_at >= tool_started_at)
);
CREATE UNIQUE INDEX IF NOT EXISTS message_tool_correlation
  ON message(session_id, tool_correlation_key) WHERE type = 'tool_use';

-- Active, reduced lifecycle evidence. This is not a native-op journal: each
-- row contains only the closed ToolUpdatePatch and exists solely so source
-- generation replacement can subtract a sibling-owned fact and deterministically
-- rebuild the correlated tool row from `tool_open` plus the remaining facts.
CREATE TABLE IF NOT EXISTS tool_update_fact (
  session_id       TEXT NOT NULL REFERENCES session(id),
  correlation_key  TEXT NOT NULL,
  source_identity  TEXT NOT NULL,
  generation       INTEGER NOT NULL CHECK (generation >= 0),
  record_offset    INTEGER NOT NULL CHECK (record_offset >= 0),
  mutation_index   INTEGER NOT NULL CHECK (mutation_index >= 0),
  patch             TEXT NOT NULL CHECK (json_valid(patch)),
  at                INTEGER,
  src_cursor        INTEGER REFERENCES ingest_cursor(id),
  PRIMARY KEY (
    session_id, correlation_key, source_identity,
    generation, record_offset, mutation_index
  )
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS tool_update_fact_target
  ON tool_update_fact(session_id, correlation_key);
CREATE INDEX IF NOT EXISTS tool_update_fact_src ON tool_update_fact(src_cursor);
-- Generation activation and rewrite reconciliation are provenance lookups.
-- Without this index, activating each source scans the entire growing table.
CREATE INDEX IF NOT EXISTS message_src ON message(src_cursor);
-- Match rebuild_bucket's expressions exactly and put both equality keys
-- before the timestamp range so SQLite can constrain all three terms.

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
CREATE INDEX IF NOT EXISTS git_commit_src ON git_commit(src_cursor);

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

-- Native agents such as Hermes report replacement-keyed task/session totals
-- without attributing them to an individual response. Keep those facts
-- canonical instead of fabricating message rows merely to feed the aggregate.
CREATE TABLE IF NOT EXISTS session_usage (
  session_id     TEXT NOT NULL REFERENCES session(id),
  sample_key     TEXT NOT NULL,
  ts             INTEGER NOT NULL,
  model          TEXT,
  in_tokens      INTEGER,
  out_tokens     INTEGER,
  cache_r_tokens INTEGER,
  cache_w_tokens INTEGER,
  think_tokens   INTEGER,
  src_cursor     INTEGER REFERENCES ingest_cursor(id),
  PRIMARY KEY (session_id, sample_key)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS session_usage_src ON session_usage(src_cursor);

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

/// Read/presentation indexes that are unnecessary while an unpublished,
/// empty projection is being rebuilt. Uniqueness, merge-key, and provenance
/// indexes stay live because the write path relies on them for correctness or
/// bounded reconciliation.
pub(crate) const BULK_DROP_INDEXES: &str = r#"
DROP INDEX IF EXISTS session_project;
DROP INDEX IF EXISTS session_runtime;
DROP INDEX IF EXISTS session_parent;
DROP INDEX IF EXISTS workspace_obs_session;
DROP INDEX IF EXISTS task_session;
DROP INDEX IF EXISTS message_bucket_key;
DROP INDEX IF EXISTS session_usage_bucket_key;
DROP INDEX IF EXISTS file_edit_session;
DROP INDEX IF EXISTS file_edit_path;
DROP INDEX IF EXISTS file_edit_message;
DROP INDEX IF EXISTS usage_bucket_hour;
"#;

/// Recreate the deferred indexes once, after all base rows and derived rows
/// have landed. Building a B-tree from the finished table is substantially
/// cheaper than maintaining it for every cold-load insert.
pub(crate) const BULK_CREATE_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS session_project ON session(project_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS session_runtime ON session(runtime_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS session_parent  ON session(parent_session_id);
CREATE INDEX IF NOT EXISTS workspace_obs_session ON workspace_obs(session_id, at);
CREATE INDEX IF NOT EXISTS task_session ON task(session_id);
CREATE INDEX IF NOT EXISTS message_bucket_key ON message(
  session_id, COALESCE(model, 'unknown'), COALESCE(ts, 0)
);
CREATE INDEX IF NOT EXISTS session_usage_bucket_key ON session_usage(
  session_id, COALESCE(model, 'unknown'), ts
);
CREATE INDEX IF NOT EXISTS file_edit_session ON file_edit(session_id, at);
CREATE INDEX IF NOT EXISTS file_edit_path    ON file_edit(path);
CREATE INDEX IF NOT EXISTS file_edit_message ON file_edit(message_id) WHERE message_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS usage_bucket_hour ON usage_bucket(hour_ts);
"#;

fn init_base(conn: &Connection) -> Result<(), StoreError> {
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

/// Initialize a normal reader-ready projection, including every secondary
/// presentation index.
pub fn init(conn: &Connection) -> Result<(), StoreError> {
    init_base(conn)?;
    conn.execute_batch(BULK_CREATE_INDEXES)?;
    Ok(())
}

/// Initialize only correctness-critical tables and indexes. Cold writers use
/// this so a resumed progressive projection does not rebuild large secondary
/// indexes merely to drop them again before continuing.
pub(crate) fn init_deferred(conn: &Connection) -> Result<(), StoreError> {
    init_base(conn)
}
