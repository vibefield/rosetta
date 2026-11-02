//! The logical projection dump every streamed-cold equivalence test compares
//! (R1 invariant I4): every canonical table, every column, every row, with
//! surrogate ids mapped to stable identities — a cursor to `runtime|path`,
//! a `message_id` to its `timeline_id` — and `message.id` and
//! `ingest_cursor.id` ignored. The same normalization as the frozen-snapshot
//! digest.
//!
//! [`session_views`] cuts the same mapping per session, for the per-commit
//! visibility audit: each session's payload rows as a reader sees them.

use std::collections::{BTreeMap, BTreeSet};

use rosetta_store::Store;
use rusqlite::Connection;
use rusqlite::types::Value;

/// Table name → its rows, each `column=value | …`, sorted.
pub type Dump = BTreeMap<String, Vec<String>>;

/// Columns no comparison reads: surrogate ids and run-dependent stamps.
fn always_ignored(table: &str, column: &str) -> bool {
    ((table == "message" || table == "ingest_cursor") && column == "id")
        || (table == "projection_state" && column == "projection_epoch")
        || (table == "agent_runtime" && column == "discovered_at")
}

/// Surrogate ids → stable identities.
struct Ids {
    cursors: BTreeMap<i64, String>,
    messages: BTreeMap<i64, String>,
}

impl Ids {
    fn read(conn: &Connection) -> Self {
        let cursors = conn
            .prepare("SELECT id, runtime_id, path FROM ingest_cursor")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    format!("{}|{}", row.get::<_, String>(1)?, row.get::<_, String>(2)?),
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let messages = conn
            .prepare("SELECT id, timeline_id FROM message")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        Self { cursors, messages }
    }

    fn text(&self, column: &str, value: &Value) -> String {
        match (value, column) {
            (Value::Integer(id), name) if name.ends_with("cursor") => self
                .cursors
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("<dangling cursor {id}>")),
            (Value::Integer(id), "message_id") => self
                .messages
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("<dangling message {id}>")),
            _ => format!("{value:?}"),
        }
    }
}

fn tables(conn: &Connection) -> Vec<String> {
    conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != 'source_audit'
         ORDER BY name",
    )
    .unwrap()
    .query_map([], |row| row.get(0))
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Every row of `table` over `columns` (`key` first when given, for
/// grouping), mapped through `ids`.
fn rows(
    conn: &Connection,
    ids: &Ids,
    table: &str,
    columns: &[String],
    key: Option<&str>,
) -> Vec<(Option<String>, String)> {
    let mut select: Vec<String> = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect();
    if let Some(key) = key {
        select.insert(0, format!("\"{key}\""));
    }
    let offset = usize::from(key.is_some());
    conn.prepare(&format!("SELECT {} FROM \"{table}\"", select.join(",")))
        .unwrap()
        .query_map([], |row| {
            // A key column may be NULL (a ledger-only source's cursor).
            let key = if key.is_some() {
                row.get::<_, Option<String>>(0)?
            } else {
                None
            };
            let mut fields = Vec::with_capacity(columns.len());
            for (index, column) in columns.iter().enumerate() {
                let value: Value = row.get(index + offset)?;
                fields.push(format!("{column}={}", ids.text(column, &value)));
            }
            Ok((key, fields.join(" | ")))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Every canonical table of `conn` with surrogate ids mapped, leaving out
/// `ignored` `(table, column)` pairs too.
pub fn dump_conn(conn: &Connection, ignored: &[(&str, &str)]) -> Dump {
    let ids = Ids::read(conn);
    let mut out = BTreeMap::new();
    for table in tables(conn) {
        let columns: Vec<String> = columns(conn, &table)
            .into_iter()
            .filter(|column| {
                !always_ignored(&table, column)
                    && !ignored.contains(&(table.as_str(), column.as_str()))
            })
            .collect();
        let mut rows: Vec<String> = rows(conn, &ids, &table, &columns, None)
            .into_iter()
            .map(|(_, row)| row)
            .collect();
        rows.sort();
        out.insert(table, rows);
    }
    out
}

pub fn dump_ignoring(store: &Store, ignored: &[(&str, &str)]) -> Dump {
    dump_conn(store.conn(), ignored)
}

pub fn dump(store: &Store) -> Dump {
    dump_ignoring(store, &[])
}

pub fn assert_same_projection(label: &str, reference: &Store, candidate: &Store) {
    assert_same_projection_ignoring(label, reference, candidate, &[]);
}

pub fn assert_same_projection_ignoring(
    label: &str,
    reference: &Store,
    candidate: &Store,
    ignored: &[(&str, &str)],
) {
    assert_same_dump(
        label,
        &dump_ignoring(reference, ignored),
        &dump_ignoring(candidate, ignored),
    );
}

/// Two dumps hold the same tables and rows; a difference names its table
/// and up to three rows on each side.
pub fn assert_same_dump(label: &str, reference: &Dump, candidate: &Dump) {
    if let Some(difference) = dump_difference(reference, candidate) {
        panic!("{label}: {difference}");
    }
}

/// The first difference between two dumps, described; `None` when equal.
pub fn dump_difference(reference: &Dump, candidate: &Dump) -> Option<String> {
    if reference.keys().ne(candidate.keys()) {
        return Some(format!(
            "table sets differ: {:?} vs {:?}",
            reference.keys().collect::<Vec<_>>(),
            candidate.keys().collect::<Vec<_>>()
        ));
    }
    for (table, expected) in reference {
        let actual = &candidate[table];
        if expected != actual {
            let missing: Vec<_> = expected
                .iter()
                .filter(|row| !actual.contains(row))
                .take(3)
                .collect();
            let extra: Vec<_> = actual
                .iter()
                .filter(|row| !expected.contains(row))
                .take(3)
                .collect();
            return Some(format!(
                "table {table} differs ({} reference rows, {} candidate rows)\n\
                 only in reference: {missing:#?}\nonly in candidate: {extra:#?}",
                expected.len(),
                actual.len()
            ));
        }
    }
    None
}

/// Usage columns a displacement may change on a row already visible (R1
/// D15, decision U-1): the token buckets and their stamps, and the model and
/// time a compare-and-restore puts back.
pub const MESSAGE_USAGE_COLUMNS: [&str; 14] = [
    "model",
    "ts",
    "in_tokens",
    "out_tokens",
    "cache_r_tokens",
    "cache_w_tokens",
    "think_tokens",
    "cache_w1h_tokens",
    "web_searches",
    "service_tier",
    "route",
    "billed_pusd",
    "usage_scope",
    "requests",
];

/// Tables holding usage alone.
const USAGE_TABLES: [&str; 2] = ["session_usage", "session_usage_total"];

/// Tables no per-session read serves while a fresh build is unpublished:
/// the cross-session usage fold is rebuilt at finalization, and the
/// aggregates that read it answer `not_ready` until publication.
const UNSERVED_TABLES: [&str; 1] = ["usage_bucket"];

/// What [`session_views`] leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Every payload row of the session.
    Whole,
    /// Without its usage: the usage columns of its message rows and its
    /// usage samples and totals.
    WithoutUsage,
}

/// Each of `sessions`' payload rows as a reader sees them: its `session` row
/// and every row of every table keyed by `session_id`, mapped as [`dump`]
/// maps them, per session, sorted.
pub fn session_views(
    conn: &Connection,
    sessions: &BTreeSet<String>,
    view: View,
) -> BTreeMap<String, Vec<String>> {
    let ids = Ids::read(conn);
    let mut out: BTreeMap<String, Vec<String>> =
        sessions.iter().map(|id| (id.clone(), Vec::new())).collect();
    for table in tables(conn) {
        if UNSERVED_TABLES.contains(&table.as_str())
            || (view == View::WithoutUsage && USAGE_TABLES.contains(&table.as_str()))
        {
            continue;
        }
        let all = columns(conn, &table);
        let key = if table == "session" {
            "id"
        } else if all.iter().any(|column| column == "session_id") {
            "session_id"
        } else {
            continue;
        };
        let columns: Vec<String> = all
            .into_iter()
            .filter(|column| {
                !always_ignored(&table, column)
                    && !(view == View::WithoutUsage
                        && table == "message"
                        && MESSAGE_USAGE_COLUMNS.contains(&column.as_str()))
            })
            .collect();
        for (session, row) in rows(conn, &ids, &table, &columns, Some(key)) {
            if let Some(rows) = session.and_then(|session| out.get_mut(&session)) {
                rows.push(format!("{table}: {row}"));
            }
        }
    }
    for rows in out.values_mut() {
        rows.sort();
    }
    out
}
