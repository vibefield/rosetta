use super::*;

use rosetta_capture::{Ledger, LedgerEntry, SourceDef, scan_ledger};
use rosetta_pipeline::{IngestOptions, Instrument, PipeCheckpoint, meta_ingest_into};
use rosetta_store::{Store, queries};

const PRIVATE_CWD: &str = "/private/tmp/rosetta-farm/basic-chat/project";

fn exclude(bench: &Bench, folders: &[&str]) {
    let mut file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&bench.enroll).unwrap()).unwrap();
    file["exclude_cwds"] = serde_json::json!(folders);
    std::fs::write(&bench.enroll, serde_json::to_vec(&file).unwrap()).unwrap();
}

fn transcript(root: &Path, native: &str) -> PathBuf {
    let filename = format!("{native}.jsonl");
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().unwrap() == filename.as_str() {
                return path;
            }
        }
    }
    panic!("missing transcript {native}");
}

fn raw_records(dir: &Path, native: &str) -> Vec<(u64, u64, Vec<u8>)> {
    scan_ledger(dir)
        .unwrap()
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            LedgerEntry::Raw {
                source,
                generation,
                start_offset,
                payload,
            } if source.native_session_id == native => Some((generation, start_offset, payload)),
            _ => None,
        })
        .collect()
}

fn append_private(bench: &Bench) {
    let line = serde_json::json!({
        "type":"user", "uuid":"excluded-offline-append", "sessionId":NATIVE_SESSION,
        "cwd":PRIVATE_CWD, "timestamp":"2026-10-29T12:00:00Z",
        "message":{"role":"user","content":"excludedofflinesentinel"}
    })
    .to_string()
        + "\n";
    std::fs::OpenOptions::new()
        .append(true)
        .open(transcript(&bench.home.join(".claude"), NATIVE_SESSION))
        .unwrap()
        .write_all(line.as_bytes())
        .unwrap();
}

fn metadata_serve(bench: &Bench) -> ChildGuard {
    let mut child = bench
        .command(
            "serve",
            &[
                "--enroll",
                bench.enroll.to_str().unwrap(),
                "--addr",
                "127.0.0.1:0",
                "--stop-after-meta",
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let guard = ChildGuard(child);
    let (send, recv) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = send.send(line);
        }
    });
    loop {
        let line = recv.recv_timeout(Duration::from_secs(20)).unwrap();
        if line.starts_with("metadata ready") {
            return guard;
        }
    }
}

#[test]
fn every_ingest_entry_point_applies_the_enrollment_folder_policy() {
    for mode in ["serve", "metadata", "ingest", "watch"] {
        let bench = Bench::new();
        exclude(&bench, &[PRIVATE_CWD]);
        let _metadata = if mode == "metadata" {
            Some(metadata_serve(&bench))
        } else {
            let output = match mode {
                "serve" => bench.serve_once(&[]),
                "watch" => bench
                    .command(
                        "watch",
                        &["--enroll", bench.enroll.to_str().unwrap(), "--ticks", "0"],
                    )
                    .output()
                    .unwrap(),
                _ => bench
                    .command("ingest", &["--enroll", bench.enroll.to_str().unwrap()])
                    .output()
                    .unwrap(),
            };
            assert_success(&output);
            None
        };
        assert!(!bench.has_session(NATIVE_SESSION), "{mode}");
        assert!(bench.has_session(KIM_SESSION), "{mode}");
        assert!(
            raw_records(&bench.native_ledger_dir(), NATIVE_SESSION).is_empty(),
            "{mode}"
        );
    }
    let bench = Bench::new();
    exclude(&bench, &["/private/tmp/rosetta-farm/tool-use"]);
    assert_success(&bench.serve_once(&[]));
    assert!(bench.has_session(NATIVE_SESSION));
    assert!(!bench.has_session(KIM_SESSION));
}

#[test]
fn warm_and_resumed_starts_keep_retired_history_without_capturing_offline_changes() {
    for resumed in [false, true] {
        let bench = Bench::new();
        assert_success(&bench.serve_once(&[]));
        let before = raw_records(&bench.native_ledger_dir(), NATIVE_SESSION);
        assert!(!before.is_empty());
        append_private(&bench);
        exclude(&bench, &[PRIVATE_CWD]);
        if resumed {
            bench.stage_cold_generation(true);
        }
        assert_success(&bench.serve_once(&[]));
        assert!(bench.has_session(NATIVE_SESSION));
        assert!(bench.has_session(KIM_SESSION));
        assert_eq!(
            raw_records(&bench.native_ledger_dir(), NATIVE_SESSION),
            before
        );
        assert_success(
            &bench
                .command("rebuild", &["--enroll", bench.enroll.to_str().unwrap()])
                .output()
                .unwrap(),
        );
        assert!(
            bench.has_session(NATIVE_SESSION),
            "retire keeps archived history through rebuild"
        );
        exclude(&bench, &[]);
        assert_success(&bench.serve_once(&[]));
        assert!(raw_records(&bench.native_ledger_dir(), NATIVE_SESSION).iter().any(|(_, _, raw)| String::from_utf8_lossy(raw).contains("excludedofflinesentinel")));
    }
}

#[test]
fn retiring_an_interrupted_first_generation_preserves_its_fence_through_rebuild_and_readmission() {
    let bench = Bench::new();
    assert_success(&bench.serve_once(&[]));
    let root = bench.home.join(".claude");
    let original = transcript(&root, NATIVE_SESSION);
    let native = "77777777-7777-4777-8777-777777777777";
    let path = original.with_file_name(format!("{native}.jsonl"));
    let contents = std::fs::read_to_string(&original)
        .unwrap()
        .replace(NATIVE_SESSION, native);
    std::fs::write(&path, &contents).unwrap();
    let relative = path
        .strip_prefix(&root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    {
        let mut store = Store::open(&bench.db).unwrap();
        let runtime = queries::runtime(store.conn(), &bench.native_id())
            .unwrap()
            .unwrap();
        let mut ledger = Ledger::open(&bench.native_ledger_dir()).unwrap();
        let (_, files) = meta_ingest_into(
            &rosetta_adapter_claude_code::ClaudeCodeAdapter,
            &runtime,
            &root,
            &mut store,
            &IngestOptions::default(),
            &Instrument::new(),
            Some(&mut ledger),
        )
        .unwrap();
        let file = files
            .iter()
            .find(|file| file.native_session_id == native)
            .unwrap();
        let source = SourceDef {
            ledger_only: false,
            relative_path: relative.clone(),
            stream_id: file.stream_id.clone(),
            session_id: file.session_id.clone(),
            native_session_id: native.into(),
            agent_version: file.agent_version.clone(),
        };
        // A durable first batch, followed by process exit before the seal.
        ledger
            .append_raw(&source, 7, [(0, contents.lines().next().unwrap())])
            .unwrap();
        ledger.flush().unwrap();
    }
    bench.stage_cold_generation(true);
    exclude(&bench, &[PRIVATE_CWD]);
    assert_success(&bench.serve_once(&[]));
    assert_eq!(raw_records(&bench.native_ledger_dir(), native).len(), 1);
    assert_success(
        &bench
            .command("rebuild", &["--enroll", bench.enroll.to_str().unwrap()])
            .output()
            .unwrap(),
    );
    {
        let store = Store::open(&bench.db).unwrap();
        let cursor = queries::cursor_state(store.conn(), &bench.native_id(), &relative)
            .unwrap()
            .unwrap();
        assert_eq!(cursor.active_generation, 0);
        assert_eq!(
            cursor.highest_generation, 0,
            "retirement must not leave staged work blocking publication"
        );
        assert_eq!(
            PipeCheckpoint::decode(&cursor.checkpoint)
                .unwrap()
                .source_generation,
            7
        );
    }
    // A different exclusion does not excuse this incomplete archive.
    exclude(&bench, &["/unrelated"]);
    let refused = bench
        .command("rebuild", &["--enroll", bench.enroll.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("no complete-generation marker"));
    exclude(&bench, &[]);
    assert_success(&bench.serve_once(&[]));
    let captured = raw_records(&bench.native_ledger_dir(), native);
    assert!(captured.iter().any(|(generation, _, _)| *generation > 7));
    assert_eq!(
        captured
            .iter()
            .filter(|(generation, _, _)| *generation == 7)
            .count(),
        1
    );
    assert_success(
        &bench
            .command("rebuild", &["--enroll", bench.enroll.to_str().unwrap()])
            .output()
            .unwrap(),
    );
    assert!(bench.has_session(native));
}

#[test]
fn a_relative_exclusion_refuses_startup_before_binding() {
    let bench = Bench::new();
    exclude(&bench, &["~/client"]);
    let output = bench.serve_once(&["--json"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exclude_cwds[0]"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\"event\":\"ready\""));
    assert!(!bench.db.exists());
}

#[test]
fn retiring_a_staged_source_does_not_block_a_disk_cold_load() {
    let bench = Bench::new();
    drop(metadata_serve(&bench));
    {
        let mut store = Store::open_progressive(&bench.db).unwrap();
        let runtime = queries::runtime(store.conn(), &bench.native_id())
            .unwrap()
            .unwrap();
        let (_, files) = meta_ingest_into(
            &rosetta_adapter_claude_code::ClaudeCodeAdapter,
            &runtime,
            &bench.home.join(".claude"),
            &mut store,
            &IngestOptions::default(),
            &Instrument::new(),
            None,
        )
        .unwrap();
        let file = files
            .iter()
            .find(|file| file.native_session_id == NATIVE_SESSION)
            .unwrap();
        rosetta_pipeline::reserve_unsealed_generation(
            &rosetta_adapter_claude_code::ClaudeCodeAdapter,
            &runtime,
            &mut store,
            file,
            7,
            &Instrument::new(),
        )
        .unwrap();
        assert!(
            store.finish_progressive_load().is_err(),
            "an admitted unfinished source still blocks publication"
        );
    }
    exclude(&bench, &[PRIVATE_CWD]);
    assert_success(&bench.serve_once(&[]));
    assert!(bench.has_session(NATIVE_SESSION));
    assert!(bench.has_session(KIM_SESSION));
    assert!(raw_records(&bench.native_ledger_dir(), NATIVE_SESSION).is_empty());
    exclude(&bench, &[]);
    assert_success(&bench.serve_once(&[]));
    assert!(
        raw_records(&bench.native_ledger_dir(), NATIVE_SESSION)
            .iter()
            .all(|(generation, _, _)| *generation > 7)
    );
}
