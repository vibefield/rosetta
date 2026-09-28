//! A transcript caught mid-append: its file ends inside a record its writer
//! has not finished. A pass that must end every source at a terminal
//! generation (`IngestOptions::stop_at_partial_record`, the fresh cold
//! build's closed walk) stops at the last complete record — the clean prefix
//! activates and seals exactly as if the file ended there, the session holds
//! Streaming, and the checkpoint keeps the unfinished suffix — so the record
//! lands later as ordinary append growth, leaving exactly what one pass over
//! the finished file leaves. Without the option, as on every live pass,
//! nothing changes: the generation stays staged until a read finds the record
//! complete. The cut falls inside a tool result, so the growth must also
//! revise the call the prefix finalized.

use std::io::Write;
use std::path::{Path, PathBuf};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_core::ids;
use rosetta_core::rows::{RuntimeRow, StreamState};
use rosetta_core::{StatusBasis, TimelineBody, ToolStatus, ToolStatusFact};
use rosetta_pipeline::{
    IngestOptions, Instrument, PipeCheckpoint, SessionFile, StreamReport, meta_ingest,
    stream_backfill,
};
use rosetta_store::{Store, queries};
use tempfile::TempDir;

fn runtime() -> RuntimeRow {
    let adapter = &ClaudeCodeAdapter;
    RuntimeRow {
        id: ids::runtime_id(adapter.kind(), &adapter.layout().data_root),
        kind: adapter.kind().to_owned(),
        data_root: adapter.layout().data_root.clone(),
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

fn live_opts() -> IngestOptions {
    IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    }
}

fn stopping_opts() -> IngestOptions {
    IngestOptions {
        stop_at_partial_record: true,
        ..live_opts()
    }
}

/// The one transcript a single-session corpus wrote.
fn only_jsonl(root: &Path) -> PathBuf {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .unwrap()
}

/// A one-session Claude corpus, and the same corpus cut inside the first
/// tool result that follows its call: `(whole, cut, finished lines, suffix)`
/// where the cut file holds the finished lines and the first half of the
/// next one, and `suffix` is everything the cut withheld.
fn corpora() -> (TempDir, TempDir, usize, Vec<u8>) {
    let whole = TempDir::new().unwrap();
    let cut = TempDir::new().unwrap();
    let spec = CorpusSpec::new(Agent::ClaudeCode, 1, 60, 3);
    generate(&spec, whole.path()).unwrap();
    generate(&spec, cut.path()).unwrap();
    let path = only_jsonl(cut.path());
    let text = std::fs::read(&path).unwrap();
    let lines: Vec<&[u8]> = text.split_inclusive(|byte| *byte == b'\n').collect();
    let opener = lines
        .windows(2)
        .position(|pair| {
            contains(pair[0], br#""type":"tool_use""#)
                && contains(pair[1], br#""type":"tool_result""#)
        })
        .expect("the corpus must contain a tool round-trip");
    let finished = opener + 1;
    let kept: usize = lines[..finished].iter().map(|line| line.len()).sum();
    let at = kept + lines[finished].len() / 2;
    std::fs::write(&path, &text[..at]).unwrap();
    (whole, cut, finished, text[at..].to_vec())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn meta(root: &Path, store: &mut Store) -> Vec<SessionFile> {
    let (_, files) = meta_ingest(
        &ClaudeCodeAdapter,
        &runtime(),
        root,
        store,
        &live_opts(),
        &Instrument::new(),
    )
    .unwrap();
    assert_eq!(files.len(), 1);
    files
}

fn pass(
    root: &Path,
    store: &mut Store,
    files: &[SessionFile],
    opts: &IngestOptions,
) -> StreamReport {
    stream_backfill(
        &ClaudeCodeAdapter,
        &runtime(),
        root,
        store,
        files,
        opts,
        &Instrument::new(),
    )
    .unwrap()
}

fn timeline(store: &Store, session: &str) -> Vec<rosetta_core::TimelineEntry> {
    queries::timeline_page(store.conn(), session, None, 10_000).unwrap()
}

type BucketRow = (i64, String, Option<i64>, Option<i64>, i64, i64);

fn buckets(store: &Store) -> Vec<BucketRow> {
    let mut statement = store
        .conn()
        .prepare(
            "SELECT hour_ts, model, in_tokens, out_tokens, message_count, tool_call_count
             FROM usage_bucket ORDER BY hour_ts, model",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn session_state(store: &Store, session: &str) -> StreamState {
    queries::session(store.conn(), session)
        .unwrap()
        .unwrap()
        .stream_state
}

/// `(active, highest)` generation and the tail's `(committed, unfinished)`
/// bytes of the transcript's cursor.
fn cursor(store: &Store, file: &SessionFile) -> ((u64, u64), (u64, u64)) {
    let cursor = queries::cursor_state(store.conn(), &runtime().id, &file.relative_path)
        .unwrap()
        .unwrap();
    let tail = PipeCheckpoint::decode(&cursor.checkpoint)
        .and_then(|checkpoint| checkpoint.tail_checkpoint())
        .map_or((0, 0), |tail| {
            (tail.committed_offset, tail.incomplete_suffix_len)
        });
    ((cursor.active_generation, cursor.highest_generation), tail)
}

fn finish_record(root: &Path, suffix: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(only_jsonl(root))
        .unwrap();
    file.write_all(suffix).unwrap();
}

#[test]
fn a_partial_record_stop_then_growth_matches_one_shot() {
    let (whole, cut, finished, suffix) = corpora();
    let mut one_shot = Store::open_in_memory().unwrap();
    let whole_files = meta(whole.path(), &mut one_shot);
    let report = pass(whole.path(), &mut one_shot, &whole_files, &live_opts());
    assert_eq!((report.files_incomplete, report.files_paused), (0, 0));

    let mut store = Store::open_in_memory().unwrap();
    let files = meta(cut.path(), &mut store);
    let session = &files[0].session_id;
    let first = pass(cut.path(), &mut store, &files, &stopping_opts());
    assert_eq!(
        first.records, finished as u64,
        "only finished records frame"
    );
    assert_eq!((first.files_incomplete, first.files_paused), (0, 1));
    let ((active, highest), (committed, unfinished)) = cursor(&store, &files[0]);
    assert_eq!(
        (active, highest),
        (1, 1),
        "the prefix is the active generation"
    );
    let cut_len = std::fs::metadata(only_jsonl(cut.path())).unwrap().len();
    assert!(unfinished > 0, "the checkpoint keeps the unfinished record");
    assert_eq!(committed + unfinished, cut_len);
    assert_eq!(session_state(&store, session), StreamState::Streaming);
    // Exactly the rows a file truncated before the record would produce: the
    // call its result was cut from is finalized, not resolved.
    let call = timeline(&store, session)
        .into_iter()
        .rfind(|entry| matches!(entry.body, TimelineBody::ToolUse { .. }))
        .expect("the prefix ends on the call");
    let TimelineBody::ToolUse { tool } = &call.body else {
        unreachable!()
    };
    assert_eq!(
        tool.status,
        ToolStatusFact {
            value: ToolStatus::Unknown,
            basis: StatusBasis::SourceFinalization,
        }
    );

    // A pass that finds nothing new keeps the stop and writes nothing.
    let quiet = pass(cut.path(), &mut store, &files, &stopping_opts());
    assert_eq!((quiet.batches, quiet.files_paused), (0, 1));

    // The writer finishes the record (and the rest of the session): an
    // ordinary live pass reads only the growth, into the same generation.
    finish_record(cut.path(), &suffix);
    let growth = pass(cut.path(), &mut store, &files, &live_opts());
    assert_eq!(
        growth.generations_restarted, 0,
        "the prefix is never replayed"
    );
    assert_eq!(growth.records, report.records - finished as u64);
    assert_eq!((growth.files_incomplete, growth.files_paused), (0, 0));
    assert_eq!(cursor(&store, &files[0]).0, (1, 1));
    assert_eq!(session_state(&store, session), StreamState::Indexed);
    assert_eq!(timeline(&store, session), timeline(&one_shot, session));
    assert_eq!(buckets(&store), buckets(&one_shot));
}

#[test]
fn without_the_option_a_partial_first_generation_stays_staged() {
    let (whole, cut, _, suffix) = corpora();
    let mut one_shot = Store::open_in_memory().unwrap();
    let whole_files = meta(whole.path(), &mut one_shot);
    pass(whole.path(), &mut one_shot, &whole_files, &live_opts());

    let mut store = Store::open_in_memory().unwrap();
    let files = meta(cut.path(), &mut store);
    let session = &files[0].session_id;
    let first = pass(cut.path(), &mut store, &files, &live_opts());
    assert_eq!((first.files_incomplete, first.files_paused), (1, 0));
    assert_eq!(cursor(&store, &files[0]).0, (0, 1), "staged, not active");
    assert!(timeline(&store, session).is_empty());
    assert_eq!(session_state(&store, session), StreamState::Streaming);

    // Once the record is finished the staged generation is not resumed: the
    // next pass reads the file from zero into a new generation.
    finish_record(cut.path(), &suffix);
    let settled = pass(cut.path(), &mut store, &files, &live_opts());
    assert_eq!(settled.generations_restarted, 0);
    assert_eq!((settled.files_incomplete, settled.files_paused), (0, 0));
    assert_eq!(cursor(&store, &files[0]).0, (2, 2));
    assert_eq!(session_state(&store, session), StreamState::Indexed);
    assert_eq!(timeline(&store, session), timeline(&one_shot, session));
}
