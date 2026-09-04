use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_core::timeline::SearchLane;
use rosetta_core::{
    CanonicalText, CorrelationDerivation, EvidenceOrder, FailureKind, IngestOp, NativeToolRef,
    SourceIdentity, StatusBasis, ToolAction, ToolCorrelation, ToolFailure, ToolInteraction,
    ToolStatus, ToolStatusFact, ToolUpdatePatch, ids,
};
use rosetta_search::{
    Fts5SearchWorker, Fts5SidecarSearch, SearchAck, SearchBackend, SearchQuery, SearchReadiness,
    TextQuery, synchronize_fts5_once,
};
use rosetta_store::queries::{self, SearchFilter};
use rosetta_store::{Batch, Store};
use rusqlite::{Connection, params};

fn runtime() -> RuntimeRow {
    RuntimeRow {
        id: "rt1".into(),
        kind: "codex".into(),
        data_root: "/agent".into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1,
    }
}

fn session(id: &str, project_id: Option<&str>) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: id.into(),
        runtime_id: "rt1".into(),
        project_id: project_id.map(str::to_string),
        cwd: project_id.map(|_| "/w/rosetta".to_owned()),
        parent_session_id: None,
        delegation: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 1,
        updated_at: 2,
        stream_state: StreamState::Indexed,
    }
}

fn message(id: &str, session_id: &str, ordinal: i64, text: &str) -> MessageRow {
    MessageRow {
        timeline_id: id.into(),
        session_id: session_id.into(),
        ordinal,
        kind: MessageKind::Agent,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(1_000 + ordinal),
        model: None,
        effort: None,
        content: Some(text.into()),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: None,
    }
}

fn message_op(row: MessageRow) -> IngestOp {
    let mut ops = IngestOp::project_message(row);
    assert_eq!(ops.len(), 1);
    ops.pop().unwrap()
}

fn tool_message() -> MessageRow {
    let mut row = message("tool-semantic", "s1", 3, "");
    row.kind = MessageKind::ToolUse;
    row.content = None;
    row.tool = Some(ToolInteraction {
        correlation: ToolCorrelation {
            key: "tool-semantic".into(),
            derivation: CorrelationDerivation::NativeId,
        },
        native: NativeToolRef {
            id: Some("tool-semantic".into()),
            namespace: None,
            name: "NativeNameSentinel".into(),
            provider: None,
        },
        status: ToolStatusFact {
            value: ToolStatus::Pending,
            basis: StatusBasis::RequestObserved,
        },
        action: ToolAction::Shell {
            command: CanonicalText::inline("semanticcommandsentinel").unwrap(),
            source_truncated: false,
            cwd: None,
            timeout_ms: None,
            background: false,
            outcome: None,
        },
        failure: None,
        timing: None,
    });
    row
}

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        text: TextQuery::parse(text).unwrap(),
        limit: 100,
        session: None,
        runtime: None,
        agent: None,
        project: None,
        kind: None,
        is_error: None,
        after_ts: None,
        before_ts: None,
    }
}

fn canonical(dir: &tempfile::TempDir) -> (std::path::PathBuf, Store) {
    let path = dir.path().join("projection.sqlite");
    let mut store = Store::open(&path).unwrap();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(runtime()),
                IngestOp::SessionMeta(session("s1", None)),
                IngestOp::SessionMeta(session("s2", None)),
                message_op(message("m1", "s1", 0, "alpha beta beta")),
                message_op(message("m2", "s1", 1, "alpha gamma")),
                message_op(message("m3", "s2", 0, "beta delta alpha")),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    (path, store)
}

fn wait_for_ack(worker: &Fts5SearchWorker, after_revision: Option<u64>) -> SearchAck {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(ack) = worker
            .drain_acknowledgements()
            .find(|ack| after_revision.is_none_or(|revision| ack.revision > revision))
        {
            return ack;
        }
        assert!(
            Instant::now() < deadline,
            "search worker did not acknowledge in time"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn legacy_reference() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE agent_runtime(id TEXT PRIMARY KEY, kind TEXT NOT NULL);
         CREATE TABLE session(
           id TEXT PRIMARY KEY, runtime_id TEXT NOT NULL, project_id TEXT
         );
         CREATE TABLE message(
           id INTEGER PRIMARY KEY, timeline_id TEXT NOT NULL, session_id TEXT NOT NULL,
           ts INTEGER, type TEXT NOT NULL, tool_is_error INTEGER,
           content TEXT, tool_input TEXT, tool_result TEXT
         );
         CREATE VIRTUAL TABLE message_fts USING fts5(
           content, tool_input, tool_result,
           content='message', content_rowid='id',
           tokenize='unicode61 remove_diacritics 2'
         );
         INSERT INTO agent_runtime VALUES ('rt1', 'codex');
         INSERT INTO session VALUES ('s1', 'rt1', NULL), ('s2', 'rt1', NULL);
         INSERT INTO message VALUES
           (1, 'm1', 's1', 1000, 'agent', NULL, 'alpha beta beta', NULL, NULL),
           (2, 'm2', 's1', 1001, 'agent', NULL, 'alpha gamma', NULL, NULL),
           (3, 'm3', 's2', 1000, 'agent', NULL, 'beta delta alpha', NULL, NULL);
         INSERT INTO message_fts(message_fts) VALUES('rebuild');",
    )
    .unwrap();
    conn
}

#[test]
fn sidecar_preserves_legacy_fts_ranking_and_snippets() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, mut store) = canonical(&dir);
    let search = Fts5SidecarSearch::new(&path);
    assert_eq!(search.state().readiness, SearchReadiness::Building);

    let ack = synchronize_fts5_once(&search, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert!(
        store
            .acknowledge_search(&ack.projection_epoch, ack.revision)
            .unwrap()
    );

    let candidate = search.search(query("alpha beta")).unwrap();
    assert_eq!(candidate.state.readiness, SearchReadiness::Ready);
    let reference = queries::search_filtered(
        &legacy_reference(),
        &TextQuery::parse("alpha beta").unwrap().fts5_expression(),
        100,
        &SearchFilter::default(),
    )
    .unwrap();
    assert_eq!(candidate.hits, reference);
}

#[test]
fn sidecar_indexes_only_explicit_typed_tool_lanes() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, mut store) = canonical(&dir);
    store
        .apply(&Batch {
            ops: vec![
                message_op(tool_message()),
                IngestOp::ToolUpdate {
                    session_id: "s1".into(),
                    correlation_key: "tool-semantic".into(),
                    evidence: EvidenceOrder {
                        source_identity: SourceIdentity("search-test".into()),
                        generation: 0,
                        record_offset: 1,
                        mutation_index: 0,
                    },
                    patch: Box::new(ToolUpdatePatch {
                        status: Some(ToolStatusFact {
                            value: ToolStatus::Failed,
                            basis: StatusBasis::NativeTerminalRecord,
                        }),
                        failure: Some(ToolFailure {
                            kind: FailureKind::Unknown,
                            summary: Some("discardeddiagnosticsentinel".into()),
                            diagnostic: None,
                        }),
                        ..Default::default()
                    }),
                    at: None,
                },
            ],
            ..Batch::default()
        })
        .unwrap();
    let search = Fts5SidecarSearch::new(&path);
    synchronize_fts5_once(&search, &AtomicBool::new(false))
        .unwrap()
        .unwrap();

    assert_eq!(
        search
            .search(query("semanticcommandsentinel"))
            .unwrap()
            .hits
            .len(),
        1
    );
    assert_eq!(search.search(query("shell")).unwrap().hits.len(), 1);
    assert!(
        search
            .search(query("nativenamesentinel"))
            .unwrap()
            .hits
            .is_empty()
    );
    assert!(
        search
            .search(query("discardeddiagnosticsentinel"))
            .unwrap()
            .hits
            .is_empty()
    );
}

#[test]
fn sidecar_resolves_full_committed_prose_members_not_only_the_preview() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, mut store) = canonical(&dir);
    let text = format!(
        "{} deepmembersentinel",
        "ordinary preview words ".repeat(2_000)
    );
    let ops = IngestOp::project_message(message("member", "s1", 4, &text));
    assert!(matches!(
        ops.first(),
        Some(IngestOp::ProjectionAsset { .. })
    ));
    store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();

    let mut observed_preview = None;
    queries::visit_session_search_documents(store.conn(), "s1", |document| {
        if document.timeline_id == "member" {
            assert!(
                document
                    .members
                    .iter()
                    .any(|member| member.lane == SearchLane::Prose)
            );
            observed_preview = Some(document.prose);
        }
        Ok(())
    })
    .unwrap();
    assert!(
        !observed_preview
            .expect("member search document")
            .contains("deepmembersentinel")
    );

    let search = Fts5SidecarSearch::new(&path);
    synchronize_fts5_once(&search, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    let hits = search.search(query("deepmembersentinel")).unwrap().hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].timeline_id, "member");
}

#[test]
fn sidecar_indexes_only_the_bounded_tool_preview() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, mut store) = canonical(&dir);
    let command = format!(
        "{} deeptoolintentsentinel",
        "ordinary command prefix ".repeat(2_000)
    );
    let mut row = tool_message();
    row.timeline_id = "tool-member".into();
    row.ordinal = 5;
    let Some(tool) = row.tool.as_mut() else {
        panic!("tool fixture");
    };
    tool.correlation.key = "tool-member".into();
    tool.native.id = Some("tool-member".into());
    tool.action = ToolAction::Shell {
        command: CanonicalText::Inline {
            text: command.clone(),
        },
        source_truncated: false,
        cwd: None,
        timeout_ms: None,
        background: false,
        outcome: None,
    };
    let ops = IngestOp::project_message(row);
    assert!(matches!(
        ops.first(),
        Some(IngestOp::ProjectionAsset { .. })
    ));
    store
        .apply(&Batch {
            ops,
            ..Batch::default()
        })
        .unwrap();

    queries::visit_session_search_documents(store.conn(), "s1", |document| {
        if document.timeline_id == "tool-member" {
            assert!(
                !document
                    .members
                    .iter()
                    .any(|member| member.lane == SearchLane::Intent)
            );
            assert!(!document.intent.contains("deeptoolintentsentinel"));
        }
        Ok(())
    })
    .unwrap();

    let search = Fts5SidecarSearch::new(&path);
    synchronize_fts5_once(&search, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    let hits = search.search(query("deeptoolintentsentinel")).unwrap().hits;
    assert!(hits.is_empty());
}

#[test]
fn commit_before_ack_replays_and_live_repair_catches_up() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, mut store) = canonical(&dir);
    let search = Fts5SidecarSearch::new(&path);
    let stop = AtomicBool::new(false);
    let first = synchronize_fts5_once(&search, &stop).unwrap().unwrap();

    // Simulate a crash after the search commit but before canonical ack.
    let replayed = synchronize_fts5_once(&search, &stop).unwrap().unwrap();
    assert_eq!(replayed, first);
    let dirty: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM search_dirty", [], |row| row.get(0))
        .unwrap();
    assert!(dirty > 0);
    store
        .acknowledge_search(&replayed.projection_epoch, replayed.revision)
        .unwrap();

    store
        .apply(&Batch {
            ops: vec![message_op(message("m2", "s1", 1, "epsilon replacement"))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    assert_eq!(search.state().readiness, SearchReadiness::CatchingUp);
    let repaired = synchronize_fts5_once(&search, &stop).unwrap().unwrap();
    assert!(search.search(query("gamma")).unwrap().hits.is_empty());
    assert_eq!(search.search(query("epsilon")).unwrap().hits.len(), 1);
    store
        .acknowledge_search(&repaired.projection_epoch, repaired.revision)
        .unwrap();
    assert_eq!(
        store
            .conn()
            .query_row("SELECT COUNT(*) FROM search_dirty", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn stale_epoch_ack_cannot_erase_current_repairs() {
    let dir = tempfile::TempDir::new().unwrap();
    let (_path, mut store) = canonical(&dir);
    let revision: i64 = store
        .conn()
        .query_row(
            "SELECT search_revision FROM projection_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !store
            .acknowledge_search("old-epoch", revision as u64)
            .unwrap()
    );
    let dirty: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM search_dirty", params![], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(dirty > 0);
}

#[test]
fn epoch_mismatch_never_serves_old_hits_and_rebuilds_a_new_generation() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, mut store) = canonical(&dir);
    let search = Fts5SidecarSearch::new(&path);
    let stop = AtomicBool::new(false);
    let first = synchronize_fts5_once(&search, &stop).unwrap().unwrap();
    store
        .acknowledge_search(&first.projection_epoch, first.revision)
        .unwrap();
    assert_eq!(search.search(query("alpha")).unwrap().hits.len(), 3);

    store
        .conn()
        .execute(
            "UPDATE projection_state
             SET projection_epoch = lower(hex(randomblob(16))), search_revision = 0
             WHERE singleton = 1",
            [],
        )
        .unwrap();
    assert_eq!(search.state().readiness, SearchReadiness::Stale);
    let unavailable = search.search(query("alpha")).unwrap();
    assert_eq!(unavailable.state.readiness, SearchReadiness::Stale);
    assert!(unavailable.hits.is_empty());

    let second = synchronize_fts5_once(&search, &stop).unwrap().unwrap();
    assert_ne!(second.projection_epoch, first.projection_epoch);
    assert_eq!(search.state().readiness, SearchReadiness::Ready);
    assert_eq!(search.search(query("alpha")).unwrap().hits.len(), 3);
}

#[test]
fn background_worker_wakes_repairs_and_delivers_acknowledgements() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, mut store) = canonical(&dir);
    let search = Fts5SidecarSearch::new(&path);
    let worker = Fts5SearchWorker::start(search.clone());

    let first = wait_for_ack(&worker, None);
    assert!(
        store
            .acknowledge_search(&first.projection_epoch, first.revision)
            .unwrap()
    );
    assert_eq!(search.state().readiness, SearchReadiness::Ready);

    store
        .apply(&Batch {
            ops: vec![message_op(message(
                "m2",
                "s1",
                1,
                "worker wake replacement",
            ))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    worker.wake();
    let repaired = wait_for_ack(&worker, Some(first.revision));
    assert!(
        store
            .acknowledge_search(&repaired.projection_epoch, repaired.revision)
            .unwrap()
    );
    assert_eq!(search.state().readiness, SearchReadiness::Ready);
    assert!(search.search(query("gamma")).unwrap().hits.is_empty());
    assert_eq!(search.search(query("worker wake")).unwrap().hits.len(), 1);

    worker.stop();
}

#[test]
fn metadata_project_assignment_repairs_structured_search_filters() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, mut store) = canonical(&dir);
    let search = Fts5SidecarSearch::new(&path);
    let stop = AtomicBool::new(false);
    let first = synchronize_fts5_once(&search, &stop).unwrap().unwrap();
    store
        .acknowledge_search(&first.projection_epoch, first.revision)
        .unwrap();

    let cwd = "/work/project";
    let project_id = ids::project_id(cwd, None);
    let mut assigned = session("s1", Some(&project_id));
    assigned.cwd = Some(cwd.into());
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Project(ProjectRow {
                    id: project_id.clone(),
                    name: "project".into(),
                    root_paths: vec![cwd.into()],
                    git_repo: None,
                }),
                IngestOp::SessionMeta(assigned),
            ],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let repaired = synchronize_fts5_once(&search, &stop).unwrap().unwrap();
    store
        .acknowledge_search(&repaired.projection_epoch, repaired.revision)
        .unwrap();

    let mut filtered = query("alpha");
    filtered.project = Some(project_id);
    let hits = search.search(filtered).unwrap().hits;
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|hit| hit.session_id == "s1"));
}
