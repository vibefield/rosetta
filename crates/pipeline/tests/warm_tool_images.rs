//! New warm sessions and resumed tool outputs publish durable optimized assets.
use rosetta_adapter_codex::CodexAdapter;
use rosetta_capture::{Ledger, ScriptedEvents};
use rosetta_core::{
    ids,
    rows::RuntimeRow,
    timeline::{AttachmentSource, TimelineBody, TimelineEntry},
};
use rosetta_pipeline::{
    IngestOptions, Instrument, LiveOptions, LiveRunner, meta_ingest_into, rebuild_runtime,
    stream_backfill_into,
};
use rosetta_store::{Store, assets, queries};
use serde_json::{Value, json};
use std::{io::Write, path::Path, time::Duration};

fn record(payload: Value) -> String {
    json!({"timestamp":"2026-09-09T00:00:01Z","type":"response_item","payload":payload}).to_string()
        + "\n"
}
fn rows(store: &Store, runtime: &RuntimeRow) -> Vec<TimelineEntry> {
    queries::timeline_page(
        store.conn(),
        &ids::session_id(&runtime.id, "warm-images"),
        None,
        100,
    )
    .unwrap()
}

#[test]
fn new_warm_session_then_resumed_outputs_equal_cold_and_ledger_only_replay() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("codex");
    std::fs::create_dir_all(&root).unwrap();
    let relative = "sessions/2026/09/09/rollout-images.jsonl";
    let native = root.join(relative);
    let projection = temp.path().join("warm.sqlite");
    let ledger_dir = temp.path().join("ledger");
    let mut store = Store::open(&projection).unwrap();
    let runtime = RuntimeRow {
        id: ids::runtime_id("codex", root.to_str().unwrap()),
        kind: "codex".into(),
        data_root: root.to_str().unwrap().into(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1,
        label: None,
        provenance: Default::default(),
    };
    let opts = IngestOptions {
        audit_files_per_run: 0,
        ..Default::default()
    };
    let live = LiveOptions {
        tick: Duration::ZERO,
        poll_interval: Duration::from_secs(3600),
        max_coalesced: 16,
    };
    let instrument = Instrument::new();
    let mut events = ScriptedEvents::default();
    events.push_changed(relative);
    // The warm roster exists before the new session does.
    let mut runner = LiveRunner::start_with_ledger(
        &CodexAdapter,
        runtime.clone(),
        root.clone(),
        Box::new(events),
        &mut store,
        opts.clone(),
        live.clone(),
        Some(Ledger::open(&ledger_dir).unwrap()),
        &instrument,
    )
    .unwrap();
    std::fs::create_dir_all(native.parent().unwrap()).unwrap();
    let header = json!({"timestamp":"2026-09-09T00:00:00Z","type":"session_meta","payload":{"id":"warm-images","timestamp":"2026-09-09T00:00:00Z","cwd":"/fixture/project","originator":"codex_exec","cli_version":"0.150.1","source":"cli","model_provider":"openai"}});
    let call = record(
        json!({"type":"custom_tool_call","call_id":"image-call","name":"exec","input":"const r = await tools.view_image({path: '/already-deleted.png'}); image(r);"}),
    );
    std::fs::write(&native, format!("{header}\n{call}")).unwrap();
    runner.tick(&mut store, &instrument).unwrap();
    let first = rows(&store, &runtime);
    assert_eq!(first.len(), 1);
    let TimelineBody::ToolUse { tool } = &first[0].body else {
        panic!("tool")
    };
    assert!(tool.attachments.is_empty());
    drop(runner);
    drop(store);

    // Append after a restart: both the opener and pending-call checkpoint live on disk.
    let image = json!({"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII="});
    let output = record(
        json!({"type":"custom_tool_call_output","call_id":"image-call","output":[image,image]}),
    );
    std::fs::OpenOptions::new()
        .append(true)
        .open(&native)
        .unwrap()
        .write_all(output.as_bytes())
        .unwrap();
    let mut store = Store::open(&projection).unwrap();
    let mut events = ScriptedEvents::default();
    events.push_changed(relative);
    let mut runner = LiveRunner::start_with_ledger(
        &CodexAdapter,
        runtime.clone(),
        root.clone(),
        Box::new(events),
        &mut store,
        opts.clone(),
        live,
        Some(Ledger::open(&ledger_dir).unwrap()),
        &instrument,
    )
    .unwrap();
    runner.tick(&mut store, &instrument).unwrap();
    let warm = rows(&store, &runtime);
    assert_eq!(warm.len(), 1);
    assert_eq!(warm[0].timeline_id, first[0].timeline_id);
    let TimelineBody::ToolUse { tool } = &warm[0].body else {
        panic!("tool")
    };
    assert_eq!(tool.attachments.len(), 2);
    assert_ne!(tool.attachments[0].id, tool.attachments[1].id);
    assert_eq!(tool.attachments[0].source, tool.attachments[1].source);
    let AttachmentSource::Asset { asset } = &tool.attachments[0].source else {
        panic!("optimized asset")
    };
    let mut members = Vec::new();
    for member in std::iter::once(asset).chain(asset.image_variants.as_ref().unwrap().values()) {
        let name = Path::new(&member.url).file_name().unwrap().to_owned();
        let bytes = std::fs::read(assets::asset_root(&projection).join(&name)).unwrap();
        assert_eq!(member.media_type, "image/webp");
        assert_eq!(&bytes[8..12], b"WEBP");
        assert_eq!(bytes.len() as u64, member.byte_len);
        assert_eq!(blake3::hash(&bytes).to_hex().as_str(), member.digest);
        members.push((name, bytes));
    }
    assert_eq!(
        assets::prune_unreferenced_images(store.conn(), &projection).unwrap(),
        0
    );
    assert_eq!(
        members.len(),
        5,
        "all members exist before the tool row is visible"
    );
    assert!(!serde_json::to_string(&warm).unwrap().contains("base64,"));
    drop(runner);
    drop(store);

    let cold_path = temp.path().join("cold.sqlite");
    let mut cold = Store::open(&cold_path).unwrap();
    let (_, files) = meta_ingest_into(
        &CodexAdapter,
        &runtime,
        &root,
        &mut cold,
        &opts,
        &instrument,
        None,
    )
    .unwrap();
    let report = stream_backfill_into(
        &CodexAdapter,
        &runtime,
        &root,
        &mut cold,
        &files,
        &opts,
        &instrument,
        None,
    )
    .unwrap();
    assert_eq!(report.hard_errors, 0);
    assert_eq!(rows(&cold, &runtime)[0].body, warm[0].body);
    drop(cold);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(assets::asset_root(&projection)).unwrap();
    let rebuilt = temp.path().join("replay.sqlite");
    let mut replay = Store::open_in_memory_for_rebuild_with_assets(&rebuilt).unwrap();
    rebuild_runtime(
        &CodexAdapter,
        &runtime,
        &ledger_dir,
        &mut replay,
        &instrument,
    )
    .unwrap();
    let replayed = rows(&replay, &runtime);
    assert_eq!(replayed[0].timeline_id, warm[0].timeline_id);
    assert_eq!(replayed[0].body, warm[0].body);
    for (name, bytes) in members {
        assert_eq!(
            std::fs::read(assets::asset_root(&rebuilt).join(name)).unwrap(),
            bytes
        );
    }
}
