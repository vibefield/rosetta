//! The projection DDL (docs/canonical-model.html, "The SQLite projection").
//! Stamped with a schema version. A mismatch fails closed on ordinary open;
//! replacement happens only through an explicit validated rebuild or fresh
//! source ingest.

use rusqlite::Connection;

use crate::StoreError;

/// Bump on any DDL or incompatible projection-semantic change. Existing
/// nonzero versions fail closed.
pub const SCHEMA_VERSION: i64 = 22;

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
  canonical_ready   INTEGER NOT NULL DEFAULT 0 CHECK (canonical_ready IN (0, 1)),
  -- The price catalog `usage_bucket` costs were computed with. A writer that
  -- finds another version re-prices the rollup from stored usage facts.
  pricing_version   TEXT
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
  cwd               TEXT,
  parent_session_id TEXT REFERENCES session(id),
  delegation        TEXT CHECK (delegation IS NULL OR json_valid(delegation)),
  root_session_id   TEXT NOT NULL REFERENCES session(id),
  role              TEXT NOT NULL DEFAULT 'main',
  title             TEXT,
  -- Minimal provenance for the one selected user-facing title. Losing title
  -- candidates remain in process RAM/the raw ledger; they are never a second
  -- durable fact set. Same-session sibling replay restores the next winner
  -- when a source generation disappears.
  title_rank        INTEGER NOT NULL DEFAULT 0 CHECK (title_rank BETWEEN 0 AND 3),
  title_at          INTEGER,
  title_src_cursor  INTEGER,
  summary           TEXT,
  started_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL,
  stream_state      TEXT NOT NULL DEFAULT 'pending'
);
CREATE INDEX IF NOT EXISTS session_parent ON session(parent_session_id);

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
  started_at INTEGER,
  terminal_at INTEGER,
  src_cursor INTEGER REFERENCES ingest_cursor(id)
);
CREATE INDEX IF NOT EXISTS task_src ON task(src_cursor);

-- Workflow attempts remain ordinary timeline tool rows. The mutable native
-- sidecar is one generation-owned run document joined to those attempts by
-- run_id at read time, so a resume does not duplicate the worker roster and
-- structured result into every invocation.
CREATE TABLE IF NOT EXISTS workflow_run (
  session_id  TEXT NOT NULL REFERENCES session(id),
  run_id      TEXT NOT NULL,
  detail      TEXT NOT NULL CHECK (json_valid(detail)),
  updated_at  INTEGER,
  src_cursor  INTEGER REFERENCES ingest_cursor(id),
  PRIMARY KEY (session_id, run_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS workflow_run_src ON workflow_run(src_cursor);

-- The coordinator journal is append-only evidence and may arrive before its
-- run snapshot. Avoid a run foreign key for that valid ordering; hydration
-- attaches matching observations once the run exists.
CREATE TABLE IF NOT EXISTS workflow_journal (
  observation_id TEXT PRIMARY KEY,
  run_id          TEXT NOT NULL,
  session_id      TEXT NOT NULL REFERENCES session(id),
  detail          TEXT NOT NULL CHECK (json_valid(detail)),
  src_cursor      INTEGER REFERENCES ingest_cursor(id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS workflow_journal_run ON workflow_journal(session_id, run_id);
CREATE INDEX IF NOT EXISTS workflow_journal_src ON workflow_journal(src_cursor);

-- ————— phase 3 · stream —————————————————————————————————————————
CREATE TABLE IF NOT EXISTS ingest_cursor (
  id          INTEGER PRIMARY KEY,
  runtime_id  TEXT NOT NULL REFERENCES agent_runtime(id),
  stream      TEXT NOT NULL,
  path        TEXT NOT NULL,
  -- Ledger-only sources retain bounded progress without a canonical session.
  session_id  TEXT REFERENCES session(id),
  active_generation INTEGER NOT NULL DEFAULT 0,
  -- Monotonic even when an unclean staged generation is abandoned. Ledger
  -- generation ids therefore never alias after a retry or process restart.
  highest_generation INTEGER NOT NULL DEFAULT 0,
  checkpoint  TEXT NOT NULL,
  updated_at  INTEGER,
  UNIQUE (runtime_id, path)
);

CREATE TABLE IF NOT EXISTS message (
  id             INTEGER PRIMARY KEY,
  timeline_id    TEXT NOT NULL UNIQUE,
  session_id     TEXT NOT NULL REFERENCES session(id),
  ordinal        INTEGER NOT NULL,
  group_id       TEXT NOT NULL,
  group_kind     TEXT NOT NULL CHECK (group_kind IN
                    ('human_turn','agent_cycle','parent_invocation','system_cycle','synthetic')),
  group_derivation TEXT NOT NULL CHECK (group_derivation IN
                    ('native','parent_linked','ordered','synthetic')),
  response_id    TEXT,
  response_derivation TEXT CHECK (response_derivation IS NULL OR response_derivation IN
                    ('native','parent_linked','ordered','synthetic')),
  block_index    INTEGER CHECK (block_index IS NULL OR block_index >= 0),
  type           TEXT NOT NULL CHECK (type IN
                    ('user','agent','thinking','collaboration','tool_use','notice','summary')),
  uuid           TEXT,
  parent_uuid    TEXT,
  ts             INTEGER,
  activity_first_at INTEGER,
  activity_last_at  INTEGER,
  model          TEXT,
  effort         TEXT,
  content        TEXT,                    -- inline prose or member preview
  detail         TEXT CHECK (detail IS NULL OR json_valid(detail)),
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
  tool_error_kind TEXT CHECK (tool_error_kind IS NULL OR tool_error_kind IN
                    ('permission','not_found','invalid_input','exit_nonzero',
                     'timeout','cancelled','provider','unknown')),
  tool_error_summary TEXT,
  tool_diagnostic TEXT,
  tool_started_at INTEGER,
  tool_completed_at INTEGER,
  tool_duration_ms INTEGER CHECK (tool_duration_ms IS NULL OR tool_duration_ms >= 0),
  parent_tool_native_id TEXT,
  in_tokens      INTEGER,
  out_tokens     INTEGER,
  cache_r_tokens INTEGER,
  cache_w_tokens INTEGER,
  think_tokens   INTEGER,
  -- Usage facts beside the six mutually exclusive token buckets
  -- (ARCHITECTURE §4.1.3). Costs are computed in usage_bucket, never here.
  cache_w1h_tokens INTEGER,
  web_searches   INTEGER,
  service_tier   TEXT CHECK (service_tier IS NULL OR service_tier IN
                    ('standard','priority','flex','batch','fast')),
  route          TEXT,
  billed_pusd    INTEGER,
  -- NULL for one model request; 'aggregate' when the agent reported a total
  -- over several requests on this response.
  usage_scope    TEXT CHECK (usage_scope IS NULL OR usage_scope = 'aggregate'),
  agent_version  TEXT,
  src_cursor     INTEGER REFERENCES ingest_cursor(id),
  src_offset     INTEGER CHECK (src_offset IS NULL OR src_offset >= 0),
  UNIQUE (session_id, ordinal),
  CHECK ((type = 'tool_use' AND content IS NULL AND detail IS NULL
          AND tool_correlation_key IS NOT NULL AND tool_correlation_kind IS NOT NULL
          AND tool_name IS NOT NULL AND tool_kind IS NOT NULL
          AND tool_status IS NOT NULL AND tool_status_basis IS NOT NULL
          AND tool_detail IS NOT NULL
          AND json_extract(tool_detail, '$.type') = tool_kind)
      OR (type != 'tool_use' AND tool_correlation_key IS NULL
          AND tool_correlation_kind IS NULL AND tool_native_id IS NULL
          AND tool_namespace IS NULL AND tool_provider IS NULL AND tool_name IS NULL
          AND tool_kind IS NULL AND tool_status IS NULL AND tool_status_basis IS NULL
          AND tool_detail IS NULL
          AND parent_tool_native_id IS NULL
          AND tool_error_kind IS NULL AND tool_error_summary IS NULL
          AND tool_diagnostic IS NULL AND tool_started_at IS NULL
          AND tool_completed_at IS NULL AND tool_duration_ms IS NULL)),
  CHECK ((type IN ('user','agent') AND (content IS NOT NULL OR detail IS NOT NULL))
      OR (type = 'thinking' AND content IS NOT NULL)
      OR (type = 'collaboration' AND detail IS NOT NULL)
      OR (type = 'summary' AND content IS NOT NULL AND detail IS NOT NULL)
      OR (type = 'notice' AND content IS NULL AND detail IS NOT NULL)
      OR (type = 'tool_use' AND content IS NULL)),
  CHECK (detail IS NULL OR
      (type IN ('user','agent') AND json_extract(detail, '$.type') = 'prose') OR
      (type = 'thinking' AND json_extract(detail, '$.type') = 'thinking') OR
      (type = 'collaboration' AND json_extract(detail, '$.type') = 'collaboration') OR
      (type = 'notice' AND json_extract(detail, '$.type') = 'notice') OR
      (type = 'summary' AND json_extract(detail, '$.type') = 'summary')),
  CHECK ((response_id IS NULL AND response_derivation IS NULL AND block_index IS NULL)
      OR (response_id IS NOT NULL AND response_derivation IS NOT NULL
          AND block_index IS NOT NULL)),
  CHECK (tool_status_basis != 'source_finalization' OR tool_status = 'unknown'),
  CHECK (tool_error_summary IS NULL OR tool_error_kind IS NOT NULL),
  CHECK (tool_diagnostic IS NULL OR tool_error_kind IS NOT NULL),
  CHECK (tool_completed_at IS NULL OR tool_started_at IS NULL
      OR tool_completed_at >= tool_started_at)
);
CREATE UNIQUE INDEX IF NOT EXISTS message_tool_correlation
  ON message(session_id, tool_correlation_key) WHERE type = 'tool_use';
CREATE UNIQUE INDEX IF NOT EXISTS message_response_block
  ON message(session_id, response_id, block_index) WHERE response_id IS NOT NULL;
-- Workflow attempts are ordinary tool rows. This expression index lets the
-- read-time run join recover all attempts without scanning every tool in a
-- long session or persisting a second attempt representation.
CREATE INDEX IF NOT EXISTS message_workflow_run
  ON message(session_id, json_extract(tool_detail, '$.run_id'), ordinal)
  WHERE type = 'tool_use' AND tool_kind = 'workflow';

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
  -- One model request, or a total over several: a long-context price tier
  -- can only be decided per request.
  scope          TEXT NOT NULL DEFAULT 'request' CHECK (scope IN ('request','aggregate')),
  in_tokens      INTEGER,
  out_tokens     INTEGER,
  cache_r_tokens INTEGER,
  cache_w_tokens INTEGER,
  think_tokens   INTEGER,
  cache_w1h_tokens INTEGER,
  web_searches   INTEGER,
  service_tier   TEXT CHECK (service_tier IS NULL OR service_tier IN
                    ('standard','priority','flex','batch','fast')),
  route          TEXT,
  billed_pusd    INTEGER,
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
  cache_w1h_tokens INTEGER,
  web_searches    INTEGER,
  -- List-price-equivalent pico-USD of the records the catalog priced (NULL
  -- when none), provider-billed pico-USD (NULL when none reported), and the
  -- tokens of records it could not price. Coverage follows from these.
  cost_pusd       INTEGER,
  billed_pusd     INTEGER,
  unpriced_tokens INTEGER NOT NULL DEFAULT 0,
  message_count   INTEGER NOT NULL DEFAULT 0,
  tool_call_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, hour_ts, model)
) WITHOUT ROWID;

CREATE VIEW IF NOT EXISTS session_contribution AS
SELECT r.id AS session_id, r.project_id,
       COALESCE(u.tokens, 0)  AS tokens,
       u.cost_pusd            AS cost_pusd,
       COALESCE(u.unpriced_tokens, 0) AS unpriced_tokens,
       COALESCE(g.commits, 0) AS commits,
       COALESCE(f.edits, 0)   AS edits,
       COALESCE(f.loc, 0)     AS loc
FROM session r
LEFT JOIN (SELECT s.root_session_id rid,
                  SUM(COALESCE(b.in_tokens,0) + COALESCE(b.out_tokens,0)
                      + COALESCE(b.cache_r_tokens,0) + COALESCE(b.cache_w_tokens,0)
                      + COALESCE(b.cache_w1h_tokens,0) + COALESCE(b.think_tokens,0)) tokens,
                  SUM(b.cost_pusd) cost_pusd, SUM(b.unpriced_tokens) unpriced_tokens
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

-- Keep these during cold loads: progressive metadata reads must remain bounded.
CREATE INDEX IF NOT EXISTS message_activity_first ON message(session_id, activity_first_at);
CREATE INDEX IF NOT EXISTS message_activity_last ON message(session_id, activity_last_at);
"#;

/// Read/presentation indexes that are unnecessary while an unpublished,
/// empty projection is being rebuilt. Uniqueness, merge-key, and provenance
/// indexes stay live because the write path relies on them for correctness or
/// bounded reconciliation.
pub(crate) const BULK_DROP_INDEXES: &str = r#"
DROP INDEX IF EXISTS session_project;
DROP INDEX IF EXISTS session_runtime;
DROP INDEX IF EXISTS task_session;
DROP INDEX IF EXISTS message_bucket_key;
DROP INDEX IF EXISTS timeline_meta_message;
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
CREATE INDEX IF NOT EXISTS session_title_src ON session(title_src_cursor);
CREATE INDEX IF NOT EXISTS task_session ON task(session_id);
CREATE INDEX IF NOT EXISTS message_bucket_key ON message(
  session_id, COALESCE(model, 'unknown'), COALESCE(ts, 0)
);
CREATE INDEX IF NOT EXISTS timeline_meta_message ON message(session_id, ordinal)
  WHERE tool_kind IN ('task', 'plan', 'artifact');
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
    crate::pricing::register(conn)?;
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
