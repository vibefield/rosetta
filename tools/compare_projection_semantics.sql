-- Compare the database opened as `main` with a second projection attached as
-- `rebuilt`. Surrogate message/cursor rowids may differ with completion-order
-- cold scheduling, so references are resolved through their stable identities.
-- Output columns: relation, main rows, rebuilt rows, main-only, rebuilt-only.

.headers off
.mode list
.separator |

WITH lhs AS (
  SELECT type, name, tbl_name, sql FROM main.sqlite_master
  WHERE name NOT LIKE 'sqlite_stat%'
), rhs AS (
  SELECT type, name, tbl_name, sql FROM rebuilt.sqlite_master
  WHERE name NOT LIKE 'sqlite_stat%'
)
SELECT 'schema', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

-- `discovered_at` is a local observation timestamp, and rebuild deliberately
-- initializes it to zero rather than pretending the ledger observed a root.
WITH lhs AS (
  SELECT id, kind, data_root, enabled, account, config, skills, versions_seen
  FROM main.agent_runtime
), rhs AS (
  SELECT id, kind, data_root, enabled, account, config, skills, versions_seen
  FROM rebuilt.agent_runtime
)
SELECT 'agent_runtime_except_discovered_at',
       (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

-- A new projection intentionally mints a new projection_epoch.
WITH lhs AS (
  SELECT singleton, search_revision, canonical_ready FROM main.projection_state
), rhs AS (
  SELECT singleton, search_revision, canonical_ready FROM rebuilt.projection_state
)
SELECT 'projection_state_except_epoch',
       (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (SELECT * FROM main.search_dirty),
     rhs AS (SELECT * FROM rebuilt.search_dirty)
SELECT 'search_dirty', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (SELECT * FROM main.source_audit),
     rhs AS (SELECT * FROM rebuilt.source_audit)
SELECT 'source_audit', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (SELECT * FROM main.project),
     rhs AS (SELECT * FROM rebuilt.project)
SELECT 'project', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (
  SELECT s.id, s.native_session_id, s.runtime_id, s.project_id, s.cwd,
         s.parent_session_id, s.root_session_id, s.role, s.title, s.title_rank,
         s.title_at, tc.runtime_id, tc.path, s.summary, s.started_at,
         s.updated_at, s.stream_state
  FROM main.session s
  LEFT JOIN main.ingest_cursor tc ON tc.id = s.title_src_cursor
), rhs AS (
  SELECT s.id, s.native_session_id, s.runtime_id, s.project_id, s.cwd,
         s.parent_session_id, s.root_session_id, s.role, s.title, s.title_rank,
         s.title_at, tc.runtime_id, tc.path, s.summary, s.started_at,
         s.updated_at, s.stream_state
  FROM rebuilt.session s
  LEFT JOIN rebuilt.ingest_cursor tc ON tc.id = s.title_src_cursor
)
SELECT 'session', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (
  SELECT p.session_id, p.revision, p.content, p.updated_at, c.runtime_id, c.path
  FROM main.plan p LEFT JOIN main.ingest_cursor c ON c.id = p.src_cursor
), rhs AS (
  SELECT p.session_id, p.revision, p.content, p.updated_at, c.runtime_id, c.path
  FROM rebuilt.plan p LEFT JOIN rebuilt.ingest_cursor c ON c.id = p.src_cursor
)
SELECT 'plan', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (
  SELECT t.id, t.session_id, t.status, t.content, t.created_at, t.updated_at,
         c.runtime_id, c.path
  FROM main.task t LEFT JOIN main.ingest_cursor c ON c.id = t.src_cursor
), rhs AS (
  SELECT t.id, t.session_id, t.status, t.content, t.created_at, t.updated_at,
         c.runtime_id, c.path
  FROM rebuilt.task t LEFT JOIN rebuilt.ingest_cursor c ON c.id = t.src_cursor
)
SELECT 'task', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (
  SELECT runtime_id, stream, path, session_id, active_generation,
         highest_generation,
         json_remove(checkpoint, '$.tail', '$.snapshot_stamp',
                     '$.snapshot_revision') AS restart_state,
         updated_at
  FROM main.ingest_cursor
), rhs AS (
  SELECT runtime_id, stream, path, session_id, active_generation,
         highest_generation,
         json_remove(checkpoint, '$.tail', '$.snapshot_stamp',
                     '$.snapshot_revision') AS restart_state,
         updated_at
  FROM rebuilt.ingest_cursor
)
SELECT 'ingest_cursor_except_source_anchors',
       (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (
  SELECT m.timeline_id, m.session_id, m.ordinal, m.group_id, m.group_kind,
         m.group_derivation, m.response_id, m.response_derivation,
         m.block_index, m.type, m.uuid, m.parent_uuid, m.ts, m.model, m.effort,
         m.content, m.detail, m.tool_correlation_key,
         m.tool_correlation_kind, m.tool_native_id, m.tool_namespace,
         m.tool_provider, m.tool_name, m.tool_kind, m.tool_status,
         m.tool_status_basis, m.tool_detail, m.tool_error_kind,
         m.tool_error_summary, m.tool_diagnostic, m.tool_started_at,
         m.tool_completed_at, m.tool_duration_ms, m.parent_tool_native_id,
         m.in_tokens, m.out_tokens, m.cache_r_tokens, m.cache_w_tokens,
         m.think_tokens, m.agent_version, c.runtime_id, c.path, m.src_offset
  FROM main.message m
  LEFT JOIN main.ingest_cursor c ON c.id = m.src_cursor
), rhs AS (
  SELECT m.timeline_id, m.session_id, m.ordinal, m.group_id, m.group_kind,
         m.group_derivation, m.response_id, m.response_derivation,
         m.block_index, m.type, m.uuid, m.parent_uuid, m.ts, m.model, m.effort,
         m.content, m.detail, m.tool_correlation_key,
         m.tool_correlation_kind, m.tool_native_id, m.tool_namespace,
         m.tool_provider, m.tool_name, m.tool_kind, m.tool_status,
         m.tool_status_basis, m.tool_detail, m.tool_error_kind,
         m.tool_error_summary, m.tool_diagnostic, m.tool_started_at,
         m.tool_completed_at, m.tool_duration_ms, m.parent_tool_native_id,
         m.in_tokens, m.out_tokens, m.cache_r_tokens, m.cache_w_tokens,
         m.think_tokens, m.agent_version, c.runtime_id, c.path, m.src_offset
  FROM rebuilt.message m
  LEFT JOIN rebuilt.ingest_cursor c ON c.id = m.src_cursor
)
SELECT 'message', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (
  SELECT f.session_id, m.timeline_id, f.path, f.kind, f.snapshot_ref,
         f.lines_added, f.lines_removed, f.at, c.runtime_id, c.path
  FROM main.file_edit f
  LEFT JOIN main.message m ON m.id = f.message_id
  LEFT JOIN main.ingest_cursor c ON c.id = f.src_cursor
), rhs AS (
  SELECT f.session_id, m.timeline_id, f.path, f.kind, f.snapshot_ref,
         f.lines_added, f.lines_removed, f.at, c.runtime_id, c.path
  FROM rebuilt.file_edit f
  LEFT JOIN rebuilt.message m ON m.id = f.message_id
  LEFT JOIN rebuilt.ingest_cursor c ON c.id = f.src_cursor
)
SELECT 'file_edit', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (
  SELECT g.commit_id, g.session_id, g.branch, g.at, c.runtime_id, c.path
  FROM main.git_commit g LEFT JOIN main.ingest_cursor c ON c.id = g.src_cursor
), rhs AS (
  SELECT g.commit_id, g.session_id, g.branch, g.at, c.runtime_id, c.path
  FROM rebuilt.git_commit g LEFT JOIN rebuilt.ingest_cursor c ON c.id = g.src_cursor
)
SELECT 'git_commit', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (
  SELECT d.runtime_id, d.agent_version, d.stream, d.record_kind, d.error_code,
         d.shape_hash, d.count, d.first_seen, d.last_seen, c.runtime_id, c.path,
         d.sample_offset
  FROM main.drift_agg d
  LEFT JOIN main.ingest_cursor c ON c.id = d.sample_cursor
), rhs AS (
  SELECT d.runtime_id, d.agent_version, d.stream, d.record_kind, d.error_code,
         d.shape_hash, d.count, d.first_seen, d.last_seen, c.runtime_id, c.path,
         d.sample_offset
  FROM rebuilt.drift_agg d
  LEFT JOIN rebuilt.ingest_cursor c ON c.id = d.sample_cursor
)
SELECT 'drift_agg', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (SELECT * FROM main.drift_seen),
     rhs AS (SELECT * FROM rebuilt.drift_seen)
SELECT 'drift_seen', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (
  SELECT u.session_id, u.sample_key, u.ts, u.model, u.in_tokens, u.out_tokens,
         u.cache_r_tokens, u.cache_w_tokens, u.think_tokens, c.runtime_id, c.path
  FROM main.session_usage u
  LEFT JOIN main.ingest_cursor c ON c.id = u.src_cursor
), rhs AS (
  SELECT u.session_id, u.sample_key, u.ts, u.model, u.in_tokens, u.out_tokens,
         u.cache_r_tokens, u.cache_w_tokens, u.think_tokens, c.runtime_id, c.path
  FROM rebuilt.session_usage u
  LEFT JOIN rebuilt.ingest_cursor c ON c.id = u.src_cursor
)
SELECT 'session_usage', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (SELECT * FROM main.usage_bucket),
     rhs AS (SELECT * FROM rebuilt.usage_bucket)
SELECT 'usage_bucket', (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));

WITH lhs AS (SELECT * FROM main.session_contribution),
     rhs AS (SELECT * FROM rebuilt.session_contribution)
SELECT 'session_contribution',
       (SELECT count(*) FROM lhs), (SELECT count(*) FROM rhs),
       (SELECT count(*) FROM (SELECT * FROM lhs EXCEPT SELECT * FROM rhs)),
       (SELECT count(*) FROM (SELECT * FROM rhs EXCEPT SELECT * FROM lhs));
