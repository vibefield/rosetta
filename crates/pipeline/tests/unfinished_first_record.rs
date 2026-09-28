//! A transcript whose first record its writer is still writing is not yet a
//! session. The metadata phase withholds it as unfinished — never as
//! unidentified, which names a complete record no skim recognizes — and
//! admits it once the record is whole.

use std::path::{Path, PathBuf};

use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_core::ids;
use rosetta_core::rows::RuntimeRow;
use rosetta_pipeline::{IngestOptions, Instrument, MetaReport, SessionFile, meta_ingest};
use rosetta_store::Store;
use tempfile::TempDir;

const FIXTURE_ID: &str = "01a036c5-6e18-73f3-8c07-d4430763840b";

/// The recorded basic-chat rollout.
fn fixture_rollout() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../groundtruth/codex/fixtures/0.149.1/basic-chat/codex-home/sessions/2026/08/24/\
             rollout-2026-08-24T19-35-03-{FIXTURE_ID}.jsonl"
    )))
    .unwrap()
}

fn runtime() -> RuntimeRow {
    RuntimeRow {
        id: ids::runtime_id(CodexAdapter.kind(), &CodexAdapter.layout().data_root),
        kind: CodexAdapter.kind().to_owned(),
        data_root: CodexAdapter.layout().data_root.clone(),
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

fn meta(root: &Path, store: &mut Store) -> (MetaReport, Vec<SessionFile>) {
    meta_ingest(
        &CodexAdapter,
        &runtime(),
        root,
        store,
        &IngestOptions::default(),
        &Instrument::new(),
    )
    .unwrap()
}

fn rollout(root: &Path, name: &str) -> PathBuf {
    let path = root.join("sessions/2026/08/24").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

#[test]
fn a_rollout_still_writing_its_first_record_is_not_yet_a_session() {
    let home = TempDir::new().unwrap();
    let root = home.path().join(".codex");
    let text = fixture_rollout();
    let first_record = text.split_inclusive('\n').next().unwrap();

    // One rollout is half way through its opening record; another was just
    // created. Neither is a session yet, and neither is unidentified.
    let writing = rollout(
        &root,
        &format!("rollout-2026-08-24T19-35-03-{FIXTURE_ID}.jsonl"),
    );
    std::fs::write(&writing, &first_record.as_bytes()[..first_record.len() / 2]).unwrap();
    let created = rollout(&root, "rollout-2026-08-24T20-00-00-created.jsonl");
    std::fs::write(&created, "").unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let (report, files) = meta(&root, &mut store);
    assert_eq!(
        (report.files_unfinished, report.files_unidentified),
        (2, 0),
        "{report:?}"
    );
    assert!(files.is_empty());

    // The whole opening record, without the rest of the rollout, makes the
    // first a session — before its delimiter lands too, as a complete record
    // always has; the stream reads it once delimited.
    for opening in [first_record.trim_end_matches('\n'), first_record] {
        std::fs::write(&writing, opening).unwrap();
        let (report, files) = meta(&root, &mut store);
        assert_eq!(
            (report.files_unfinished, report.files_unidentified),
            (1, 0),
            "{report:?}"
        );
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].native_session_id, FIXTURE_ID);
    }

    // A complete opening record no skim identifies is unidentified.
    std::fs::write(
        &created,
        "{\"timestamp\":\"2026-08-25T02:35:03.577Z\",\"type\":\"event_msg\",\
         \"payload\":{\"type\":\"task_started\"}}\n",
    )
    .unwrap();
    let (report, files) = meta(&root, &mut store);
    assert_eq!(
        (report.files_unfinished, report.files_unidentified),
        (0, 1),
        "{report:?}"
    );
    assert_eq!(files.len(), 1);
}
