//! The metadata-only summary index, replayed through the declared plan
//! against every committed `conversation_summaries.db`. 1.4.12 adds two
//! columns (`raw_summary`, `group_id`; sources/1.4.12.sqlite-schema.sql) that
//! the plan never selects, so both writers must still admit only the root
//! conversation whose transcript the fixture holds. 1.1.20 left the index
//! empty in its print-mode basic-chat and tool-use runs (only resume wrote a
//! row); every 1.4.12 run writes its one row.

mod support;

use rosetta_adapter_antigravity::{AntigravityAdapter, spec};
use rosetta_adapter_support::adapter::AgentAdapter;

use support::fixtures_root;

#[test]
fn every_summary_index_admits_its_root_conversation() {
    let plan = AntigravityAdapter
        .snapshot_plan(spec::CONVERSATION_SUMMARIES)
        .expect("summary plan");
    let mut versions = std::collections::BTreeSet::new();
    let mut stores = 0;
    let mut admitted = 0;
    for entry in walkdir::WalkDir::new(fixtures_root()).sort_by_file_name() {
        let entry = entry.unwrap();
        if entry.file_name() != "conversation_summaries.db" {
            continue;
        }
        let db = entry.path();
        let relative = db.strip_prefix(fixtures_root()).unwrap();
        let version = relative.components().next().unwrap().as_os_str().to_owned();
        versions.insert(version.clone());

        // Copy first so SQLite's -shm bookkeeping never touches the corpus.
        let scratch = std::env::temp_dir().join(format!(
            "rosetta-antigravity-summaries-{}-{stores}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let copy = scratch.join("conversation_summaries.db");
        std::fs::copy(db, &copy).unwrap();
        let connection = rusqlite::Connection::open(&copy).unwrap();
        let mut statement = connection.prepare(&plan.sessions_sql).unwrap();
        let rows: Vec<(String, Option<String>, String)> = statement
            .query_map([], |row| {
                Ok((
                    row.get("native_session_id")?,
                    row.get("parent_native_session_id")?,
                    row.get("role")?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        if version == "1.4.12" {
            assert_eq!(rows.len(), 1, "{}", relative.display());
        }
        assert!(rows.len() <= 1, "{}", relative.display());
        for (native, parent, role) in &rows {
            assert_eq!(parent, &None, "{}", relative.display());
            assert_eq!(role, "main", "{}", relative.display());
            assert!(
                db.parent()
                    .unwrap()
                    .join("brain")
                    .join(native)
                    .join(".system_generated/logs/transcript.jsonl")
                    .is_file(),
                "{}: summary names no committed transcript",
                relative.display()
            );
            admitted += 1;
        }
        drop(statement);
        drop(connection);
        let _ = std::fs::remove_dir_all(&scratch);
        stores += 1;
    }
    assert!(stores > 0);
    assert_eq!(admitted, 5, "1.1.20 resume plus the four 1.4.12 runs");
    assert_eq!(
        versions.into_iter().collect::<Vec<_>>(),
        ["1.1.20", "1.4.12"],
        "both writer generations are exercised"
    );
}
