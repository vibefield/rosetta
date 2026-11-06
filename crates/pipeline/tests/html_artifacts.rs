//! A review page survives its temporary source and the entire projection.
use rosetta_adapter_codex::CodexAdapter;
use rosetta_capture::{Ledger, LedgerEntry, scan_ledger, verify_ledger};
use rosetta_core::{
    html_artifacts::{HTML_MAX_BYTES, HtmlArtifactAvailability},
    ids,
    rows::RuntimeRow,
};
use rosetta_pipeline::{
    IngestOptions, Instrument, RebuildOptions, apply_prepared_runtime, meta_ingest_into,
    prepare_runtime, rebuild_runtime_with_options, stream_backfill_into,
};
use rosetta_store::{
    Store, assets,
    html_artifacts::{self, ArtifactEntry, ArtifactListRequest, ListError},
};
use serde_json::json;
use std::{fs, io::Write, path::PathBuf};

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    transcript: PathBuf,
    page: PathBuf,
    db: PathBuf,
    runtime: RuntimeRow,
    store: Store,
    ledger: Ledger,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("codex");
        let transcript = root.join("sessions/2026/10/01/rollout-review.jsonl");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let page = temp.path().join("review.html");
        let db = temp.path().join("projection.sqlite");
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
        let header = json!({"timestamp":"2026-10-01T00:00:00Z","type":"session_meta","payload":{"id":"review-session","timestamp":"2026-10-01T00:00:00Z","cwd":temp.path(),"originator":"codex_exec","cli_version":"0.150.1","source":"cli","model_provider":"openai"}});
        fs::write(&transcript, format!("{header}\n")).unwrap();
        let store = Store::open(&db).unwrap();
        let ledger = Ledger::open(&temp.path().join("ledger"))
            .unwrap()
            .with_max_segment_bytes(512);
        Self {
            temp,
            root,
            transcript,
            page,
            db,
            runtime,
            store,
            ledger,
        }
    }
    fn present(&mut self, n: u32, name: &str, path: &str) {
        let marker = format!("visualize{}", json!({"path":path,"title":name}));
        let record = json!({"timestamp":format!("2026-10-01T00:00:{n:02}Z"),"type":"response_item","payload":{"type":"message","id":format!("reply-{n}"),"role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":marker}]}});
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .open(&self.transcript)
                .unwrap(),
            "{record}"
        )
        .unwrap();
    }
    fn present_page(&mut self, n: u32, name: &str) {
        let path = self.page.to_string_lossy().into_owned();
        self.present(n, name, &path);
    }
    fn ingest(&mut self) {
        self.ingest_with(&CodexAdapter);
    }
    fn ingest_with(&mut self, adapter: &dyn rosetta_adapter_support::AgentAdapter) {
        self.ingest_recovery(adapter, Default::default());
    }
    fn ingest_recovery(
        &mut self,
        adapter: &dyn rosetta_adapter_support::AgentAdapter,
        recovery: rosetta_pipeline::ArtifactRecovery,
    ) {
        let options = IngestOptions {
            artifact_recovery: recovery,
            audit_files_per_run: 0,
            ..Default::default()
        };
        let instrument = Instrument::new();
        let (_, files) = meta_ingest_into(
            adapter,
            &self.runtime,
            &self.root,
            &mut self.store,
            &options,
            &instrument,
            Some(&mut self.ledger),
        )
        .unwrap();
        let report = stream_backfill_into(
            adapter,
            &self.runtime,
            &self.root,
            &mut self.store,
            &files,
            &options,
            &instrument,
            Some(&mut self.ledger),
        )
        .unwrap();
        assert_eq!(report.hard_errors, 0);
        self.ledger.flush().unwrap();
    }
    fn list(&self) -> Vec<ArtifactEntry> {
        list(&self.store)
    }
}

const NATIVE_ARTIFACT: &str = "12345678-1234-4234-9234-123456789abc";
const PUBLISHED_URL: &str = "https://claude.ai/artifact/published-review";

fn claude_fixture() -> Fixture {
    let mut f = Fixture::new();
    fs::remove_dir_all(f.root.join("sessions")).unwrap();
    f.runtime.kind = "claude-code".into();
    f.runtime.id = ids::runtime_id("claude-code", f.root.to_str().unwrap());
    f.transcript = f.root.join("projects/synthetic/native.jsonl");
    fs::create_dir_all(f.transcript.parent().unwrap()).unwrap();
    fs::write(&f.transcript, "").unwrap();
    f
}

fn append(f: &Fixture, record: serde_json::Value) {
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(&f.transcript)
            .unwrap(),
        "{record}"
    )
    .unwrap();
}

fn claude_call(
    f: &Fixture,
    id: &str,
    name: &str,
    input: serde_json::Value,
    result: serde_json::Value,
    failed: bool,
) {
    append(
        f,
        json!({"type":"assistant","parentUuid":null,"isSidechain":false,"uuid":format!("open-{id}"),"timestamp":"2026-10-01T12:00:00Z","sessionId":"native","version":"2.1.283","cwd":f.temp.path(),"gitBranch":"main","userType":"external",
        "message":{"id":format!("response-{id}"),"type":"message","role":"assistant","model":"claude-opus-5","stop_reason":"tool_use","content":[{"type":"tool_use","id":id,"name":name,"input":input}]}}),
    );
    append(
        f,
        json!({"type":"user","parentUuid":format!("open-{id}"),"isSidechain":false,"uuid":format!("result-{id}"),"timestamp":"2026-10-01T12:00:01Z","sessionId":"native","version":"2.1.283","cwd":f.temp.path(),"gitBranch":"main","userType":"external",
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":"finished","is_error":failed}]},"toolUseResult":result}),
    );
}

fn claude_publish(f: &Fixture) {
    claude_call(
        f,
        "publish",
        "Artifact",
        json!({"file_path":f.page,"title":"Recovered review"}),
        json!({"artifact_id":NATIVE_ARTIFACT,"version":"2","url":PUBLISHED_URL,"path":f.page,"updated":false}),
        false,
    );
}

fn replay_recovery(f: &mut Fixture, expected: &[u8]) {
    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    let rows = f.list();
    assert_eq!(rows[0].publication.url.as_deref(), Some(PUBLISHED_URL));
    assert!(rows[0].recovered_from.is_some());
    fs::remove_dir_all(&f.root).unwrap();
    fs::remove_dir_all(assets::asset_root(&f.db)).unwrap();
    let db = f.temp.path().join("recovered-rebuild.sqlite");
    let mut rebuilt = Store::open_in_memory_for_rebuild_with_assets(&db).unwrap();
    rebuild_runtime_with_options(
        &ClaudeCodeAdapter,
        &f.runtime,
        &f.temp.path().join("ledger"),
        &mut rebuilt,
        &Instrument::new(),
        RebuildOptions {
            chunk_ops: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(list(&rebuilt), rows);
    assert_eq!(member(&db, &rows[0]), expected);
}

#[test]
fn recovers_native_backup_by_recorded_path_and_replays_without_the_backup() {
    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    let mut f = claude_fixture();
    let backup = f.root.join("file-history/native/backup@v3");
    fs::create_dir_all(backup.parent().unwrap()).unwrap();
    fs::write(&backup, b"<h1>Preserved backup</h1>").unwrap();
    append(
        &f,
        json!({"type":"file-history-snapshot","messageId":"snapshot","snapshot":{"messageId":"snapshot","timestamp":"2026-10-01T11:00:00Z","trackedFileBackups":{f.page.to_str().unwrap():{"backupFileName":"backup@v3","version":3,"backupTime":"2026-10-01T11:00:00Z"}}},"isSnapshotUpdate":false}),
    );
    claude_publish(&f);
    f.ingest_with(&ClaudeCodeAdapter);
    let row = f.list().remove(0);
    assert_eq!(member(&f.db, &row), b"<h1>Preserved backup</h1>");
    assert_eq!(row.recovered_from.as_deref(), backup.to_str());
    replay_recovery(&mut f, b"<h1>Preserved backup</h1>");
}

#[test]
fn successful_recorded_write_survives_a_checkpoint_and_failed_writes_do_not_recover() {
    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    for failed in [false, true] {
        let mut f = claude_fixture();
        let body = "<h1>Recorded source</h1>";
        claude_call(
            &f,
            "write",
            "Write",
            json!({"file_path":f.page,"content":body}),
            json!({"success":!failed}),
            failed,
        );
        f.ingest_with(&ClaudeCodeAdapter);
        assert!(
            f.list().is_empty(),
            "a Write is never an artifact presentation"
        );
        let checkpoints: Vec<String> = f
            .store
            .conn()
            .prepare("SELECT checkpoint FROM ingest_cursor WHERE checkpoint IS NOT NULL")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            checkpoints.iter().all(|c| !c.contains(body)),
            "checkpoint contains identifiers, not source HTML"
        );
        claude_publish(&f);
        f.ingest_with(&ClaudeCodeAdapter);
        let row = f.list().remove(0);
        assert_eq!(row.publication.url.as_deref(), Some(PUBLISHED_URL));
        if failed {
            assert_eq!(row.availability, HtmlArtifactAvailability::Missing);
            assert!(row.recovered_from.is_none());
        } else {
            assert_eq!(member(&f.db, &row), body.as_bytes());
            assert!(
                row.recovered_from
                    .as_deref()
                    .unwrap()
                    .starts_with("transcript:")
            );
            replay_recovery(&mut f, body.as_bytes());
        }
    }
}

#[test]
fn archive_identity_matches_renamed_html_but_conflicting_copies_are_not_guessed() {
    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    for conflicting in [false, true] {
        let mut f = claude_fixture();
        let archive = tempfile::tempdir().unwrap();
        fs::write(
            archive.path().join("renamed.html"),
            b"<h1>Archived review</h1>",
        )
        .unwrap();
        fs::write(
            archive.path().join("review.html"),
            b"<h1>Wrong basename-only file</h1>",
        )
        .unwrap();
        let mut manifest = format!(
            "| Name | File | Publication |\n|---|---|---|\n| Review | [page](renamed.html) | [old link](https://claude.ai/code/artifact/{NATIVE_ARTIFACT}) |\n"
        );
        if conflicting {
            fs::write(
                archive.path().join("other.html"),
                b"<h1>Another version</h1>",
            )
            .unwrap();
            manifest.push_str(&format!(
                "| Review | [other](other.html) | [published]({PUBLISHED_URL}) |\n"
            ));
        }
        fs::write(archive.path().join("README.md"), manifest).unwrap();
        claude_publish(&f);
        let mut recovery = rosetta_pipeline::ArtifactRecovery::default();
        recovery.search_roots.push(archive.path().to_path_buf());
        f.ingest_recovery(&ClaudeCodeAdapter, recovery);
        let row = f.list().remove(0);
        if conflicting {
            assert_eq!(row.availability, HtmlArtifactAvailability::Missing);
            assert_eq!(row.publication.url.as_deref(), Some(PUBLISHED_URL));
        } else {
            assert_eq!(member(&f.db, &row), b"<h1>Archived review</h1>");
            assert!(row.recovered_from.unwrap().ends_with("renamed.html"));
        }
    }
}

fn git_fixture(root: &std::path::Path, args: &[&str]) {
    let result = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn archived_readme_history_preserves_old_artifact_ids_and_their_matching_git_content() {
    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    let mut f = claude_fixture();
    let archive = tempfile::tempdir().unwrap();
    git_fixture(archive.path(), &["init", "-q"]);
    fs::write(
        archive.path().join("page.html"),
        b"<h1>Old publication</h1>",
    )
    .unwrap();
    fs::write(
        archive.path().join("README.md"),
        format!(
            "[page](page.html) [published](https://claude.ai/code/artifact/{NATIVE_ARTIFACT})\n"
        ),
    )
    .unwrap();
    git_fixture(archive.path(), &["add", "."]);
    git_fixture(archive.path(), &["commit", "-qm", "archive"]);
    fs::write(
        archive.path().join("page.html"),
        b"<h1>Different publication</h1>",
    )
    .unwrap();
    fs::write(
        archive.path().join("README.md"),
        "[page](page.html) [published](https://claude.ai/artifact/different-id)\n",
    )
    .unwrap();
    git_fixture(archive.path(), &["add", "."]);
    git_fixture(archive.path(), &["commit", "-qm", "republish"]);
    // The archive link remains useful even after its working copy is deleted.
    fs::remove_file(archive.path().join("page.html")).unwrap();
    claude_publish(&f);
    let mut recovery = rosetta_pipeline::ArtifactRecovery::default();
    recovery.search_roots.push(archive.path().to_path_buf());
    f.ingest_recovery(&ClaudeCodeAdapter, recovery);
    let row = f.list().remove(0);
    assert_eq!(member(&f.db, &row), b"<h1>Old publication</h1>");
    assert!(row.recovered_from.unwrap().starts_with("git:"));
    fs::remove_dir_all(archive.path()).unwrap();
    replay_recovery(&mut f, b"<h1>Old publication</h1>");
}

#[test]
fn a_deleted_tracked_source_recovers_from_git_and_never_from_a_basename_alone() {
    for tracked in [false, true] {
        let mut f = Fixture::new();
        if tracked {
            git_fixture(f.temp.path(), &["init", "-q"]);
            fs::write(&f.page, b"<h1>Tracked review</h1>").unwrap();
            git_fixture(f.temp.path(), &["add", "review.html"]);
            git_fixture(f.temp.path(), &["commit", "-qm", "review"]);
            fs::remove_file(&f.page).unwrap();
        }
        let elsewhere = f.temp.path().join("other");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("review.html"), "<h1>Unrelated page</h1>").unwrap();
        f.present_page(1, "Review");
        f.ingest();
        let row = f.list().remove(0);
        if tracked {
            assert_eq!(member(&f.db, &row), b"<h1>Tracked review</h1>");
            assert!(row.recovered_from.unwrap().starts_with("git:"));
        } else {
            assert_eq!(row.availability, HtmlArtifactAvailability::Missing);
        }
    }
}
fn list(store: &Store) -> Vec<ArtifactEntry> {
    html_artifacts::list(store.conn(), &ArtifactListRequest::default(), false)
        .unwrap()
        .entries
}
fn member(db: &std::path::Path, row: &ArtifactEntry) -> Vec<u8> {
    fs::read(assets::asset_root(db).join(format!("{}.html", row.asset.as_ref().unwrap().digest)))
        .unwrap()
}

#[test]
fn captures_versions_and_rebuilds_from_ledger_after_the_source_and_assets_are_gone() {
    let mut f = Fixture::new();
    fs::write(&f.page, b"<h1>First review</h1>").unwrap();
    f.present_page(1, "First review");
    f.ingest();
    let first = f.list().remove(0);
    assert_eq!(first.publication.filename, "review.html");
    assert_eq!(first.availability, HtmlArtifactAvailability::Captured);
    assert_eq!(member(&f.db, &first), b"<h1>First review</h1>");
    fs::write(&f.page, b"<h1>Second review</h1>").unwrap();
    f.present_page(2, "Second review");
    f.ingest();
    let current = f.list();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].presentations, 2);
    assert_ne!(current[0].asset, first.asset);
    let versions = html_artifacts::list(
        f.store.conn(),
        &ArtifactListRequest {
            artifact_id: Some(first.publication.artifact_id.clone()),
            ..Default::default()
        },
        false,
    )
    .unwrap()
    .entries;
    assert_eq!(versions.len(), 2);
    assert_eq!(member(&f.db, &versions[1]), b"<h1>First review</h1>");
    assert_eq!(
        rosetta_store::timeline_meta::read(f.store.conn(), &first.publication.session_id)
            .unwrap()
            .artifacts
            .len(),
        1
    );
    let before = verify_ledger(&f.temp.path().join("ledger")).unwrap();
    assert_eq!(before.tally.documents, 2);
    f.ingest();
    assert_eq!(
        verify_ledger(&f.temp.path().join("ledger"))
            .unwrap()
            .tally
            .documents,
        2,
        "quiet scans do not duplicate captures"
    );
    fs::remove_file(&f.page).unwrap();
    fs::remove_dir_all(&f.root).unwrap();
    fs::remove_dir_all(assets::asset_root(&f.db)).unwrap();
    let cache = f.temp.path().join("election.json");
    let rebuilt_db = f.temp.path().join("rebuilt.sqlite");
    let mut rebuilt = Store::open_in_memory_for_rebuild_with_assets(&rebuilt_db).unwrap();
    rebuild_runtime_with_options(
        &CodexAdapter,
        &f.runtime,
        &f.temp.path().join("ledger"),
        &mut rebuilt,
        &Instrument::new(),
        RebuildOptions {
            chunk_ops: 1,
            election_cache: Some(cache.clone()),
            prefetch_ledger: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(list(&rebuilt), current);
    assert_eq!(member(&rebuilt_db, &current[0]), b"<h1>Second review</h1>");
    let prepared = prepare_runtime(
        &CodexAdapter,
        &f.runtime,
        &f.temp.path().join("ledger"),
        &Instrument::new(),
        RebuildOptions {
            election_cache: Some(cache),
            ..Default::default()
        },
    )
    .unwrap();
    let prepared_db = f.temp.path().join("prepared.sqlite");
    let mut replay = Store::open_in_memory_for_rebuild_with_assets(&prepared_db).unwrap();
    apply_prepared_runtime(
        &CodexAdapter,
        &f.runtime,
        prepared,
        &mut replay,
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(list(&replay), current);
    assert_eq!(member(&prepared_db, &current[0]), b"<h1>Second review</h1>");
}

#[test]
fn missing_large_and_encoded_pages_are_honest_and_pages_are_filterable() {
    let mut f = Fixture::new();
    f.present(1, "Missing design", "/missing/synthetic-review.html");
    let big = f.temp.path().join("large.html");
    fs::File::create(&big)
        .unwrap()
        .set_len(HTML_MAX_BYTES as u64 + 1)
        .unwrap();
    f.present(2, "Large design", big.to_str().unwrap());
    let mut utf16 = vec![0xff, 0xfe];
    for unit in "<h1>Review π</h1>".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(&f.page, &utf16).unwrap();
    f.present_page(3, "Encoded design");
    f.ingest();
    let rows = f.list();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].availability, HtmlArtifactAvailability::Captured);
    assert_eq!(member(&f.db, &rows[0]), "<h1>Review π</h1>".as_bytes());
    assert_eq!(rows[1].availability, HtmlArtifactAvailability::TooLarge);
    assert_eq!(rows[2].availability, HtmlArtifactAvailability::Missing);
    let scan = scan_ledger(&f.temp.path().join("ledger")).unwrap();
    assert!(
        scan.entries
            .iter()
            .any(|e| matches!(e,LedgerEntry::Document {document,..} if document.bytes==utf16)),
        "ledger preserves exact encoded bytes"
    );
    let request = ArtifactListRequest {
        limit: Some(1),
        ..Default::default()
    };
    let first = html_artifacts::list(f.store.conn(), &request, false).unwrap();
    let next = html_artifacts::list(
        f.store.conn(),
        &ArtifactListRequest {
            cursor: first.next_cursor.clone(),
            ..request.clone()
        },
        false,
    )
    .unwrap();
    assert_eq!(next.entries[0], rows[1]);
    let filtered = html_artifacts::list(
        f.store.conn(),
        &ArtifactListRequest {
            q: Some("Missing".into()),
            agent: Some("codex".into()),
            ..Default::default()
        },
        false,
    )
    .unwrap();
    assert_eq!(filtered.entries, vec![rows[2].clone()]);
    f.present(4, "Another missing design", "/missing/second.html");
    f.ingest();
    assert!(matches!(
        html_artifacts::list(
            f.store.conn(),
            &ArtifactListRequest {
                cursor: first.next_cursor,
                ..request
            },
            false
        ),
        Err(ListError::Expired)
    ));
}

#[test]
fn a_replaced_transcript_removes_publications_owned_by_its_old_generation() {
    let mut f = Fixture::new();
    fs::write(&f.page, b"<h1>Old</h1>").unwrap();
    f.present_page(1, "Old");
    f.ingest();
    assert_eq!(f.list().len(), 1);
    let header = fs::read_to_string(&f.transcript)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_owned();
    fs::write(&f.transcript, format!("{header}\n")).unwrap();
    f.ingest();
    assert!(f.list().is_empty());
    let rebuilt_db = f.temp.path().join("replacement.sqlite");
    let mut rebuilt = Store::open_in_memory_for_rebuild_with_assets(&rebuilt_db).unwrap();
    rebuild_runtime_with_options(
        &CodexAdapter,
        &f.runtime,
        &f.temp.path().join("ledger"),
        &mut rebuilt,
        &Instrument::new(),
        RebuildOptions::default(),
    )
    .unwrap();
    assert!(
        list(&rebuilt).is_empty(),
        "stale captured bytes cannot resurrect an old publication"
    );
}

#[test]
fn legacy_ledger_rebuild_never_substitutes_todays_file_for_missing_capture_evidence() {
    let mut f = Fixture::new();
    fs::write(&f.page, b"<h1>Current file is not historical evidence</h1>").unwrap();
    f.present_page(1, "Old presentation");
    f.ingest();
    let legacy_path = f.temp.path().join("legacy-ledger");
    let mut legacy = Ledger::open(&legacy_path).unwrap();
    for entry in scan_ledger(&f.temp.path().join("ledger")).unwrap().entries {
        match entry {
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } => legacy
                .append_raw(&source, generation, [(start_offset, payload.as_slice())])
                .unwrap(),
            LedgerEntry::Meta {
                schema_version,
                payload,
            } => legacy.append_meta(schema_version, &payload).unwrap(),
            LedgerEntry::GenerationComplete {
                source,
                generation,
                normalizer_version,
                normalized_clean,
            } => legacy
                .complete_generation(&source, generation, normalizer_version, normalized_clean)
                .unwrap(),
            LedgerEntry::Document { .. } => {}
        }
    }
    legacy.flush().unwrap();
    let db = f.temp.path().join("legacy.sqlite");
    let mut rebuilt = Store::open_in_memory_for_rebuild_with_assets(&db).unwrap();
    rebuild_runtime_with_options(
        &CodexAdapter,
        &f.runtime,
        &legacy_path,
        &mut rebuilt,
        &Instrument::new(),
        RebuildOptions::default(),
    )
    .unwrap();
    let rows = list(&rebuilt);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].availability, HtmlArtifactAvailability::NotCaptured);
    assert!(rows[0].asset.is_none());
    assert!(rows[0].captured_at.is_none());
}

#[test]
fn a_files_map_string_names_the_page_file_which_is_captured_and_rebuilds_from_the_ledger() {
    // The RO-34a probe (2026-11-05): a files-map string is a source path, as
    // the Artifact tool's schema says, so the page is the file it names and is
    // observed like any `file_path`. Normalizer 38 stored the string's own
    // text as the page; this test replaces the one that pinned that reading.
    use rosetta_adapter_claude_code::ClaudeCodeAdapter;
    let mut f = claude_fixture();
    let html = "<h1>Review π</h1><button>Compare designs</button>";
    fs::write(&f.page, html).unwrap();
    claude_call(
        &f,
        "publish",
        "Artifact",
        json!({"action":"publish","title":"Review","description":"Two checkout flows, side by side",
            "files":{"index.html":f.page,"app.js":"app.js","logo.png":{"from":"logo.png"},"old.css":null}}),
        json!({"artifact_id":NATIVE_ARTIFACT,"version":1,"url":PUBLISHED_URL,"updated":false}),
        false,
    );
    f.ingest_with(&ClaudeCodeAdapter);
    let rows = f.list();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.availability, HtmlArtifactAvailability::Captured);
    assert_eq!(row.publication.path.as_deref(), f.page.to_str());
    assert_eq!(row.publication.filename, "review.html");
    assert!(row.captured_at.is_some(), "the named file was observed");
    assert_eq!(
        row.publication.description.as_deref(),
        Some("Two checkout flows, side by side")
    );
    assert_eq!(
        row.publication.dropped_files,
        Some(2),
        "app.js and logo.png were named and not kept; a null removes and names nothing"
    );
    assert_eq!(member(&f.db, row), html.as_bytes());
    fs::remove_file(&f.page).unwrap();
    fs::remove_dir_all(&f.root).unwrap();
    fs::remove_dir_all(assets::asset_root(&f.db)).unwrap();
    let db = f.temp.path().join("files-map-rebuilt.sqlite");
    let mut rebuilt = Store::open_in_memory_for_rebuild_with_assets(&db).unwrap();
    rebuild_runtime_with_options(
        &ClaudeCodeAdapter,
        &f.runtime,
        &f.temp.path().join("ledger"),
        &mut rebuilt,
        &Instrument::new(),
        RebuildOptions {
            chunk_ops: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        list(&rebuilt),
        rows,
        "description and dropped_files rebuild too"
    );
    assert_eq!(member(&db, &rows[0]), html.as_bytes());
}
