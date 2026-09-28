//! Failures keep the serial walk's semantics whatever the pool does: the
//! first failure in path order wins, the ledger holds exactly the serial
//! prefix, cancellation returns and joins the workers, and an interrupted
//! pooled ledger resumes like the serial one.

use std::any::Any;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::{
    AgentAdapter, FileStat, GroundTruthDoc, LineOutcome, NormalizeCtx, SkimResult, SnapshotPlan,
    StreamDriver,
};
use rosetta_capture::{Ledger, SegmentCodec};
use tempfile::TempDir;

use super::{
    Config, SERIAL, assert_same_dump, capture, capture_into, claude_home, claude_line, cold_store,
    configs, dump, ledger_files, session_records, tiny_batches,
};
use crate::progress::ColdProgressObserver;
use crate::{
    IngestOptions, Instrument, PipelineError, RebuildOptions, SessionFile, discover,
    meta_ingest_inventory_into, rebuild_runtime_with_options, reserve_unsealed_generation,
    stream_backfill_into,
};

/// Claude Code, except the record carrying the marker fails its step: the
/// checkpoint encoder refuses the state it poisoned, or it panics.
struct Faulty {
    marker: &'static str,
    panic: bool,
}

struct Poisoned;

impl AgentAdapter for Faulty {
    fn kind(&self) -> &'static str {
        ClaudeCodeAdapter.kind()
    }

    fn layout(&self) -> &GroundTruthDoc {
        ClaudeCodeAdapter.layout()
    }

    fn stream_driver(&self, stream_id: &str) -> Option<StreamDriver> {
        ClaudeCodeAdapter.stream_driver(stream_id)
    }

    fn normalizer_version(&self) -> u32 {
        ClaudeCodeAdapter.normalizer_version()
    }

    fn snapshot_plan(&self, stream_id: &str) -> Option<SnapshotPlan> {
        ClaudeCodeAdapter.snapshot_plan(stream_id)
    }

    fn snapshot_session(&self, stream_id: &str, row: &serde_json::Value) -> SkimResult {
        ClaudeCodeAdapter.snapshot_session(stream_id, row)
    }

    fn document_sessions(
        &self,
        stream_id: &str,
        stat: &FileStat,
        document: &[u8],
    ) -> Vec<SkimResult> {
        ClaudeCodeAdapter.document_sessions(stream_id, stat, document)
    }

    fn document_size_limit(&self, stream_id: &str, pipeline_limit: usize) -> usize {
        ClaudeCodeAdapter.document_size_limit(stream_id, pipeline_limit)
    }

    fn document_records(
        &self,
        stream_id: &str,
        native_session_id: &str,
        document: &[u8],
    ) -> Vec<Vec<u8>> {
        ClaudeCodeAdapter.document_records(stream_id, native_session_id, document)
    }

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        ClaudeCodeAdapter.init_state()
    }

    fn checkpoint_state(
        &self,
        state: Option<&(dyn Any + Send)>,
    ) -> Result<Option<serde_json::Value>, String> {
        if state.is_some_and(|state| state.is::<Poisoned>()) {
            return Err("injected capture failure".to_string());
        }
        ClaudeCodeAdapter.checkpoint_state(state)
    }

    fn restore_state(
        &self,
        checkpoint: Option<&serde_json::Value>,
    ) -> Result<Option<Box<dyn Any + Send>>, String> {
        ClaudeCodeAdapter.restore_state(checkpoint)
    }

    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        ClaudeCodeAdapter.skim(stream_id, stat, head, tail)
    }

    fn wants_skim_fallback(&self, stream_id: &str, skim: &SkimResult) -> bool {
        ClaudeCodeAdapter.wants_skim_fallback(stream_id, skim)
    }

    fn absorb_skim_fallback_record(
        &self,
        stream_id: &str,
        record: &[u8],
        skim: &mut SkimResult,
    ) -> bool {
        ClaudeCodeAdapter.absorb_skim_fallback_record(stream_id, record, skim)
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        let marker = self.marker.as_bytes();
        if line.windows(marker.len()).any(|window| window == marker) {
            assert!(!self.panic, "injected capture panic");
            ctx.state = Some(Box::new(Poisoned));
            return LineOutcome::parsed(Vec::new());
        }
        ClaudeCodeAdapter.ingest_line(ctx, stream_id, line)
    }
}

const MARKER: &str = "FAIL-HERE";

/// Eight sessions; the fifth record of `s4` carries the marker, so with
/// two-record batches `s4` commits two batches before its third fails, while
/// the pool has stepped later sources ahead.
fn faulty_home() -> TempDir {
    let sessions: Vec<(String, Vec<String>)> = (0..8)
        .map(|index| {
            let session = format!("s{index}");
            let mut records = session_records(&session, 12, "hello");
            if index == 4 {
                records[4] = claude_line(&session, "s4-marker", MARKER);
            }
            (session, records)
        })
        .collect();
    claude_home(&sessions, None)
}

/// A failure at one source, however the pool schedules: the same error, the
/// same emitted sources and planner cursors, and a ledger that is byte for
/// byte the serial prefix.
fn assert_failure_matches(panic: bool) {
    let adapter: &'static dyn AgentAdapter = Box::leak(Box::new(Faulty {
        marker: MARKER,
        panic,
    }));
    let home = faulty_home();
    let opts = tiny_batches(2);
    let serial = capture(adapter, home.path(), &opts, SERIAL);
    let expected = if panic {
        "panicked: injected capture panic"
    } else {
        "injected capture failure"
    };
    assert!(
        serial.result.as_ref().unwrap_err().contains(expected),
        "{:?}",
        serial.result
    );
    assert!(!serial.emitted.is_empty(), "sources before s4 were emitted");
    for config in &configs()[1..] {
        let pooled = capture(adapter, home.path(), &opts, *config);
        let label = format!("failure (panic: {panic}) {config:?}");
        let error = pooled.result.as_ref().unwrap_err();
        if panic {
            assert!(
                error.contains("s4.jsonl panicked: injected capture panic"),
                "{label}: {error}"
            );
        } else {
            assert_eq!(pooled.result, serial.result, "{label}");
        }
        assert_eq!(pooled.cursors, serial.cursors, "{label}: planner cursors");
        assert_eq!(pooled.emitted, serial.emitted, "{label}: closed sources");
        assert!(pooled.ledger == serial.ledger, "{label}: ledger bytes");
    }
}

#[test]
fn an_error_at_one_source_leaves_the_serial_prefix() {
    assert_failure_matches(false);
}

#[test]
fn a_panic_at_one_source_leaves_the_serial_prefix() {
    assert_failure_matches(true);
}

/// Cancels the build at the sixth batch any worker reads.
#[derive(Debug)]
struct CancelAt {
    cancel: Arc<AtomicBool>,
    batches: AtomicU64,
}

impl ColdProgressObserver for CancelAt {
    fn source_bytes(&self, _file: &SessionFile, _bytes: u64) {
        if self.batches.fetch_add(1, Ordering::SeqCst) == 5 {
            self.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn reading_finished(&self) {}
}

#[test]
fn cancelling_a_pooled_capture_returns_and_joins_its_workers() {
    let home = faulty_home();
    for config in configs() {
        let (done, finished) = mpsc::channel();
        let path = home.path().to_path_buf();
        std::thread::spawn(move || {
            let cancel = Arc::new(AtomicBool::new(false));
            let opts = IngestOptions {
                cancel: Some(cancel.clone()),
                progress: Some(Arc::new(CancelAt {
                    cancel,
                    batches: AtomicU64::new(0),
                })),
                ..tiny_batches(2)
            };
            let _ = done.send(capture(&ClaudeCodeAdapter, &path, &opts, config).result);
        });
        let result = finished
            .recv_timeout(Duration::from_secs(120))
            .unwrap_or_else(|_| panic!("{config:?}: a cancelled capture never returned"));
        assert_eq!(
            result,
            Err(PipelineError::Cancelled.to_string()),
            "{config:?}"
        );
    }
}

/// What a resumed fresh serve does with an interrupted staged ledger: replay
/// its sealed generations, settle the roster from current metadata, reserve
/// every unsealed generation, then finish from the sources.
fn resume(home: &Path, ledger_dir: &Path, opts: &IngestOptions) -> BTreeMap<String, Vec<String>> {
    let adapter = &ClaudeCodeAdapter;
    let instrument = Instrument::new();
    let discovery = discover(&[adapter], home, opts, &instrument)
        .unwrap()
        .pop()
        .unwrap();
    let mut runtime = discovery.runtime;
    runtime.enabled = true;
    runtime.discovered_at = 1;
    let mut store = cold_store();
    let unsealed = if ledger_files(ledger_dir).is_empty() {
        BTreeMap::new()
    } else {
        let replayed = rebuild_runtime_with_options(
            adapter,
            &runtime,
            ledger_dir,
            &mut store,
            &instrument,
            RebuildOptions {
                flush_reconciliation: false,
                allow_unsealed_sources: true,
                ..RebuildOptions::default()
            },
        )
        .unwrap();
        assert_eq!(replayed.hard_errors, 0);
        replayed.unsealed_sources
    };
    let mut ledger = Ledger::open(ledger_dir)
        .unwrap()
        .with_codec(SegmentCodec::Plain)
        .with_max_segment_bytes(4 * 1024);
    let (_, roster) = meta_ingest_inventory_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut store,
        &discovery.inventory,
        discovery.walk_truncated,
        opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    for (path, (generation, _)) in &unsealed {
        let file = roster
            .iter()
            .find(|file| file.relative_path == *path)
            .unwrap();
        reserve_unsealed_generation(
            adapter,
            &runtime,
            &mut store,
            file,
            *generation,
            &instrument,
        )
        .unwrap();
    }
    stream_backfill_into(
        adapter,
        &runtime,
        &discovery.root,
        &mut store,
        &roster,
        opts,
        &instrument,
        Some(&mut ledger),
    )
    .unwrap();
    ledger.flush().unwrap();
    store.finish_bulk_load().unwrap();
    dump(&store)
}

#[test]
fn an_interrupted_pooled_ledger_resumes_like_the_serial_one() {
    let adapter: &'static dyn AgentAdapter = Box::leak(Box::new(Faulty {
        marker: MARKER,
        panic: false,
    }));
    let home = faulty_home();
    let opts = IngestOptions {
        audit_files_per_run: 0,
        ..tiny_batches(2)
    };
    let serial_ledger = TempDir::new().unwrap();
    let serial = capture_into(adapter, home.path(), &opts, SERIAL, serial_ledger.path());
    assert!(serial.result.is_err());
    let pooled_ledger = TempDir::new().unwrap();
    let chaos = Config {
        workers: 4,
        chaos: Some(71),
    };
    let pooled = capture_into(adapter, home.path(), &opts, chaos, pooled_ledger.path());
    assert!(pooled.ledger == serial.ledger, "interrupted ledgers differ");

    // The interrupted source left an unsealed generation behind.
    let mut runtime = discover(&[adapter], home.path(), &opts, &Instrument::new())
        .unwrap()
        .pop()
        .unwrap()
        .runtime;
    runtime.enabled = true;
    let unsealed = rebuild_runtime_with_options(
        adapter,
        &runtime,
        pooled_ledger.path(),
        &mut cold_store(),
        &Instrument::new(),
        RebuildOptions {
            flush_reconciliation: false,
            allow_unsealed_sources: true,
            ..RebuildOptions::default()
        },
    )
    .unwrap()
    .unsealed_sources;
    assert_eq!(
        unsealed.keys().collect::<Vec<_>>(),
        ["projects/-w-pool/s4.jsonl"]
    );

    // Resumed with the fault gone, both finish to the same projection and
    // the same ledger, and that projection is the uninterrupted build's.
    let from_serial = resume(home.path(), serial_ledger.path(), &opts);
    let from_pool = resume(home.path(), pooled_ledger.path(), &opts);
    assert_same_dump("resumed", &from_serial, &from_pool);
    assert!(
        ledger_files(pooled_ledger.path()) == ledger_files(serial_ledger.path()),
        "resumed ledgers differ"
    );
    let uninterrupted = resume(home.path(), TempDir::new().unwrap().path(), &opts);
    assert_same_dump(
        "resumed against uninterrupted",
        &without_generations(&uninterrupted),
        &without_generations(&from_pool),
    );
}

/// A dump without the columns that number generations: resuming reserves a
/// generation above the interrupted one.
fn without_generations(dump: &BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    dump.iter()
        .map(|(table, rows)| {
            let rows = rows
                .iter()
                .map(|row| {
                    row.split(" | ")
                        .filter(|field| {
                            let column = field.split_once('=').map_or(*field, |(column, _)| column);
                            !column.contains("generation") && column != "checkpoint"
                        })
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect();
            (table.clone(), rows)
        })
        .collect()
}
