//! `absorb_meta` against `rewarm`: the metadata seam must leave the mirror in
//! exactly the state a whole-surface rewarm would have built, one named row
//! at a time, and publish the same precise diffs on the way there.

use std::time::Duration;

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_server::{Hub, HubConfig, HubItem};
use rosetta_store::hot::{MetaEffects, Mutation};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};

const TICK: Duration = Duration::from_millis(200);

fn session(id: &str, title: Option<&str>) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/rosetta".into()),
        parent_session_id: None,
        delegation: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: title.map(str::to_owned),
        summary: None,
        started_at: 10_000,
        updated_at: 10_000,
        activity: None,
        stream_state: StreamState::Streaming,
    }
}

fn meta_batch() -> Batch {
    Batch {
        ops: vec![
            IngestOp::Runtime(RuntimeRow {
                id: "rt1".into(),
                kind: "claude-code".into(),
                data_root: "~/.claude".into(),
                enabled: true,
                account: None,
                config: None,
                skills: None,
                versions_seen: None,
                discovered_at: 1_000,
                label: None,
                provenance: Default::default(),
            }),
            IngestOp::Project(ProjectRow {
                id: "p1".into(),
                name: "rosetta".into(),
                root_paths: vec!["/w/rosetta".into()],
                git_repo: None,
            }),
            IngestOp::SessionMeta(session("s1", None)),
            IngestOp::SessionMeta(session("s2", None)),
        ],
        ..Batch::default()
    }
}

fn message(timeline_id: &str, cursor: i64) -> MessageRow {
    MessageRow {
        timeline_id: timeline_id.into(),
        session_id: "s1".into(),
        ordinal: 0,
        kind: MessageKind::User,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(3_600_000),
        model: None,
        effort: None,
        content: Some(format!("content of {timeline_id}")),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: Some(cursor),
        src_offset: Some(0),
    }
}

fn store_with_meta() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store.observe_meta_effects();
    store.apply(&meta_batch()).unwrap();
    store
}

fn install_cursor(store: &mut Store) -> i64 {
    store
        .apply(&Batch {
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: "p/x.jsonl".into(),
                session_id: Some("s1".into()),
                expected: None,
                next: "blob-v0".into(),
                updated_at: Some(1),
            }],
            ..Batch::default()
        })
        .unwrap();
    queries::cursor_state(store.conn(), "rt1", "p/x.jsonl")
        .unwrap()
        .unwrap()
        .id
}

fn source(cursor: i64, generation: u64, first_sequence: u64, mode: SourceMode) -> SourceWrite {
    SourceWrite {
        cursor_id: cursor,
        session_id: "s1".into(),
        generation,
        first_sequence,
        mode,
    }
}

/// The drain-and-absorb the daemon would run after each committed batch.
fn pump(store: &mut Store, hub: &Hub) -> usize {
    let effects = store.drain_meta_effects();
    hub.absorb_meta(store.conn(), effects).unwrap()
}

fn sessions(hub: &Hub) -> Vec<SessionRow> {
    let mut rows = hub.sessions();
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    rows
}

fn projects(hub: &Hub) -> Vec<ProjectRow> {
    let mut rows = hub.projects();
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    rows
}

fn runtimes(hub: &Hub) -> Vec<RuntimeRow> {
    let mut rows = hub.runtimes();
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    rows
}

#[test]
fn absorb_meta_leaves_the_mirror_a_rewarm_would_have_built() {
    let mut store = store_with_meta();
    // The incremental hub starts cold and only ever sees absorb_meta — the
    // daemon shape this exists for.
    let incremental = Hub::new(HubConfig::default(), 1);
    assert_eq!(
        pump(&mut store, &incremental),
        4,
        "runtime, project, s1, s2"
    );

    let cursor = install_cursor(&mut store);
    assert_eq!(
        pump(&mut store, &incremental),
        0,
        "a checkpoint moves no identity row"
    );

    // A stream batch: the message drags the session's activity time forward.
    store
        .apply(&Batch {
            ops: IngestOp::project_message(message("m1", cursor)),
            source: Some(source(cursor, 0, 0, SourceMode::Append)),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(pump(&mut store, &incremental), 1);

    // The readiness badge, which never round-trips a projected row.
    store
        .set_session_stream_state("s1", StreamState::Indexed)
        .unwrap();
    assert_eq!(pump(&mut store, &incremental), 1);

    // A rewrite: stage a replacement generation, then activate it — the
    // sweep removes the old contribution and renumbers the session. The
    // replacement record is later, so the session's activity time moves.
    let mut rewritten = message("rewritten", cursor);
    rewritten.ts = Some(3_900_000);
    store
        .apply(&Batch {
            ops: IngestOp::project_message(rewritten),
            source: Some(source(cursor, 1, 0, SourceMode::Stage)),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(
        pump(&mut store, &incremental),
        0,
        "staging projects nothing"
    );
    store
        .apply(&Batch {
            ops: vec![],
            source: Some(source(cursor, 1, 1, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(pump(&mut store, &incremental), 1);

    // A later metadata pass retitles the other session, finds a second
    // worktree root for the project, and re-reads the runtime's data root —
    // all three key kinds ride this seam.
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::SessionMeta(session("s2", Some("Named"))),
                IngestOp::Project(ProjectRow {
                    id: "p1".into(),
                    name: "rosetta".into(),
                    root_paths: vec!["/w/rosetta-worktree".into()],
                    git_repo: Some("git@example.test:rosetta.git".into()),
                }),
                IngestOp::Runtime(RuntimeRow {
                    id: "rt1".into(),
                    kind: "claude-code".into(),
                    data_root: "~/.config/claude".into(),
                    enabled: true,
                    account: None,
                    config: None,
                    skills: None,
                    versions_seen: None,
                    discovered_at: 1_000,
                    label: None,
                    provenance: Default::default(),
                }),
            ],
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(pump(&mut store, &incremental), 3);

    // A row the projection no longer holds. No store path deletes identity
    // rows today, so this stands in for a projection swapped underneath a
    // live hub: the drained id is the only tombstone the mirror needs, and
    // the metadata topic has no removal event to publish.
    store
        .conn()
        .execute("DELETE FROM session WHERE id = 's2'", [])
        .unwrap();
    let dropped = MetaEffects {
        sessions: ["s2".to_string()].into_iter().collect(),
        ..MetaEffects::default()
    };
    assert_eq!(incremental.absorb_meta(store.conn(), dropped).unwrap(), 0);

    // The comparison: a hub that never saw an effect, built wholesale.
    let rebuilt = Hub::new(HubConfig::default(), 1);
    rebuilt.warm(store.conn()).unwrap();
    assert_eq!(sessions(&incremental), sessions(&rebuilt));
    assert_eq!(projects(&incremental), projects(&rebuilt));
    assert_eq!(runtimes(&incremental), runtimes(&rebuilt));
    assert_eq!(
        sessions(&incremental).len(),
        1,
        "the dropped session left both mirrors"
    );

    // And nothing resident is stale: a full rewarm over the incrementally
    // maintained mirror finds nothing left to say.
    assert_eq!(incremental.rewarm(store.conn()).unwrap(), 0);
}

#[test]
fn absorb_meta_publishes_precise_diffs_and_stays_silent_on_no_ops() {
    let mut store = store_with_meta();
    let hub = Hub::new(HubConfig::default(), 1);
    hub.warm(store.conn()).unwrap();
    store.drain_meta_effects();
    let sub = hub.subscribe_meta();

    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(session("s1", Some("Named")))],
            ..Batch::default()
        })
        .unwrap();
    assert_eq!(pump(&mut store, &hub), 1);
    match sub.recv_timeout(TICK) {
        Some(HubItem::Mutation { mutation, stamp }) => {
            assert_eq!(stamp, None, "the meta topic has no ring position");
            match mutation.as_ref() {
                Mutation::Session(row) => assert_eq!(row.title.as_deref(), Some("Named")),
                other => panic!("expected a session diff, got {other:?}"),
            }
        }
        other => panic!("expected a mutation, got {other:?}"),
    }
    assert_eq!(
        hub.session("s1").unwrap().title.as_deref(),
        Some("Named"),
        "list reads serve from the mirror, not the DB"
    );

    // The identical upsert changes no row. The id is still reported —
    // precision belongs to the mirror's diff, not to the store guessing —
    // and absorbing it reloads that row and publishes nothing.
    store
        .apply(&Batch {
            ops: vec![IngestOp::SessionMeta(session("s1", Some("Named")))],
            ..Batch::default()
        })
        .unwrap();
    let repeated = store.drain_meta_effects();
    assert!(!repeated.is_empty(), "the touched id still reaches the hub");
    assert_eq!(hub.absorb_meta(store.conn(), repeated).unwrap(), 0);
    assert_eq!(sub.try_recv(), None);
}

/// Every mutation a meta subscriber holds right now.
fn drain(subscription: &rosetta_server::Subscription) -> Vec<Mutation> {
    let mut seen = Vec::new();
    while let Some(item) = subscription.try_recv() {
        if let HubItem::Mutation { mutation, .. } = item {
            seen.push(mutation.as_ref().clone());
        }
    }
    seen
}

#[test]
fn a_live_library_change_announces_its_revision_once_on_the_meta_topic() {
    use rosetta_core::html_artifacts::{HtmlArtifactPublication, identity};
    let mut store = store_with_meta();
    let hub = Hub::new(HubConfig::default(), 1);
    assert_eq!(
        pump(&mut store, &hub),
        4,
        "the first revision read is a baseline, never an announcement"
    );
    let meta = hub.subscribe_meta();
    let publication = HtmlArtifactPublication {
        id: identity(&["publication"]),
        artifact_id: identity(&["artifact"]),
        session_id: "s1".into(),
        timeline_id: "t1".into(),
        title: "Review".into(),
        filename: "review.html".into(),
        path: Some("/w/rosetta/review.html".into()),
        url: None,
        native_id: None,
        version: None,
        published_at: Some(5),
        source_offset: 10,
        description: Some("Two checkout flows".into()),
        dropped_files: Some(1),
    };
    store
        .apply(&Batch {
            ops: vec![IngestOp::HtmlArtifact(publication.clone())],
            ..Batch::default()
        })
        .unwrap();
    pump(&mut store, &hub);
    let seen = drain(&meta);
    assert_eq!(
        seen.iter()
            .filter(|mutation| matches!(mutation, Mutation::ArtifactsChanged { revision: 1 }))
            .count(),
        1,
        "{seen:?}"
    );

    // A replay that restates the held publication moves nothing.
    store
        .apply(&Batch {
            ops: vec![IngestOp::HtmlArtifact(publication)],
            ..Batch::default()
        })
        .unwrap();
    pump(&mut store, &hub);
    assert!(
        !drain(&meta)
            .iter()
            .any(|mutation| matches!(mutation, Mutation::ArtifactsChanged { .. })),
        "an unchanged library is never announced"
    );
    assert_eq!(
        rosetta_store::html_artifacts::revision(store.conn()).unwrap(),
        1
    );

    // A message is not a library change either.
    let cursor = install_cursor(&mut store);
    store
        .apply(&Batch {
            ops: IngestOp::project_message(message("m1", cursor)),
            source: Some(source(cursor, 0, 0, SourceMode::Append)),
            ..Batch::default()
        })
        .unwrap();
    pump(&mut store, &hub);
    assert!(
        !drain(&meta)
            .iter()
            .any(|mutation| matches!(mutation, Mutation::ArtifactsChanged { .. }))
    );
    assert_eq!(
        rosetta_store::html_artifacts::revision(store.conn()).unwrap(),
        1
    );
}

/// One publication of `name` in `s1`, at `at`, owned by no source.
fn publication(name: &str, at: i64) -> rosetta_core::html_artifacts::HtmlArtifactPublication {
    use rosetta_core::html_artifacts::{HtmlArtifactPublication, identity};
    HtmlArtifactPublication {
        id: identity(&["publication", name]),
        artifact_id: identity(&["artifact", name]),
        session_id: "s1".into(),
        timeline_id: name.into(),
        title: name.into(),
        filename: format!("{name}.html"),
        path: Some(format!("/w/rosetta/{name}.html")),
        url: None,
        native_id: None,
        version: None,
        published_at: Some(at),
        source_offset: at as u64,
        description: None,
        dropped_files: Some(0),
    }
}

/// The revisions among `seen`'s library announcements, in order.
fn announced(seen: &[Mutation]) -> Vec<u64> {
    seen.iter()
        .filter_map(|mutation| match mutation {
            Mutation::ArtifactsChanged { revision } => Some(*revision),
            _ => None,
        })
        .collect()
}

#[test]
fn a_capture_that_lands_on_its_own_announces_the_library_once() {
    use rosetta_core::ProjectionAsset;
    use rosetta_core::html_artifacts::{
        HTML_MEDIA_TYPE, HtmlArtifactAvailability, HtmlArtifactContent,
    };
    let mut store = store_with_meta();
    let hub = Hub::new(HubConfig::default(), 1);
    pump(&mut store, &hub);
    let meta = hub.subscribe_meta();
    let review = publication("review", 5);
    store
        .apply(&Batch {
            ops: vec![IngestOp::HtmlArtifact(review.clone())],
            ..Batch::default()
        })
        .unwrap();
    pump(&mut store, &hub);
    assert_eq!(announced(&drain(&meta)), [1]);

    // The capture arrives in a batch of its own, as a late capture or a
    // recovery does.
    let asset = ProjectionAsset::new(HTML_MEDIA_TYPE, b"<h1>Review</h1>".to_vec()).unwrap();
    let capture = Batch {
        ops: vec![
            IngestOp::ProjectionAsset {
                asset: asset.clone(),
            },
            IngestOp::HtmlArtifactContent(HtmlArtifactContent {
                session_id: "s1".into(),
                source_offset: review.source_offset,
                path: review.path.clone(),
                availability: HtmlArtifactAvailability::Captured,
                captured_at: Some(50),
                recovered_from: None,
                asset: Some(asset.reference()),
            }),
        ],
        ..Batch::default()
    };
    store.apply(&capture).unwrap();
    pump(&mut store, &hub);
    assert_eq!(announced(&drain(&meta)), [2]);
    store.apply(&capture).unwrap();
    pump(&mut store, &hub);
    assert_eq!(
        announced(&drain(&meta)),
        Vec::<u64>::new(),
        "a replayed capture"
    );
    assert_eq!(
        rosetta_store::html_artifacts::revision(store.conn()).unwrap(),
        2
    );
}

#[test]
fn a_replaced_source_generation_that_sweeps_a_publication_announces_the_library_once() {
    let mut store = store_with_meta();
    let hub = Hub::new(HubConfig::default(), 1);
    pump(&mut store, &hub);
    let cursor = install_cursor(&mut store);
    pump(&mut store, &hub);
    let meta = hub.subscribe_meta();

    // Generation 0 of the transcript publishes; the source owns the row.
    let mut ops = IngestOp::project_message(message("m1", cursor));
    ops.push(IngestOp::HtmlArtifact(publication("review", 5)));
    store
        .apply(&Batch {
            ops,
            source: Some(source(cursor, 0, 0, SourceMode::Append)),
            ..Batch::default()
        })
        .unwrap();
    pump(&mut store, &hub);
    assert_eq!(announced(&drain(&meta)), [1]);

    // A rewrite without the publication: staging moves nothing, and the
    // activation's sweep removes the row, one step for the transaction.
    store
        .apply(&Batch {
            ops: IngestOp::project_message(message("rewritten", cursor)),
            source: Some(source(cursor, 1, 0, SourceMode::Stage)),
            ..Batch::default()
        })
        .unwrap();
    pump(&mut store, &hub);
    assert_eq!(announced(&drain(&meta)), Vec::<u64>::new(), "staging");
    store
        .apply(&Batch {
            ops: vec![],
            source: Some(source(cursor, 1, 1, SourceMode::Activate)),
            ..Batch::default()
        })
        .unwrap();
    pump(&mut store, &hub);
    assert_eq!(announced(&drain(&meta)), [2]);
    assert_eq!(
        rosetta_store::html_artifacts::revision(store.conn()).unwrap(),
        2
    );
    let library = rosetta_store::html_artifacts::list(
        store.conn(),
        &rosetta_store::html_artifacts::ArtifactListRequest::default(),
        false,
    )
    .unwrap();
    assert!(
        library.entries.is_empty(),
        "the sweep removed the publication"
    );
}

#[test]
fn a_cold_build_announces_its_growing_library_on_the_meta_topic() {
    // The fresh build's shape: a private cold store that readers attach to,
    // its metadata seam drained after each commit.
    let mut store = Store::open_in_memory_for_rebuild().unwrap();
    store.observe_meta_effects();
    store.apply(&meta_batch()).unwrap();
    let hub = Hub::new(HubConfig::default(), 1);
    assert_eq!(
        pump(&mut store, &hub),
        4,
        "the first revision read is a baseline"
    );
    let meta = hub.subscribe_meta();
    for (step, name) in ["first", "second"].into_iter().enumerate() {
        store
            .apply(&Batch {
                ops: vec![IngestOp::HtmlArtifact(publication(name, 5 + step as i64))],
                ..Batch::default()
            })
            .unwrap();
        pump(&mut store, &hub);
        assert_eq!(announced(&drain(&meta)), [step as u64 + 1], "{name}");
    }
}
