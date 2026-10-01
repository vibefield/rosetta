//! Authored media survives capture, publication and replay without native files.
use rosetta_adapter_codex::CodexAdapter;
use rosetta_capture::Ledger;
use rosetta_core::{
    ids,
    rows::RuntimeRow,
    timeline::{AttachmentSource, TimelineBody},
};
use rosetta_pipeline::{
    IngestOptions, Instrument, meta_ingest_into, rebuild_runtime, stream_backfill_into,
};
use rosetta_store::{Store, assets, queries};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    path::Path,
};

fn exercise(header: &str, message: &str) -> Vec<(u64, Option<(u32, u32)>)> {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("codex");
    let path = root.join("sessions/2026/09/09/rollout-images.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("{header}\n{message}\n")).unwrap();
    let header: Value = serde_json::from_str(header).unwrap();
    let native = header["payload"]["id"].as_str().unwrap();
    let runtime = RuntimeRow {
        id: ids::runtime_id("codex", root.to_str().unwrap()),
        kind: "codex".into(),
        data_root: root.to_string_lossy().into_owned(),
        enabled: true,
        account: None,
        config: None,
        skills: None,
        versions_seen: None,
        discovered_at: 1,
        label: None,
        provenance: Default::default(),
    };
    let session = ids::session_id(&runtime.id, native);
    let projection = temp.path().join("original.sqlite");
    let ledger_dir = temp.path().join("ledger");
    let mut ledger = Ledger::open(&ledger_dir).unwrap();
    let mut store = Store::open(&projection).unwrap();
    let instrument = Instrument::new();
    let options = IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    };
    let (_, files) = meta_ingest_into(
        &CodexAdapter,
        &runtime,
        &root,
        &mut store,
        &options,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    let report = stream_backfill_into(
        &CodexAdapter,
        &runtime,
        &root,
        &mut store,
        &files,
        &options,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    assert_eq!(report.hard_errors, 0);
    assert_eq!(report.files_incomplete, 0);
    let rows = queries::timeline_page(store.conn(), &session, None, 100).unwrap();
    assert_eq!(rows.len(), 1, "images belong to one prompt");
    let TimelineBody::User { text, attachments } = &rows[0].body else {
        panic!("user message")
    };
    assert!(!text.as_ref().unwrap().as_preview().contains("<image name="));
    assert!(!serde_json::to_string(&rows).unwrap().contains("data:image"));
    let mut facts = Vec::new();
    let mut members = Vec::new();
    for attachment in attachments {
        if let AttachmentSource::Asset { asset } = &attachment.source {
            let file = Path::new(&asset.url).file_name().unwrap();
            let bytes = std::fs::read(assets::asset_root(&projection).join(file)).unwrap();
            assert_eq!(
                bytes.len() as u64,
                asset.byte_len,
                "member exists before the published row is readable"
            );
            assert_eq!(asset.media_type, "image/webp");
            assert_eq!(&bytes[8..12], b"WEBP");
            assert_eq!(asset.digest, blake3::hash(&bytes).to_hex().to_string());
            for variant in asset.image_variants.as_ref().unwrap().values() {
                let name = Path::new(&variant.url)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap();
                let rendition = std::fs::read(assets::asset_root(&projection).join(name)).unwrap();
                assert_eq!(&rendition[8..12], b"WEBP");
                members.push((name.into(), rendition));
            }
            facts.push((
                asset.byte_len,
                attachment.dimensions.map(|d| (d.width, d.height)),
            ));
            members.push((file.to_owned(), bytes));
        }
    }
    assert!(!members.is_empty());
    let mut search = Vec::new();
    queries::visit_session_search_documents(store.conn(), &session, |document| {
        search.push(format!(
            "{} {} {} {} {}",
            document.prose, document.target, document.intent, document.change, document.tool
        ));
        Ok(())
    })
    .unwrap();
    assert!(
        search
            .iter()
            .all(|doc| !doc.contains("base64,") && !doc.contains("<image name="))
    );
    ledger.flush().unwrap();
    drop(ledger);
    drop(store);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(assets::asset_root(&projection)).unwrap();
    let rebuilt = temp.path().join("rebuilt.sqlite");
    let mut replay = Store::open_in_memory_for_rebuild_with_assets(&rebuilt).unwrap();
    rebuild_runtime(
        &CodexAdapter,
        &runtime,
        &ledger_dir,
        &mut replay,
        &instrument,
    )
    .unwrap();
    let replayed = queries::timeline_page(replay.conn(), &session, None, 100).unwrap();
    assert_eq!(replayed[0].timeline_id, rows[0].timeline_id);
    assert_eq!(replayed[0].body, rows[0].body);
    for (file, bytes) in members {
        assert_eq!(
            std::fs::read(assets::asset_root(&rebuilt).join(file)).unwrap(),
            bytes
        );
    }
    facts
}

#[test]
fn authored_images_materialize_before_publication_and_rebuild_from_the_ledger_alone() {
    let header = json!({"timestamp":"2026-09-09T00:00:00Z","type":"session_meta","payload":{"id":"image-session","timestamp":"2026-09-09T00:00:00Z","cwd":"/fixture/project","originator":"codex_exec","cli_version":"0.150.1","source":"cli","model_provider":"openai"}});
    let image = json!({"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII="});
    let message = json!({"timestamp":"2026-09-09T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[
        {"type":"input_text","text":"<image name=[Image #1] path=\"/gone/screenshot.png\">"},image,
        {"type":"input_text","text":"</image>"},{"type":"input_text","text":"Review [Image #1]."}, image
    ]}});
    let facts = exercise(&header.to_string(), &message.to_string());
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0], facts[1]);
    assert_eq!(facts[0].1, Some((1, 1)));
    assert!(facts[0].0 > 0);
}

/// Opt-in local corroboration. No private path/content is a committed fixture.
#[test]
#[ignore = "requires an explicitly selected local rollout and one-based message line"]
fn selected_local_image_record_replays_without_its_temporary_image_path() {
    let file = std::env::var("ROSETTA_IMAGE_ROLLOUT").expect("selected rollout path");
    let number: usize = std::env::var("ROSETTA_IMAGE_LINE")
        .expect("selected line")
        .parse()
        .unwrap();
    let mut lines = BufReader::new(std::fs::File::open(file).unwrap()).lines();
    let header = lines.next().unwrap().unwrap();
    let message = lines.nth(number - 2).unwrap().unwrap();
    println!(
        "Recovered image facts (bytes, dimensions): {:?}",
        exercise(&header, &message)
    );
}
