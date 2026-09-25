//! SQLite snapshot extraction plans for the two store generations OpenCode
//! writes into one `opencode.db`.
//!
//! 1.x keeps `session`/`message`/`part` (+ the `event` log). 2.x (2.0.0,
//! 2026-09-11) keeps `session_v2`/`session_message` in the same file
//! (`packages/core/src/session/sql.ts:22-98`), leaves the 1.x tables in place
//! beside them, imports each 1.x session once under its own id
//! (`database/v1-migration.bun.ts:592-642`), and, on a pre-split 2.x lineage,
//! renames `session` to `session_v2` instead
//! (`migration/20260804233008_loose_psylocke.ts`). A plan is therefore chosen
//! by table presence, never by version: every statement it declares must
//! prepare against the database it is run on.

use std::collections::BTreeSet;

use rosetta_adapter_support::{SnapshotPlan, SnapshotQuery};

use crate::spec;

/// The plan for a database whose table inventory is known.
///
/// * 1.x tables only: the 1.x plan.
/// * 2.x tables beside 1.x tables: sessions from `session_v2` plus the 1.x
///   sessions not yet imported; each session's 1.x rows are its faithful
///   prefix (the import copies a message's last step only), and its
///   `session_message` rows are read minus the ones mirroring a 1.x message
///   id, so nothing counts twice and post-upgrade activity still lands.
/// * 2.x tables only: the 2.x plan.
pub fn plan_for_tables(stream_id: &str, tables: &BTreeSet<String>) -> Option<SnapshotPlan> {
    if stream_id != spec::DATABASE {
        return None;
    }
    let has = |name: &str| tables.contains(name);
    let v2 = has("session_v2") && has("session_message");
    let v1 = has("session") && has("message") && has("part");
    if !v2 {
        return Some(v1_plan());
    }
    let mut record_queries = Vec::new();
    if v1 {
        record_queries.push(SnapshotQuery {
            name: spec::TIMELINE_QUERY.to_owned(),
            sql: TIMELINE_SQL.to_owned(),
        });
    }
    record_queries.push(SnapshotQuery {
        name: spec::TIMELINE_V2_QUERY.to_owned(),
        sql: timeline_v2_sql(v1),
    });
    if has("event") {
        record_queries.push(SnapshotQuery {
            name: spec::USAGE_V2_QUERY.to_owned(),
            sql: USAGE_V2_SQL.to_owned(),
        });
    }
    Some(SnapshotPlan {
        sessions_sql: sessions_v2_sql(v1),
        record_queries,
    })
}

/// The 1.x plan: what a database without `session_v2` gets, and what the
/// `AgentAdapter::snapshot_plan` contract (no table inventory) returns.
pub fn v1_plan() -> SnapshotPlan {
    SnapshotPlan {
        sessions_sql: format!(
            "WITH RECURSIVE {}\n{}\nORDER BY native_session_id",
            lineage_cte("lineage", "session"),
            sessions_select("session", "lineage", "")
        ),
        record_queries: vec![SnapshotQuery {
            name: spec::TIMELINE_QUERY.to_owned(),
            sql: TIMELINE_SQL.to_owned(),
        }],
    }
}

/// Enumerate 2.x sessions and, when the 1.x tables are still present, the
/// 1.x sessions the one-time import has not copied yet.
fn sessions_v2_sql(with_legacy: bool) -> String {
    if !with_legacy {
        return format!(
            "WITH RECURSIVE {}\n{}\nORDER BY native_session_id",
            lineage_cte("lineage", "session_v2"),
            sessions_select("session_v2", "lineage", "")
        );
    }
    format!(
        "WITH RECURSIVE {},\n{}\n{}\nUNION ALL\n{}\nORDER BY native_session_id",
        lineage_cte("lineage_v2", "session_v2"),
        lineage_cte("lineage_v1", "session"),
        sessions_select("session_v2", "lineage_v2", ""),
        sessions_select(
            "session",
            "lineage_v1",
            "\nWHERE NOT EXISTS (SELECT 1 FROM session_v2 WHERE session_v2.id = session.id)"
        ),
    )
}

/// Resolve the root of each parent chain. A missing ancestor keeps the named
/// parent as root; the pipeline materializes a placeholder for that retained
/// relationship.
fn lineage_cte(name: &str, table: &str) -> String {
    format!(
        "{name}(native_session_id, ancestor_id, parent_id, depth) AS (
    SELECT id, id, parent_id, 0 FROM {table}
    UNION ALL
    SELECT {name}.native_session_id, ancestor.id, ancestor.parent_id, {name}.depth + 1
    FROM {name}
    JOIN {table} AS ancestor ON ancestor.id = {name}.parent_id
    WHERE {name}.depth < 100
)"
    )
}

/// One session row in the standardized aliases. A 2.x fork
/// (`fork_session_id`) is a sibling top-level session, never a child.
fn sessions_select(table: &str, lineage: &str, filter: &str) -> String {
    format!(
        "SELECT
    session.id AS native_session_id,
    session.title AS title,
    session.directory AS cwd,
    session.version AS agent_version,
    session.time_created AS started_at,
    session.time_updated AS updated_at,
    session.parent_id AS parent_native_session_id,
    CASE
        WHEN session.parent_id IS NULL THEN session.id
        ELSE COALESCE(
            (SELECT ancestor_id
             FROM {lineage}
             WHERE native_session_id = session.id AND parent_id IS NULL
             ORDER BY depth DESC
             LIMIT 1),
            session.parent_id
        )
    END AS root_native_session_id,
    CASE WHEN session.parent_id IS NULL THEN 'main' ELSE 'subagent' END AS role
FROM {table} AS session{filter}"
    )
}

/// 1.x rows. Final message/part state is conversational truth. The
/// append-only event table contributes only first-seen presentation order:
/// repeated updates to a text or tool part never move the canonical row. The
/// session's creation time decides fork copies (`usage.rs`).
const TIMELINE_SQL: &str = r#"
WITH first_part_event AS (
    SELECT json_extract(data, '$.part.id') AS part_id, MIN(seq) AS first_event_seq
    FROM event
    WHERE aggregate_id = ?1 AND type = 'message.part.updated.1'
    GROUP BY json_extract(data, '$.part.id')
)
SELECT
    message.id AS message_id,
    message.data AS message_data,
    part.id AS part_id,
    part.time_created AS part_time_created,
    part.data AS part_data,
    first_part_event.first_event_seq AS first_event_seq,
    session.time_created AS session_time_created
FROM message
JOIN part ON part.message_id = message.id AND part.session_id = message.session_id
LEFT JOIN session ON session.id = message.session_id
LEFT JOIN first_part_event ON first_part_event.part_id = part.id
WHERE message.session_id = ?1
ORDER BY
    COALESCE(first_part_event.first_event_seq, 9223372036854775807),
    message.time_created,
    message.id,
    part.time_created,
    part.id
"#;

/// 2.x rows: one `session_message` per record in `seq` order (the projector's
/// own order, `session/projector.ts`). With the 1.x tables present, a row
/// whose id mirrors a 1.x message of the same session is the import's copy
/// of what the 1.x query already read.
fn timeline_v2_sql(with_legacy: bool) -> String {
    let legacy_filter = if with_legacy {
        "\n  AND NOT EXISTS (
        SELECT 1 FROM message AS legacy
        WHERE legacy.id = sm.id AND legacy.session_id = sm.session_id
      )"
    } else {
        ""
    };
    format!(
        "SELECT
    sm.id AS message_id,
    sm.type AS message_type,
    sm.seq AS seq,
    sm.time_created AS message_time_created,
    sm.data AS message_data,
    session.time_created AS session_time_created,
    session.fork_session_id AS fork_session_id,
    session.directory AS session_directory
FROM session_message AS sm
JOIN session_v2 AS session ON session.id = sm.session_id
WHERE sm.session_id = ?1{legacy_filter}
ORDER BY sm.seq, sm.id"
    )
}

/// 2.x durable `session.usage.recorded.*` events: requests recorded outside
/// any message (`session-event.ts` UsageRecorded, sources `title` and
/// `compaction`). `event.created` exists from the 2.x split migration on.
const USAGE_V2_SQL: &str = r#"
SELECT
    event.id AS event_id,
    event.seq AS seq,
    event.created AS created,
    event.type AS event_type,
    event.data AS data
FROM event
WHERE event.aggregate_id = ?1 AND event.type LIKE 'session.usage.recorded.%'
ORDER BY event.seq, event.id
"#;

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use rusqlite::Connection;

    use super::{plan_for_tables, v1_plan};
    use crate::spec;

    /// The 1.x tables the plans touch, as the 1.18.22 fixture schema dump
    /// declares them (`sources/1.18.22.sqlite-schema.sql`; columns the plans
    /// never select are omitted).
    pub(crate) const V1_SCHEMA: &str = "
        CREATE TABLE session (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT,
            slug TEXT NOT NULL, directory TEXT NOT NULL, title TEXT NOT NULL,
            version TEXT NOT NULL, time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL
        );
        CREATE TABLE message (
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
            data TEXT NOT NULL
        );
        CREATE TABLE part (
            id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
            data TEXT NOT NULL
        );";

    /// The 1.x event log (`event` without `created`).
    pub(crate) const V1_EVENT_SCHEMA: &str = "
        CREATE TABLE event (
            id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL,
            type TEXT NOT NULL, data TEXT NOT NULL
        );";

    /// The 2.x tables (`session/sql.ts:22-98`, `event/sql.ts:10-26`; the
    /// split migration adds `event.created`).
    pub(crate) const V2_SCHEMA: &str = "
        CREATE TABLE session_v2 (
            id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT,
            fork_session_id TEXT, fork_boundary TEXT, slug TEXT NOT NULL,
            directory TEXT NOT NULL, title TEXT, version TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
        );
        CREATE TABLE session_message (
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL,
            seq INTEGER NOT NULL, time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL, data TEXT NOT NULL
        );
        CREATE TABLE event (
            id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL,
            created INTEGER NOT NULL DEFAULT 0, type TEXT NOT NULL, data TEXT NOT NULL
        );";

    pub(crate) fn tables(connection: &Connection) -> std::collections::BTreeSet<String> {
        let mut statement = connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    pub(crate) fn rows(
        connection: &Connection,
        sql: &str,
        bind: Option<&str>,
    ) -> Vec<serde_json::Value> {
        let mut statement = connection.prepare(sql).unwrap();
        let names: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut rows = match bind {
            Some(value) => statement.query([value]),
            None => statement.query([]),
        }
        .unwrap();
        let mut out = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let mut object = serde_json::Map::new();
            for (index, name) in names.iter().enumerate() {
                let value = match row.get_ref(index).unwrap() {
                    rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                    rusqlite::types::ValueRef::Integer(value) => value.into(),
                    rusqlite::types::ValueRef::Real(value) => value.into(),
                    rusqlite::types::ValueRef::Text(text) => {
                        String::from_utf8_lossy(text).into_owned().into()
                    }
                    rusqlite::types::ValueRef::Blob(_) => panic!("blob"),
                };
                object.insert(name.clone(), value);
            }
            out.push(serde_json::Value::Object(object));
        }
        out
    }

    fn sessions(connection: &Connection, sql: &str) -> BTreeMap<String, serde_json::Value> {
        rows(connection, sql, None)
            .into_iter()
            .map(|row| (row["native_session_id"].as_str().unwrap().to_owned(), row))
            .collect()
    }

    #[test]
    fn a_1x_database_gets_the_1x_plan_with_the_session_creation_time() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(V1_SCHEMA).unwrap();
        connection.execute_batch(V1_EVENT_SCHEMA).unwrap();
        connection
            .execute_batch(
                "INSERT INTO session VALUES ('root', 'p', NULL, 's', '/w', 'Root', '1.18.22', 1, 4);
                 INSERT INTO session VALUES ('child', 'p', 'root', 's', '/w', 'Child', '1.18.22', 2, 4);
                 INSERT INTO session VALUES ('grand', 'p', 'child', 's', '/w', 'Grand', '1.18.22', 3, 4);
                 INSERT INTO message VALUES ('m1', 'root', 10, 11, '{}');
                 INSERT INTO part VALUES ('p1', 'm1', 'root', 12, 12, '{}');",
            )
            .unwrap();
        let plan = plan_for_tables(spec::DATABASE, &tables(&connection)).unwrap();
        assert_eq!(plan.sessions_sql, v1_plan().sessions_sql);
        assert_eq!(plan.record_queries.len(), 1);
        assert_eq!(plan.record_queries[0].name, spec::TIMELINE_QUERY);
        let sessions = sessions(&connection, &plan.sessions_sql);
        assert_eq!(sessions["root"]["root_native_session_id"], "root");
        assert_eq!(sessions["root"]["role"], "main");
        assert_eq!(sessions["child"]["parent_native_session_id"], "root");
        assert_eq!(sessions["grand"]["root_native_session_id"], "root");
        assert_eq!(sessions["grand"]["role"], "subagent");
        let timeline = rows(&connection, &plan.record_queries[0].sql, Some("root"));
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0]["session_time_created"], 1);
        assert_eq!(timeline[0]["part_id"], "p1");
        assert!(timeline[0]["first_event_seq"].is_null());
        assert!(plan_for_tables("log", &tables(&connection)).is_none());
    }

    #[test]
    fn a_2x_database_beside_1x_tables_reads_each_session_once() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(V1_SCHEMA).unwrap();
        connection.execute_batch(V2_SCHEMA).unwrap();
        connection
            .execute_batch(
                "-- Imported: in both tables, its 1.x rows and one post-upgrade 2.x row.
                 INSERT INTO session VALUES ('imported', 'p', NULL, 's', '/w', 'Old', '1.18.23', 1, 4);
                 INSERT INTO session_v2 VALUES ('imported', 'p', NULL, NULL, NULL, 's', '/w', 'Old', '1.18.23', 1, 9);
                 INSERT INTO message VALUES ('m1', 'imported', 10, 11, '{}');
                 INSERT INTO part VALUES ('p1', 'm1', 'imported', 12, 12, '{}');
                 INSERT INTO session_message VALUES ('m1', 'imported', 'assistant', 0, 10, 11, '{}');
                 INSERT INTO session_message VALUES ('m2', 'imported', 'user', 1, 20, 20, '{}');
                 -- Not yet imported: 1.x only.
                 INSERT INTO session VALUES ('legacy', 'p', NULL, 's', '/w', 'Legacy', '1.18.23', 2, 4);
                 INSERT INTO session VALUES ('legacy-child', 'p', 'legacy', 's', '/w', 'Sub', '1.18.23', 3, 4);
                 -- Native 2.x sessions: a subagent and a fork.
                 INSERT INTO session_v2 VALUES ('fresh', 'p', NULL, NULL, NULL, 's', '/w', NULL, '2.0.14', 5, 9);
                 INSERT INTO session_v2 VALUES ('fresh-child', 'p', 'fresh', NULL, NULL, 's', '/w', 'Sub', '2.0.14', 6, 9);
                 INSERT INTO session_v2 VALUES ('fork', 'p', NULL, 'fresh', '{\"type\":\"before\",\"messageID\":\"m9\"}', 's', '/w', 'F', '2.0.14', 8, 9);
                 INSERT INTO session_message VALUES ('m8', 'fresh', 'assistant', 0, 7, 7, '{}');
                 INSERT INTO event VALUES ('e1', 'fresh', 1, 7, 'session.usage.recorded.1', '{}');
                 INSERT INTO event VALUES ('e2', 'fresh', 2, 7, 'session.step.ended.1', '{}');",
            )
            .unwrap();
        let plan = plan_for_tables(spec::DATABASE, &tables(&connection)).unwrap();
        let names: Vec<_> = plan
            .record_queries
            .iter()
            .map(|query| query.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                spec::TIMELINE_QUERY,
                spec::TIMELINE_V2_QUERY,
                spec::USAGE_V2_QUERY
            ]
        );
        let sessions = sessions(&connection, &plan.sessions_sql);
        assert_eq!(
            sessions.keys().collect::<Vec<_>>(),
            [
                "fork",
                "fresh",
                "fresh-child",
                "imported",
                "legacy",
                "legacy-child"
            ]
        );
        assert_eq!(sessions["imported"]["updated_at"], 9, "the 2.x row wins");
        assert!(sessions["fresh"]["title"].is_null());
        assert_eq!(sessions["fork"]["role"], "main");
        assert!(sessions["fork"]["parent_native_session_id"].is_null());
        assert_eq!(sessions["fresh-child"]["root_native_session_id"], "fresh");
        assert_eq!(sessions["legacy-child"]["root_native_session_id"], "legacy");

        let by_name: BTreeMap<_, _> = plan
            .record_queries
            .iter()
            .map(|query| (query.name.as_str(), query.sql.as_str()))
            .collect();
        let legacy_rows = rows(&connection, by_name[spec::TIMELINE_QUERY], Some("imported"));
        assert_eq!(legacy_rows.len(), 1, "the 1.x prefix");
        let v2_rows = rows(
            &connection,
            by_name[spec::TIMELINE_V2_QUERY],
            Some("imported"),
        );
        assert_eq!(v2_rows.len(), 1, "only the post-upgrade row");
        assert_eq!(v2_rows[0]["message_id"], "m2");
        assert_eq!(v2_rows[0]["session_time_created"], 1);
        assert!(v2_rows[0]["fork_session_id"].is_null());
        assert_eq!(v2_rows[0]["session_directory"], "/w");
        assert!(rows(&connection, by_name[spec::TIMELINE_QUERY], Some("fresh")).is_empty());
        assert_eq!(
            rows(&connection, by_name[spec::TIMELINE_V2_QUERY], Some("fresh")).len(),
            1
        );
        let usage = rows(&connection, by_name[spec::USAGE_V2_QUERY], Some("fresh"));
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0]["event_id"], "e1");
        assert_eq!(usage[0]["created"], 7);
    }

    #[test]
    fn a_renamed_2x_lineage_gets_the_2x_plan_only() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(V2_SCHEMA).unwrap();
        connection
            .execute_batch(
                "INSERT INTO session_v2 VALUES ('s', 'p', NULL, NULL, NULL, 's', '/w', 'T', '2.0.14', 5, 9);
                 INSERT INTO session_message VALUES ('m1', 's', 'user', 0, 6, 6, '{}');",
            )
            .unwrap();
        let plan = plan_for_tables(spec::DATABASE, &tables(&connection)).unwrap();
        let names: Vec<_> = plan
            .record_queries
            .iter()
            .map(|query| query.name.as_str())
            .collect();
        assert_eq!(names, [spec::TIMELINE_V2_QUERY, spec::USAGE_V2_QUERY]);
        assert_eq!(sessions(&connection, &plan.sessions_sql).len(), 1);
        assert_eq!(
            rows(&connection, &plan.record_queries[0].sql, Some("s")).len(),
            1
        );
        assert!(rows(&connection, &plan.record_queries[1].sql, Some("s")).is_empty());
    }
}
