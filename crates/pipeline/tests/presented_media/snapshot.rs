//! The snapshot driver: a database-first agent's sent file is captured,
//! replayed and carried into a later generation (moved from
//! `presented_media.rs`, which keeps the shared fixture).

use super::*;

/// A database-first agent whose `sent` rows list files a tool row sent.
struct SenderDb;

impl AgentAdapter for SenderDb {
    fn kind(&self) -> &'static str {
        "senderdb"
    }

    fn layout(&self) -> &GroundTruthDoc {
        static LAYOUT: OnceLock<GroundTruthDoc> = OnceLock::new();
        LAYOUT.get_or_init(|| {
            GroundTruthDoc::parse(
                r#"{"agent":"senderdb","data_root":"~/.senderdb","streams":[
                    {"id":"db","glob":"agent.db*","file_format":"sqlite",
                     "discipline":"database","evidence":"fixture"}]}"#,
            )
            .unwrap()
        })
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        (stream_id == "db").then_some(StreamDriver::SqliteSnapshot)
    }

    fn normalizer_version(&self) -> u32 {
        1
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        (stream_id == "db").then(|| SnapshotPlan {
            sessions_sql: "SELECT id AS native_session_id, title, cwd, updated_at
                           FROM sessions ORDER BY id"
                .to_string(),
            record_queries: vec![SnapshotQuery {
                name: "messages".to_string(),
                sql: "SELECT id, role, body FROM messages WHERE session = ?1 ORDER BY id"
                    .to_string(),
            }],
        })
    }

    fn skim(&self, _stream_id: &str, _stat: &FileStat, _head: &[u8], _tail: &[u8]) -> SkimResult {
        SkimResult::default()
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, _stream_id: &str, line: &[u8]) -> LineOutcome {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            return LineOutcome {
                ops: vec![],
                status: LineStatus::HardError {
                    code: "invalid_json".to_string(),
                },
            };
        };
        let row = &value["row"];
        let id = row["id"].as_i64().unwrap_or_default();
        let body = row["body"].as_str().unwrap_or_default();
        match row["role"].as_str() {
            Some("tool") => LineOutcome::parsed(opener(ctx, body)),
            Some("sent") => {
                let sent_files: Value = serde_json::from_str(body).unwrap_or_default();
                LineOutcome::parsed(sent(
                    ctx,
                    sent_files["call"].as_str().unwrap_or_default(),
                    &format!("sent-{id}"),
                    &sent_files["files"],
                ))
            }
            _ => {
                let message: MessageRow = serde_json::from_value(json!({
                    "timeline_id": ids::timeline_id(&ctx.session_id, &format!("msg-{id}")),
                    "session_id": ctx.session_id, "ordinal": ctx.take_ordinal(),
                    "kind": "agent", "ts": id * 1_000, "content": body,
                    "attachments": [], "citations": [],
                    "src_cursor": ctx.src_cursor, "src_offset": ctx.src_offset,
                }))
                .unwrap();
                LineOutcome::parsed(IngestOp::project_message(message))
            }
        }
    }
}

#[test]
fn the_snapshot_driver_captures_replays_and_carries_a_sent_file() {
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&SenderDb.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    let video = mp4(4096, 9);
    let clip = home.path().join("clip.mp4");
    std::fs::write(&clip, &video).unwrap();
    let files =
        json!({"call":"send-1","files":[{"path":clip,"size":4096,"media_type":"video/mp4"}]});
    let agent_db = root.join("agent.db");
    let conn = rusqlite::Connection::open(&agent_db).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, cwd TEXT, updated_at INTEGER);
         CREATE TABLE messages (id INTEGER PRIMARY KEY, session TEXT, role TEXT, body TEXT);
         INSERT INTO sessions VALUES ('sess-a', 'Sent a clip', '/w/alpha', 1000);
         INSERT INTO messages VALUES (1, 'sess-a', 'agent', 'here is the recording');
         INSERT INTO messages VALUES (2, 'sess-a', 'tool', 'send-1');",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages VALUES (3, 'sess-a', 'sent', ?1)",
        [files.to_string()],
    )
    .unwrap();
    drop(conn);

    let opts = options();
    let instrument = Instrument::new();
    let mut runtime = discover(&[&SenderDb], home.path(), &opts, &instrument)
        .unwrap()
        .remove(0)
        .runtime;
    runtime.enabled = true;
    let db = home.path().join("projection.sqlite");
    let ledger_dir = home.path().join("ledger");
    let mut store = Store::open(&db).unwrap();
    let mut ledger = Ledger::open(&ledger_dir).unwrap();
    let pass = |store: &mut Store, ledger: &mut Ledger| {
        let (_, files) = meta_ingest_into(
            &SenderDb,
            &runtime,
            &root,
            store,
            &opts,
            &instrument,
            Some(&mut *ledger),
        )
        .unwrap();
        let report = stream_backfill_into(
            &SenderDb,
            &runtime,
            &root,
            store,
            &files,
            &opts,
            &instrument,
            Some(&mut *ledger),
        )
        .unwrap();
        assert_eq!(report.hard_errors, 0);
        ledger.flush().unwrap();
    };
    let replay = |name: &str| {
        let db = home.path().join(format!("{name}.sqlite"));
        let mut store = Store::open(&db).unwrap();
        rebuild_runtime_with_options(
            &SenderDb,
            &runtime,
            &ledger_dir,
            &mut store,
            &Instrument::new(),
            RebuildOptions::default(),
        )
        .unwrap();
        (store, db)
    };

    // The switch: a reference, no member and no frame, on this driver too.
    {
        let quiet = IngestOptions {
            observe_presented_media: false,
            ..options()
        };
        let quiet_db = home.path().join("quiet.sqlite");
        let quiet_dir = home.path().join("quiet-ledger");
        let mut quiet_store = Store::open(&quiet_db).unwrap();
        let mut quiet_ledger = Ledger::open(&quiet_dir).unwrap();
        let (_, files) = meta_ingest_into(
            &SenderDb,
            &runtime,
            &root,
            &mut quiet_store,
            &quiet,
            &instrument,
            Some(&mut quiet_ledger),
        )
        .unwrap();
        stream_backfill_into(
            &SenderDb,
            &runtime,
            &root,
            &mut quiet_store,
            &files,
            &quiet,
            &instrument,
            Some(&mut quiet_ledger),
        )
        .unwrap();
        quiet_ledger.flush().unwrap();
        let shown = attachments(&quiet_store, &ids::session_id(&runtime.id, "sess-a"));
        assert_eq!(shown.len(), 1);
        assert!(matches!(
            shown[0].source,
            AttachmentSource::Reference { .. }
        ));
        assert!(members(&quiet_db).is_empty());
        assert!(media_frames(&quiet_dir).is_empty());
    }

    // Generation 1: captured at the record's index among the session's rows.
    pass(&mut store, &mut ledger);
    let session = ids::session_id(&runtime.id, "sess-a");
    let shown = attachments(&store, &session);
    assert_eq!(shown.len(), 1);
    assert_eq!(asset_digest(&shown[0]), Some(digest(&video).as_str()));
    let captured = BTreeMap::from([(format!("{}.mp4", digest(&video)), video.clone())]);
    assert_eq!(members(&db), captured);
    let frames = media_frames(&ledger_dir);
    assert_eq!(frames.len(), 1);
    assert_eq!(
        (frames[0].0, frames[0].1, frames[0].2.status),
        (1, 2, Status::Captured)
    );
    assert_eq!(frames[0].2.recovered_from, None);

    // Rebuilt from the ledger with the file gone.
    std::fs::remove_file(&clip).unwrap();
    let (rebuilt, rebuilt_db) = replay("rebuilt");
    assert_eq!(attachments(&rebuilt, &session), shown);
    assert_eq!(
        tool_details(&rebuilt, &session),
        tool_details(&store, &session)
    );
    assert_eq!(members(&rebuilt_db), captured);

    // Generation 2: the database changed, the file is still gone, and the
    // capture is carried from the ledger to the record's new coordinate.
    let conn = rusqlite::Connection::open(&agent_db).unwrap();
    conn.execute_batch("DELETE FROM messages WHERE id = 1;")
        .unwrap();
    drop(conn);
    pass(&mut store, &mut ledger);
    let carried = attachments(&store, &session);
    assert_eq!(carried.len(), 1);
    assert_eq!(asset_digest(&carried[0]), Some(digest(&video).as_str()));
    let frames = media_frames(&ledger_dir);
    assert_eq!(frames.len(), 2);
    assert_eq!(
        (frames[1].0, frames[1].1, frames[1].2.status),
        (2, 1, Status::Captured)
    );
    assert_eq!(frames[1].2.recovered_from.as_deref(), Some("ledger:1:2"));
    assert_eq!(frames[1].2.captured_at, frames[0].2.captured_at);
    let (rebuilt, rebuilt_db) = replay("rebuilt-again");
    assert_eq!(attachments(&rebuilt, &session), carried);
    assert_eq!(members(&rebuilt_db), captured);
}
