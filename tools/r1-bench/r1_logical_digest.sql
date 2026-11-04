-- R1 logical digest, format r1-logical-digest/1 (from imgdefer/logical_digest.sql; brief v3 §12 E3).
-- Run read-only against a published projection (the sqlite3 shell provides sha3_agg):
--   /usr/bin/sqlite3 -readonly "file:<DB>?mode=ro&immutable=1" < r1_logical_digest.sql
-- Output lines: section|component|rows|sha3-256   (section count: count|name|value).
-- Every digest is sha3_agg over rows ORDER BY the row text, so physical rowid
-- order (U-2: free to vary per run for root-tree runtimes) never enters it.
--
-- Sections:
--   logical      every canonical table, natural keys only. Surrogates out:
--                message.id (file_edit.message_id mapped to the message's
--                natural key), every src_cursor / title_src_cursor id, and the
--                wall clock html_artifact.captured_at. drift_agg's sample is IN,
--                as (cursor path, offset) (T1: lexicographic min). No
--                image-attachment exclusions: detail / tool_detail are whole.
--                TOTAL folds the 15 table components. Gate: every pair.
--   strict       the cursor-id columns (cursor ids are path rank in every
--                binary: ingest_cursor rows, every table's src_cursor by
--                natural key, session.title_src_cursor, drift sample_cursor
--                and offset). Gate: P0, P1, P2, S, X.
--   strict_noid  strict with every cursor id replaced by its cursor's
--                (runtime_id, path), ingest_cursor without its id. Evidence R
--                (rebuild numbers cursors in ledger frame order; parent
--                decision under U-2: R's strict gate drops surrogate row ids,
--                cursor and message ids). The checkpoint's file evidence
--                ($.tail for JSONL, $.snapshot_stamp / $.snapshot_revision for
--                SQLite members) is its own component,
--                ingest_cursor_file_evidence: a rebuild has no file stat and
--                leaves them null (already so at B0 3ca7410), so TOTAL_R folds
--                every component except that one; TOTAL folds all. Which
--                total gates R is the parent's call. Gate: R.
--   surrogate    message.id and file_edit.message_id by natural key. Reported,
--                never gated (U-2: rowids vary per run for root-tree runtimes;
--                expected equal only between two path walks, e.g. P0 and X).
--   aux          agent_runtime without discovered_at (discovery metadata, not
--                ingest content; versions_seen may follow walk order). Reported.
--   count        row counts.
PRAGMA query_only = 1;
.bail on
.headers off
.mode list
.separator |

-- ------------------------------------------------------------------ logical
WITH
cur(id, p) AS (SELECT id, concat_ws(char(30), runtime_id, path) FROM ingest_cursor),
rows(ord, component, r) AS (
  SELECT 1, 'project', concat_ws(char(31), quote(id), quote(name), quote(root_paths), quote(git_repo)) FROM project
  UNION ALL
  SELECT 2, 'session', concat_ws(char(31), quote(id), quote(native_session_id), quote(runtime_id), quote(project_id), quote(cwd),
    quote(parent_session_id), quote(delegation), quote(root_session_id), quote(role), quote(title), quote(title_rank), quote(title_at),
    quote(summary), quote(started_at), quote(updated_at), quote(stream_state)) FROM session
  UNION ALL
  SELECT 3, 'message_core', concat_ws(char(31), quote(timeline_id), quote(session_id), quote(ordinal), quote(group_id), quote(group_kind),
    quote(group_derivation), quote(response_id), quote(response_derivation), quote(block_index), quote(type), quote(uuid), quote(parent_uuid),
    quote(ts), quote(activity_first_at), quote(activity_last_at), quote(model), quote(effort), quote(content), quote(tool_correlation_key),
    quote(tool_correlation_kind), quote(tool_native_id), quote(tool_namespace), quote(tool_provider), quote(tool_name), quote(tool_kind),
    quote(tool_status), quote(tool_status_basis), quote(tool_error_kind), quote(tool_error_summary), quote(tool_diagnostic),
    quote(tool_started_at), quote(tool_completed_at), quote(tool_duration_ms), quote(parent_tool_native_id), quote(in_tokens),
    quote(out_tokens), quote(cache_r_tokens), quote(cache_w_tokens), quote(think_tokens), quote(cache_w1h_tokens), quote(web_searches),
    quote(service_tier), quote(route), quote(billed_pusd), quote(usage_scope), quote(requests), quote(agent_version), quote(src_offset))
    FROM message
  UNION ALL
  SELECT 4, 'message_detail', concat_ws(char(31), quote(timeline_id), quote(detail)) FROM message
  UNION ALL
  SELECT 5, 'message_tool_detail', concat_ws(char(31), quote(timeline_id), quote(tool_detail)) FROM message
  UNION ALL
  SELECT 6, 'session_usage', concat_ws(char(31), quote(session_id), quote(sample_key), quote(ts), quote(model), quote(scope), quote(requests),
    quote(in_tokens), quote(out_tokens), quote(cache_r_tokens), quote(cache_w_tokens), quote(think_tokens), quote(cache_w1h_tokens),
    quote(web_searches), quote(service_tier), quote(route), quote(billed_pusd)) FROM session_usage
  UNION ALL
  SELECT 7, 'session_usage_total', concat_ws(char(31), quote(session_id), quote(model), quote(ts), quote(in_tokens), quote(output_total),
    quote(think_tokens), quote(cache_r_tokens), quote(cache_write_total), quote(web_searches), quote(estimated_pusd)) FROM session_usage_total
  UNION ALL
  SELECT 8, 'usage_bucket', concat_ws(char(31), quote(session_id), quote(hour_ts), quote(model), quote(source), quote(in_tokens),
    quote(out_tokens), quote(cache_r_tokens), quote(cache_w_tokens), quote(think_tokens), quote(cache_w1h_tokens), quote(web_searches),
    quote(cost_pusd), quote(billed_pusd), quote(unpriced_tokens), quote(unrecorded_output), quote(message_count), quote(tool_call_count))
    FROM usage_bucket
  UNION ALL
  SELECT 9, 'file_edit', concat_ws(char(31), quote(f.session_id), quote(m.timeline_id), quote(f.message_id IS NULL), quote(f.path),
    quote(f.kind), quote(f.snapshot_ref), quote(f.lines_added), quote(f.lines_removed), quote(f.at))
    FROM file_edit f LEFT JOIN message m ON m.id = f.message_id
  UNION ALL
  SELECT 10, 'git_commit', concat_ws(char(31), quote(commit_id), quote(session_id), quote(branch), quote(at)) FROM git_commit
  UNION ALL
  SELECT 11, 'plan', concat_ws(char(31), quote(session_id), quote(revision), quote(content), quote(updated_at)) FROM plan
  UNION ALL
  SELECT 12, 'task', concat_ws(char(31), quote(id), quote(session_id), quote(status), quote(content), quote(created_at), quote(updated_at),
    quote(started_at), quote(terminal_at)) FROM task
  UNION ALL
  SELECT 13, 'workflow_journal', concat_ws(char(31), quote(observation_id), quote(run_id), quote(session_id), quote(detail)) FROM workflow_journal
  UNION ALL
  SELECT 14, 'workflow_run', concat_ws(char(31), quote(session_id), quote(run_id), quote(detail), quote(updated_at)) FROM workflow_run
  UNION ALL
  SELECT 15, 'html_artifact', concat_ws(char(31), quote(id), quote(artifact_id), quote(session_id), quote(timeline_id), quote(title),
    quote(filename), quote(path), quote(url), quote(native_id), quote(version), quote(published_at), quote(sort_at), quote(source_offset),
    quote(availability), quote(recovered_from), quote(asset)) FROM html_artifact
  UNION ALL
  SELECT 16, 'drift_agg', concat_ws(char(31), quote(d.runtime_id), quote(d.agent_version), quote(d.stream), quote(d.record_kind),
    quote(d.error_code), quote(d.shape_hash), quote(d.count), quote(d.first_seen), quote(d.last_seen), quote(c.p), quote(d.sample_offset))
    FROM drift_agg d LEFT JOIN cur c ON c.id = d.sample_cursor
  UNION ALL
  SELECT 17, 'drift_seen', quote(event_key) FROM drift_seen
),
components(ord, component, n, d) AS (
  SELECT ord, component, count(r), lower(hex(sha3_agg(r ORDER BY r, 256))) FROM rows GROUP BY ord, component
)
SELECT sec, component, n, d FROM (
  SELECT 'logical' AS sec, component, n, d, ord FROM components
  UNION ALL
  SELECT 'logical', 'TOTAL', (SELECT sum(n) FROM components),
    lower(hex(sha3_agg(concat_ws(char(31), component, n, d) ORDER BY ord, 256))), 99
    FROM components
) ORDER BY ord;

-- ------------------------------------------------------------------ strict
-- Cursor-id columns (path rank in every binary). Each component keys the id
-- by its row's natural key.
WITH
src(component, r) AS (
  SELECT 'ingest_cursor', concat_ws(char(31), quote(id), quote(runtime_id), quote(stream), quote(path), quote(session_id),
    quote(active_generation), quote(highest_generation), quote(checkpoint)) FROM ingest_cursor
  UNION ALL SELECT 'message_src', concat_ws(char(31), quote(timeline_id), quote(src_cursor)) FROM message
  UNION ALL SELECT 'session_title_src', concat_ws(char(31), quote(id), quote(title_src_cursor)) FROM session
  UNION ALL SELECT 'plan_src', concat_ws(char(31), quote(session_id), quote(revision), quote(src_cursor)) FROM plan
  UNION ALL SELECT 'task_src', concat_ws(char(31), quote(id), quote(src_cursor)) FROM task
  UNION ALL SELECT 'html_artifact_src', concat_ws(char(31), quote(id), quote(src_cursor)) FROM html_artifact
  UNION ALL SELECT 'workflow_run_src', concat_ws(char(31), quote(session_id), quote(run_id), quote(src_cursor)) FROM workflow_run
  UNION ALL SELECT 'workflow_journal_src', concat_ws(char(31), quote(observation_id), quote(src_cursor)) FROM workflow_journal
  UNION ALL SELECT 'file_edit_src', concat_ws(char(31), quote(f.session_id), quote(m.timeline_id), quote(f.path), quote(f.kind),
    quote(f.snapshot_ref), quote(f.at), quote(f.src_cursor)) FROM file_edit f LEFT JOIN message m ON m.id = f.message_id
  UNION ALL SELECT 'git_commit_src', concat_ws(char(31), quote(commit_id), quote(session_id), quote(src_cursor)) FROM git_commit
  UNION ALL SELECT 'session_usage_src', concat_ws(char(31), quote(session_id), quote(sample_key), quote(src_cursor)) FROM session_usage
  UNION ALL SELECT 'session_usage_total_src', concat_ws(char(31), quote(session_id), quote(model), quote(src_cursor)) FROM session_usage_total
  UNION ALL SELECT 'drift_sample', concat_ws(char(31), quote(runtime_id), quote(agent_version), quote(stream), quote(record_kind),
    quote(error_code), quote(shape_hash), quote(sample_cursor), quote(sample_offset)) FROM drift_agg
),
components(component, n, d) AS (
  SELECT component, count(r), lower(hex(sha3_agg(r ORDER BY r, 256))) FROM src GROUP BY component
)
SELECT 'strict', component, n, d FROM (
  SELECT component, n, d, 0 AS o FROM components
  UNION ALL
  SELECT 'TOTAL', (SELECT sum(n) FROM components),
    lower(hex(sha3_agg(concat_ws(char(31), component, n, d) ORDER BY component, 256))), 1 FROM components
) ORDER BY o, component;

-- ------------------------------------------------------------------ strict_noid
-- The same provenance with each cursor id replaced by (runtime_id, path).
WITH
cur(id, p) AS (SELECT id, concat_ws(char(30), runtime_id, path) FROM ingest_cursor),
src(component, r) AS (
  SELECT 'ingest_cursor', concat_ws(char(31), quote(runtime_id), quote(stream), quote(path), quote(session_id),
    quote(active_generation), quote(highest_generation),
    quote(json_remove(checkpoint, '$.tail', '$.snapshot_stamp', '$.snapshot_revision'))) FROM ingest_cursor
  UNION ALL SELECT 'ingest_cursor_file_evidence', concat_ws(char(31), quote(runtime_id), quote(path),
    quote(json_extract(checkpoint, '$.tail')), quote(json_extract(checkpoint, '$.snapshot_stamp')),
    quote(json_extract(checkpoint, '$.snapshot_revision'))) FROM ingest_cursor
  UNION ALL SELECT 'message_src', concat_ws(char(31), quote(t.timeline_id), quote(c.p)) FROM message t LEFT JOIN cur c ON c.id = t.src_cursor
  UNION ALL SELECT 'session_title_src', concat_ws(char(31), quote(t.id), quote(c.p)) FROM session t LEFT JOIN cur c ON c.id = t.title_src_cursor
  UNION ALL SELECT 'plan_src', concat_ws(char(31), quote(t.session_id), quote(t.revision), quote(c.p)) FROM plan t LEFT JOIN cur c ON c.id = t.src_cursor
  UNION ALL SELECT 'task_src', concat_ws(char(31), quote(t.id), quote(c.p)) FROM task t LEFT JOIN cur c ON c.id = t.src_cursor
  UNION ALL SELECT 'html_artifact_src', concat_ws(char(31), quote(t.id), quote(c.p)) FROM html_artifact t LEFT JOIN cur c ON c.id = t.src_cursor
  UNION ALL SELECT 'workflow_run_src', concat_ws(char(31), quote(t.session_id), quote(t.run_id), quote(c.p))
    FROM workflow_run t LEFT JOIN cur c ON c.id = t.src_cursor
  UNION ALL SELECT 'workflow_journal_src', concat_ws(char(31), quote(t.observation_id), quote(c.p))
    FROM workflow_journal t LEFT JOIN cur c ON c.id = t.src_cursor
  UNION ALL SELECT 'file_edit_src', concat_ws(char(31), quote(f.session_id), quote(m.timeline_id), quote(f.path), quote(f.kind),
    quote(f.snapshot_ref), quote(f.at), quote(c.p)) FROM file_edit f LEFT JOIN message m ON m.id = f.message_id LEFT JOIN cur c ON c.id = f.src_cursor
  UNION ALL SELECT 'git_commit_src', concat_ws(char(31), quote(t.commit_id), quote(t.session_id), quote(c.p))
    FROM git_commit t LEFT JOIN cur c ON c.id = t.src_cursor
  UNION ALL SELECT 'session_usage_src', concat_ws(char(31), quote(t.session_id), quote(t.sample_key), quote(c.p))
    FROM session_usage t LEFT JOIN cur c ON c.id = t.src_cursor
  UNION ALL SELECT 'session_usage_total_src', concat_ws(char(31), quote(t.session_id), quote(t.model), quote(c.p))
    FROM session_usage_total t LEFT JOIN cur c ON c.id = t.src_cursor
  UNION ALL SELECT 'drift_sample', concat_ws(char(31), quote(t.runtime_id), quote(t.agent_version), quote(t.stream), quote(t.record_kind),
    quote(t.error_code), quote(t.shape_hash), quote(c.p), quote(t.sample_offset)) FROM drift_agg t LEFT JOIN cur c ON c.id = t.sample_cursor
),
components(component, n, d) AS (
  SELECT component, count(r), lower(hex(sha3_agg(r ORDER BY r, 256))) FROM src GROUP BY component
)
SELECT 'strict_noid', component, n, d FROM (
  SELECT component, n, d, 0 AS o FROM components
  UNION ALL
  SELECT 'TOTAL', (SELECT sum(n) FROM components),
    lower(hex(sha3_agg(concat_ws(char(31), component, n, d) ORDER BY component, 256))), 1 FROM components
  UNION ALL
  SELECT 'TOTAL_R', (SELECT sum(n) FROM components WHERE component <> 'ingest_cursor_file_evidence'),
    lower(hex(sha3_agg(concat_ws(char(31), component, n, d) ORDER BY component, 256))), 2 FROM components
    WHERE component <> 'ingest_cursor_file_evidence'
) ORDER BY o, component;

-- ------------------------------------------------------------------ surrogate (reported, U-2)
WITH s(component, r) AS (
  SELECT 'message_id', concat_ws(char(31), quote(timeline_id), quote(id)) FROM message
  UNION ALL
  SELECT 'file_edit_message_id', concat_ws(char(31), quote(f.session_id), quote(m.timeline_id), quote(f.path), quote(f.kind),
    quote(f.snapshot_ref), quote(f.at), quote(f.message_id)) FROM file_edit f LEFT JOIN message m ON m.id = f.message_id
)
SELECT 'surrogate', component, count(r), lower(hex(sha3_agg(r ORDER BY r, 256))) FROM s GROUP BY component ORDER BY component;

-- ------------------------------------------------------------------ aux (reported)
WITH a(r) AS (SELECT concat_ws(char(31), quote(id), quote(kind), quote(data_root), quote(enabled), quote(account), quote(config),
  quote(skills), quote(versions_seen)) FROM agent_runtime)
SELECT 'aux', 'agent_runtime', count(r), lower(hex(sha3_agg(r ORDER BY r, 256))) FROM a;

-- ------------------------------------------------------------------ counts
SELECT 'count', 'sessions', count(*) FROM session;
SELECT 'count', 'projects', count(*) FROM project;
SELECT 'count', 'messages', count(*) FROM message;
SELECT 'count', 'tool_rows', count(*) FROM message WHERE tool_name IS NOT NULL;
SELECT 'count', 'usage_bearing_messages', count(*) FROM message WHERE in_tokens IS NOT NULL OR out_tokens IS NOT NULL;
SELECT 'count', 'session_usage_rows', count(*) FROM session_usage;
SELECT 'count', 'session_usage_total_rows', count(*) FROM session_usage_total;
SELECT 'count', 'usage_bucket_rows', count(*) FROM usage_bucket;
SELECT 'count', 'file_edit_rows', count(*) FROM file_edit;
SELECT 'count', 'git_commit_rows', count(*) FROM git_commit;
SELECT 'count', 'plan_rows', count(*) FROM plan;
SELECT 'count', 'task_rows', count(*) FROM task;
SELECT 'count', 'workflow_run_rows', count(*) FROM workflow_run;
SELECT 'count', 'workflow_journal_rows', count(*) FROM workflow_journal;
SELECT 'count', 'html_artifact_rows', count(*) FROM html_artifact;
SELECT 'count', 'drift_agg_rows', count(*) FROM drift_agg;
SELECT 'count', 'drift_seen_rows', count(*) FROM drift_seen;
SELECT 'count', 'ingest_cursor_rows', count(*) FROM ingest_cursor;
SELECT 'count', 'ingest_cursor_active', count(*) FROM ingest_cursor WHERE active_generation > 0;
SELECT 'count', 'ingest_cursor_sqlite_members_active', count(*) FROM ingest_cursor WHERE path LIKE '%.sqlite#%' AND active_generation > 0;
SELECT 'count', 'agent_runtime_rows', count(*) FROM agent_runtime;
SELECT 'count', 'message_type:' || type, count(*) FROM message GROUP BY type ORDER BY type;
SELECT 'count', 'session_stream_state:' || stream_state, count(*) FROM session GROUP BY stream_state ORDER BY stream_state;
SELECT 'count', 'session_kind:' || r.kind, count(*) FROM session s JOIN agent_runtime r ON r.id = s.runtime_id GROUP BY r.kind ORDER BY r.kind;
SELECT 'count', 'rows_with_asset_refs:message.detail', count(*) FROM message WHERE instr(detail, '/api/assets/') > 0;
SELECT 'count', 'rows_with_asset_refs:message.tool_detail', count(*) FROM message WHERE instr(tool_detail, '/api/assets/') > 0;
SELECT 'count', 'rows_with_asset_refs:html_artifact.asset', count(*) FROM html_artifact WHERE asset IS NOT NULL;
