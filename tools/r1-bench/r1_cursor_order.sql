-- R1 cursor-order diagnostic (Task C; reported, never gated). Read-only, like r1_logical_digest.sql:
--   /usr/bin/sqlite3 -readonly "file:<DB>?mode=ro&immutable=1" < r1_cursor_order.sql
-- Output lines:
--   cursor_order|<agent kind>|<cursors>|<min id>|<max id>|<cursors whose id rank equals their path rank in the runtime>
--   strict_local|<component>|<rows>|<sha3-256>
-- strict_local is r1_logical_digest.sql's strict section with every cursor id replaced by
-- (agent kind, id - min id of its runtime): it separates "ids are path rank within each runtime"
-- (brief D5/I6) from the order in which the writer first inserts each runtime's cursors, which
-- offsets every id of the later runtime. No paths or content are printed.
PRAGMA query_only = 1;
.bail on
.headers off
.mode list
.separator |

WITH r AS (
  SELECT c.id, a.kind,
    row_number() OVER (PARTITION BY c.runtime_id ORDER BY c.path) AS pr,
    row_number() OVER (PARTITION BY c.runtime_id ORDER BY c.id) AS ir
  FROM ingest_cursor c JOIN agent_runtime a ON a.id = c.runtime_id
)
SELECT 'cursor_order', kind, count(*), min(id), max(id), sum(pr = ir) FROM r GROUP BY kind ORDER BY kind;

WITH
base(runtime_id, b) AS (SELECT runtime_id, min(id) FROM ingest_cursor GROUP BY runtime_id),
loc(id, l) AS (SELECT c.id, a.kind || ':' || (c.id - base.b) FROM ingest_cursor c
  JOIN base ON base.runtime_id = c.runtime_id JOIN agent_runtime a ON a.id = c.runtime_id),
src(component, r) AS (
  SELECT 'ingest_cursor', concat_ws(char(31), quote(l.l), quote(c.runtime_id), quote(c.stream), quote(c.path), quote(c.session_id),
    quote(c.active_generation), quote(c.highest_generation), quote(c.checkpoint)) FROM ingest_cursor c JOIN loc l ON l.id = c.id
  UNION ALL SELECT 'message_src', concat_ws(char(31), quote(m.timeline_id), quote(l.l)) FROM message m LEFT JOIN loc l ON l.id = m.src_cursor
  UNION ALL SELECT 'session_title_src', concat_ws(char(31), quote(s.id), quote(l.l)) FROM session s LEFT JOIN loc l ON l.id = s.title_src_cursor
  UNION ALL SELECT 'plan_src', concat_ws(char(31), quote(p.session_id), quote(p.revision), quote(l.l)) FROM plan p LEFT JOIN loc l ON l.id = p.src_cursor
  UNION ALL SELECT 'task_src', concat_ws(char(31), quote(t.id), quote(l.l)) FROM task t LEFT JOIN loc l ON l.id = t.src_cursor
  UNION ALL SELECT 'html_artifact_src', concat_ws(char(31), quote(h.id), quote(l.l)) FROM html_artifact h LEFT JOIN loc l ON l.id = h.src_cursor
  UNION ALL SELECT 'workflow_run_src', concat_ws(char(31), quote(w.session_id), quote(w.run_id), quote(l.l)) FROM workflow_run w
    LEFT JOIN loc l ON l.id = w.src_cursor
  UNION ALL SELECT 'workflow_journal_src', concat_ws(char(31), quote(j.observation_id), quote(l.l)) FROM workflow_journal j
    LEFT JOIN loc l ON l.id = j.src_cursor
  UNION ALL SELECT 'file_edit_src', concat_ws(char(31), quote(f.session_id), quote(m.timeline_id), quote(f.path), quote(f.kind),
    quote(f.snapshot_ref), quote(f.at), quote(l.l)) FROM file_edit f LEFT JOIN message m ON m.id = f.message_id LEFT JOIN loc l ON l.id = f.src_cursor
  UNION ALL SELECT 'git_commit_src', concat_ws(char(31), quote(g.commit_id), quote(g.session_id), quote(l.l)) FROM git_commit g
    LEFT JOIN loc l ON l.id = g.src_cursor
  UNION ALL SELECT 'session_usage_src', concat_ws(char(31), quote(u.session_id), quote(u.sample_key), quote(l.l)) FROM session_usage u
    LEFT JOIN loc l ON l.id = u.src_cursor
  UNION ALL SELECT 'session_usage_total_src', concat_ws(char(31), quote(u.session_id), quote(u.model), quote(l.l)) FROM session_usage_total u
    LEFT JOIN loc l ON l.id = u.src_cursor
  UNION ALL SELECT 'drift_sample', concat_ws(char(31), quote(d.runtime_id), quote(d.agent_version), quote(d.stream), quote(d.record_kind),
    quote(d.error_code), quote(d.shape_hash), quote(l.l), quote(d.sample_offset)) FROM drift_agg d LEFT JOIN loc l ON l.id = d.sample_cursor
),
components(component, n, d) AS (
  SELECT component, count(r), lower(hex(sha3_agg(r ORDER BY r, 256))) FROM src GROUP BY component
)
SELECT 'strict_local', component, n, d FROM (
  SELECT component, n, d, 0 AS o FROM components
  UNION ALL
  SELECT 'TOTAL', (SELECT sum(n) FROM components),
    lower(hex(sha3_agg(concat_ws(char(31), component, n, d) ORDER BY component, 256))), 1 FROM components
) ORDER BY o, component;
