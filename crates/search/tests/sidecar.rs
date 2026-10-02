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
        label: None,
        provenance: Default::default(),
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
        activity: None,
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
        attachments: Vec::new(),
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
        prefix: false,
        limit: 100,
        session: None,
        sessions: Vec::new(),
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
fn sidecar_indexes_no_tool_lane_under_prompt_and_reply_coverage() {
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

    // Coverage is prompts and replies (`queries::SEARCH_KINDS`, 2026-09-09):
    // the typed command, the canonical kind, the native name and the failure
    // diagnostic of a tool row all stay out of the index, and the build's
    // denominator counts only the rows it will visit.
    for term in [
        "semanticcommandsentinel",
        "shell",
        "nativenamesentinel",
        "discardeddiagnosticsentinel",
    ] {
        assert!(
            search.search(query(term)).unwrap().hits.is_empty(),
            "{term} must not be indexed"
        );
    }
    let canonical =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let indexed: i64 = canonical
        .query_row(
            "SELECT count(*) FROM message WHERE type IN ('user', 'agent')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        queries::search_document_count(&canonical).unwrap(),
        u64::try_from(indexed).unwrap()
    );
    let mut kinds = Vec::new();
    queries::visit_session_search_documents(&canonical, "s1", |document| {
        kinds.push(document.kind);
        Ok(())
    })
    .unwrap();
    assert!(
        kinds
            .iter()
            .all(|kind| queries::SEARCH_KINDS.contains(&kind.as_str()))
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
fn sidecar_never_makes_a_tool_row_a_document() {
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

    // Coverage is prompts and replies (`queries::SEARCH_KINDS`): the tool row
    // is not visited at all, so neither its bounded preview nor its member
    // can reach the index.
    let mut visited = Vec::new();
    queries::visit_session_search_documents(store.conn(), "s1", |document| {
        assert!(queries::SEARCH_KINDS.contains(&document.kind.as_str()));
        visited.push(document.timeline_id);
        Ok(())
    })
    .unwrap();
    assert!(!visited.iter().any(|id| id == "tool-member"));

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
    // Behind the watermark, the generation still answers from what it has:
    // the old `gamma` row, labelled catching-up rather than hidden behind an
    // empty page. Readers hold that answer until the repair lands.
    let held = search.search(query("gamma")).unwrap();
    assert_eq!(held.state.readiness, SearchReadiness::CatchingUp);
    assert_eq!(held.hits.len(), 1);
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

fn search_revision(store: &Store) -> u64 {
    queries::projection_state(store.conn())
        .unwrap()
        .search_revision
}

fn dirty_revision(store: &Store, session_id: &str) -> Option<i64> {
    store
        .conn()
        .query_row(
            "SELECT revision FROM search_dirty WHERE kind = 'session' AND key = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .ok()
}

/// Fresh serve wakes the paused worker the moment the projection is
/// published and lets live catch-up commit beside the initial build. The
/// build reads one canonical snapshot and records that snapshot's revision,
/// so a commit landing while it indexes is ordinary repair work: its dirty key
/// survives the build's acknowledgement and the next pass indexes it.
#[test]
fn a_commit_landing_during_the_initial_build_is_repaired_after_it() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("projection.sqlite");
    let mut store = Store::open(&path).unwrap();
    let mut ops = vec![
        IngestOp::Runtime(runtime()),
        IngestOp::SessionMeta(session("s1", None)),
        IngestOp::SessionMeta(session("s2", None)),
    ];
    // Enough documents that the commit below usually lands mid-build.
    for ordinal in 0..4_000 {
        ops.push(message_op(message(
            &format!("bulk-{ordinal}"),
            "s1",
            ordinal,
            &format!("alpha bulk document {ordinal} with several ordinary words to tokenize"),
        )));
    }
    store
        .apply(&Batch {
            ops,
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let search = Fts5SidecarSearch::new(&path);
    let worker = Fts5SearchWorker::start_paused(search.clone());
    worker.wake();

    // Progress starts only after the build's snapshot transaction has read
    // its revision, so from here on every commit postdates that snapshot.
    let mut acks = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while search.state().progress.is_none() && acks.is_empty() {
        acks.extend(worker.drain_acknowledgements());
        assert!(Instant::now() < deadline, "the initial build never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    store
        .apply(&Batch {
            ops: vec![message_op(message(
                "late",
                "s2",
                0,
                "zeta committed while the build indexed",
            ))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let late = search_revision(&store);
    assert_eq!(dirty_revision(&store, "s2"), Some(late as i64));

    // Apply acknowledgements the way the daemon's writer does until one
    // covers the late commit.
    let mut built = false;
    loop {
        acks.extend(worker.drain_acknowledgements());
        let mut covered = false;
        for ack in acks.drain(..) {
            store
                .acknowledge_search(&ack.projection_epoch, ack.revision)
                .unwrap();
            if !built {
                // The build's generation predates the late commit, and its
                // acknowledgement leaves the late session's repair key alone.
                assert!(ack.revision < late);
                assert_eq!(dirty_revision(&store, "s2"), Some(late as i64));
                built = true;
            }
            covered |= ack.revision >= late;
        }
        if covered {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the late commit was never repaired"
        );
        worker.wake();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(dirty_revision(&store, "s2"), None);
    assert_eq!(search.state().readiness, SearchReadiness::Ready);
    let hits = search.search(query("zeta")).unwrap().hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].timeline_id, "late");
    assert_eq!(search.search(query("alpha")).unwrap().hits.len(), 100);
    worker.stop();
}

/// Generations built before the metadata indexes were trimmed also carry
/// runtime, agent, project, ts, kind and error indexes plus planner
/// statistics. The search schema did not change, so such a generation stays
/// current — no rebuild — keeps answering, and takes live repairs in place.
#[test]
fn a_generation_with_the_former_metadata_indexes_stays_current_and_repairs() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, mut store) = canonical(&dir);
    let search = Fts5SidecarSearch::new(&path);
    let stop = AtomicBool::new(false);
    let first = synchronize_fts5_once(&search, &stop).unwrap().unwrap();
    store
        .acknowledge_search(&first.projection_epoch, first.revision)
        .unwrap();
    let generation = search.current_generation_path().unwrap().unwrap();
    let former = Connection::open(&generation).unwrap();
    former
        .execute_batch(
            "CREATE INDEX search_document_runtime ON search_document(runtime_id);
             CREATE INDEX search_document_agent ON search_document(agent);
             CREATE INDEX search_document_project ON search_document(project_id);
             CREATE INDEX search_document_ts ON search_document(ts);
             CREATE INDEX search_document_kind ON search_document(kind);
             CREATE INDEX search_document_error ON search_document(is_error);
             ANALYZE;",
        )
        .unwrap();
    drop(former);

    assert_eq!(
        synchronize_fts5_once(&search, &stop).unwrap().unwrap(),
        first
    );
    assert_eq!(
        search.current_generation_path().unwrap().unwrap(),
        generation
    );
    assert_eq!(search.state().readiness, SearchReadiness::Ready);

    store
        .apply(&Batch {
            ops: vec![message_op(message("m2", "s1", 1, "omega replacement"))],
            checkpoints: vec![],
            source: None,
        })
        .unwrap();
    let repaired = synchronize_fts5_once(&search, &stop).unwrap().unwrap();
    assert!(repaired.revision > first.revision);
    assert_eq!(
        search.current_generation_path().unwrap().unwrap(),
        generation,
        "repaired in place, not rebuilt"
    );
    store
        .acknowledge_search(&repaired.projection_epoch, repaired.revision)
        .unwrap();
    assert_eq!(search.search(query("omega")).unwrap().hits.len(), 1);
    assert!(search.search(query("gamma")).unwrap().hits.is_empty());
    let mut filtered = query("alpha");
    filtered.agent = Some("codex".into());
    assert_eq!(search.search(filtered).unwrap().hits.len(), 2);
    let indexes: i64 = Connection::open(&generation)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'index' AND name LIKE 'search_document_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 7);
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
