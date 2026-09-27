//! The stat poll's RAM evidence against the path it replaced. Every poll here
//! first runs the old evidence path verbatim — read and decode each walked
//! roster source's cursor row — over the same roster state, and the real poll
//! must nominate exactly the same sources and admission.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_bench_corpus::{Agent, CorpusSpec, generate};
use rosetta_capture::ScriptedEvents;
use rosetta_store::queries;
use tempfile::TempDir;

use super::*;
use crate::{discover, expand_data_root, meta_ingest, stream_backfill};

struct Harness {
    _home: TempDir,
    root: PathBuf,
    runtime: RuntimeRow,
    store: Store,
    opts: IngestOptions,
    instrument: Instrument,
    files: Vec<SessionFile>,
}

/// A Claude root after meta and a full backfill: the state a runner inherits.
fn backfilled() -> Harness {
    let adapter = &ClaudeCodeAdapter;
    let home = TempDir::new().unwrap();
    let root = expand_data_root(&adapter.layout().data_root, home.path());
    std::fs::create_dir_all(&root).unwrap();
    generate(&CorpusSpec::new(Agent::ClaudeCode, 6, 60, 7), &root).unwrap();

    let opts = IngestOptions::default();
    let instrument = Instrument::new();
    let mut runtime = discover(&[adapter], home.path(), &opts, &instrument).unwrap()[0]
        .runtime
        .clone();
    runtime.enabled = true;
    let mut store = Store::open_in_memory().unwrap();
    let (_, mut files) =
        meta_ingest(adapter, &runtime, &root, &mut store, &opts, &instrument).unwrap();
    stream_backfill(
        adapter,
        &runtime,
        &root,
        &mut store,
        &files,
        &opts,
        &instrument,
    )
    .unwrap();
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    assert!(files.len() >= 5, "the scenarios need five transcripts");
    Harness {
        _home: home,
        root,
        runtime,
        store,
        opts,
        instrument,
        files,
    }
}

fn runner(h: &Harness, events: ScriptedEvents) -> LiveRunner<'static> {
    LiveRunner::resume_from_roster(
        &ClaudeCodeAdapter,
        h.runtime.clone(),
        h.root.clone(),
        Box::new(events),
        h.files.clone(),
        h.opts.clone(),
        LiveOptions {
            tick: Duration::ZERO,
            poll_interval: Duration::from_secs(3600),
            max_coalesced: 16,
        },
        None,
    )
}

/// What a cold writer hands over: the evidence of every committed cursor.
fn committed(h: &Harness) -> HashMap<String, PollEvidence> {
    h.files
        .iter()
        .map(|file| {
            let evidence = PollEvidence::read(&h.store, &h.runtime.id, &file.relative_path);
            (file.relative_path.clone(), evidence.unwrap())
        })
        .collect()
}

/// The poll before its evidence lived in RAM, over copies of the runner's
/// roster state: each walked roster source's cursor row read and decoded.
fn reference_poll(runner: &LiveRunner<'_>, store: &Store) -> (BTreeSet<String>, bool) {
    let adapter = runner.adapter;
    let layout = adapter.layout();
    let walk = rosetta_capture::walk_filtered(&runner.root, runner.opts.max_files, |path| {
        layout
            .matching_stream(path)
            .is_some_and(|stream| adapter.stream_driver(&stream.id).is_some())
    })
    .unwrap();
    let files = runner.files.clone();
    let mut physical_seen = runner.physical_seen.clone();
    let mut changed = BTreeSet::new();
    let mut needs_admission = false;
    let mut by_path = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        by_path.entry(file.relative_path.clone()).or_insert(index);
    }
    for walked in &walk.files {
        match by_path.get(&walked.relative_path).copied() {
            Some(index) => {
                let known = &files[index];
                let checkpoint =
                    queries::cursor_state(store.conn(), &runner.runtime.id, &walked.relative_path)
                        .unwrap()
                        .and_then(|cursor| PipeCheckpoint::decode(&cursor.checkpoint));
                let normalizer_stale = checkpoint.as_ref().is_some_and(|checkpoint| {
                    checkpoint.normalizer_version != adapter.normalizer_version()
                });
                let ingested_len = checkpoint
                    .and_then(|checkpoint| checkpoint.tail_checkpoint())
                    .map(|tail| tail.observed_len);
                let behind_cursor = ingested_len != Some(walked.size);
                let touched = known.modified_ms != walked.modified_ms;
                if normalizer_stale || behind_cursor || touched {
                    changed.insert(known.relative_path.clone());
                }
            }
            None => {
                let stats = (walked.size, walked.modified_ms);
                let seen = physical_seen.get(&walked.relative_path);
                let moved = seen.is_some_and(|seen| *seen != stats);
                let first = seen.is_none();
                physical_seen.insert(walked.relative_path.clone(), stats);
                if !(first || moved) {
                    continue;
                }
                let mapping = map_hint(adapter, &files, &walked.relative_path);
                if moved {
                    changed.extend(mapping.nominated);
                    needs_admission |= mapping.needs_admission;
                } else if mapping.nominated.is_empty() {
                    needs_admission |= mapping.needs_admission;
                }
            }
        }
    }
    let present: BTreeSet<&str> = walk
        .files
        .iter()
        .map(|file| file.relative_path.as_str())
        .collect();
    for file in &files {
        if let Some(physical) = physical_snapshot_path(file)
            && !present.contains(physical)
        {
            changed.insert(file.relative_path.clone());
            needs_admission = true;
        }
    }
    (changed, needs_admission)
}

/// One forced poll through both evidence paths, then the rest of the tick.
fn poll_step(runner: &mut LiveRunner<'_>, h: &mut Harness, audit: bool) -> BTreeSet<String> {
    let expected = reference_poll(runner, &h.store);
    let mut changed = BTreeSet::new();
    let mut needs_admission = false;
    runner
        .poll(
            &mut h.store,
            &mut changed,
            &mut needs_admission,
            &h.instrument,
        )
        .unwrap();
    assert_eq!(
        (changed.clone(), needs_admission),
        expected,
        "RAM evidence nominated differently from the cursor rows"
    );
    runner
        .settle(
            &mut h.store,
            changed.clone(),
            needs_admission,
            audit,
            &h.instrument,
        )
        .unwrap();
    for (path, evidence) in &runner.evidence {
        assert_eq!(
            PollEvidence::read(&h.store, &h.runtime.id, path).ok(),
            Some(*evidence),
            "{path} kept evidence its cursor no longer holds"
        );
    }
    changed
}

fn cursor_reads(h: &Harness) -> u64 {
    h.instrument.get(Metric::LivePollCursorReads)
}

fn paths(files: &[&SessionFile]) -> BTreeSet<String> {
    files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect()
}

/// Mirrors the corpus writer's user record: all canonical-required fields.
fn append_user_line(path: &Path, native_session_id: &str, content: &str) {
    let line = serde_json::json!({
        "type": "user",
        "sessionId": native_session_id,
        "uuid": format!("00000000-0000-4000-8000-{:012x}", content.len()),
        "parentUuid": null,
        "isSidechain": false,
        "userType": "external",
        "timestamp": "2026-05-01T00:00:00.000Z",
        "cwd": "/tmp/bench/project-0",
        "gitBranch": "main",
        "version": "2.1.240",
        "message": {"role": "user", "content": content},
    });
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

fn set_modified(path: &Path, at: SystemTime) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(at))
        .unwrap();
}

fn later(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap() + Duration::from_secs(90)
}

#[test]
fn a_seeded_poll_reads_no_cursor_row_and_nominates_like_the_cursor_rows() {
    let mut h = backfilled();
    let mut live = runner(&h, ScriptedEvents::default()).with_poll_evidence(committed(&h));
    let reads = cursor_reads(&h);
    assert!(poll_step(&mut live, &mut h, false).is_empty());
    assert_eq!(cursor_reads(&h), reads, "seeded evidence decodes nothing");

    // Unseeded — a warm start — reads each row once, then holds it.
    let mut warm = runner(&h, ScriptedEvents::default());
    assert!(poll_step(&mut warm, &mut h, false).is_empty());
    assert_eq!(cursor_reads(&h), reads + h.files.len() as u64);
    assert!(poll_step(&mut warm, &mut h, false).is_empty());
    assert_eq!(cursor_reads(&h), reads + h.files.len() as u64);
}

#[test]
fn growth_truncation_rewrite_touch_and_vanishing_nominate_like_the_cursor_rows() {
    let mut h = backfilled();
    let mut live = runner(&h, ScriptedEvents::default()).with_poll_evidence(committed(&h));
    let [grown, truncated, rewritten, touched, vanished] =
        std::array::from_fn(|index| h.files[index].clone());
    let root = h.root.clone();
    let at = |file: &SessionFile| root.join(&file.relative_path);

    append_user_line(&at(&grown), &grown.native_session_id, "offline growth");
    let text = std::fs::read_to_string(at(&truncated)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    std::fs::write(
        at(&truncated),
        format!("{}\n", lines[..lines.len() / 2].join("\n")),
    )
    .unwrap();
    // A same-size rewrite: only the mtime and the content chain move.
    let original = std::fs::read_to_string(at(&rewritten)).unwrap();
    let rewrite = original.replacen("\"timestamp\":\"2026-", "\"timestamp\":\"2025-", 1);
    assert_ne!(rewrite, original);
    let stamp = later(&at(&rewritten));
    std::fs::write(at(&rewritten), rewrite).unwrap();
    set_modified(&at(&rewritten), stamp);
    set_modified(&at(&touched), later(&at(&touched)));
    let vanished_text = std::fs::read(at(&vanished)).unwrap();
    std::fs::remove_file(at(&vanished)).unwrap();

    assert_eq!(
        poll_step(&mut live, &mut h, false),
        paths(&[&grown, &truncated, &rewritten, &touched])
    );
    // What the runner streamed was forgotten and is read back from the
    // cursors it just committed; everything else stays in RAM.
    let reads = cursor_reads(&h);
    assert!(poll_step(&mut live, &mut h, false).is_empty());
    assert_eq!(cursor_reads(&h), reads + 4);

    // The vanished source's evidence is still its committed cursor, so its
    // return is judged against that cursor, exactly like the row itself.
    std::fs::write(at(&vanished), vanished_text).unwrap();
    set_modified(&at(&vanished), later(&at(&vanished)));
    assert_eq!(poll_step(&mut live, &mut h, false), paths(&[&vanished]));
    assert!(poll_step(&mut live, &mut h, true).is_empty());
    assert!(poll_step(&mut live, &mut h, false).is_empty());
}

#[test]
fn a_normalizer_change_missing_cursor_and_new_file_nominate_like_the_cursor_rows() {
    for seeded in [false, true] {
        let mut h = backfilled();
        let adapter = &ClaudeCodeAdapter;
        let stale = h.files[0].clone();
        let cursor = queries::cursor_state(h.store.conn(), &h.runtime.id, &stale.relative_path)
            .unwrap()
            .unwrap();
        let mut checkpoint = PipeCheckpoint::decode(&cursor.checkpoint).unwrap();
        checkpoint.normalizer_version = adapter.normalizer_version() - 1;
        h.store
            .conn()
            .execute(
                "UPDATE ingest_cursor SET checkpoint = ?1 WHERE id = ?2",
                rusqlite::params![checkpoint.encode(), cursor.id],
            )
            .unwrap();

        // Admitted by meta but never streamed: no cursor row at all.
        let unstreamed = "0f000000-1111-4222-8333-444455556661";
        let relative = format!("projects/-tmp-bench-live/{unstreamed}.jsonl");
        let path = h.root.join(&relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::File::create(&path).unwrap();
        append_user_line(&path, unstreamed, "admitted, never streamed");
        let (_, files) = meta_ingest(
            adapter,
            &h.runtime,
            &h.root,
            &mut h.store,
            &h.opts,
            &h.instrument,
        )
        .unwrap();
        h.files = files;
        assert!(
            queries::cursor_state(h.store.conn(), &h.runtime.id, &relative)
                .unwrap()
                .is_none()
        );
        let mut live = runner(&h, ScriptedEvents::default());
        if seeded {
            live = live.with_poll_evidence(committed(&h));
        }

        // A claimed file no roster entry covers: first sighting, admission.
        let fresh = "0f000000-1111-4222-8333-444455556662";
        let fresh_path = h
            .root
            .join(format!("projects/-tmp-bench-live/{fresh}.jsonl"));
        std::fs::File::create(&fresh_path).unwrap();
        append_user_line(&fresh_path, fresh, "born during the handoff");

        let nominated = poll_step(&mut live, &mut h, false);
        assert_eq!(
            nominated,
            [stale.relative_path.clone(), relative.clone()].into()
        );
        assert_eq!(live.report().admissions, 1, "seeded={seeded}");
        assert!(poll_step(&mut live, &mut h, false).is_empty());
        let checkpoint = PollEvidence::read(&h.store, &h.runtime.id, &stale.relative_path);
        assert_eq!(
            checkpoint.unwrap().normalizer_version,
            Some(adapter.normalizer_version())
        );
        assert_eq!(
            queries::search(h.store.conn(), "handoff", 5).unwrap().len(),
            1,
            "the admitted file streamed"
        );
    }
}

#[test]
fn a_source_the_runner_just_streamed_is_read_back_from_its_new_cursor() {
    let mut h = backfilled();
    let target = h.files[0].clone();
    let path = h.root.join(&target.relative_path);
    append_user_line(&path, &target.native_session_id, "hinted growth");
    let mut events = ScriptedEvents::default();
    events.push_changed(&target.relative_path);
    let mut live = runner(&h, events).with_poll_evidence(committed(&h));

    live.tick_ready(&mut h.store, &h.instrument).unwrap();
    assert_eq!(live.report().polls, 0);
    assert_eq!(live.report().stream.stats.messages_inserted, 1);
    assert!(!live.evidence.contains_key(&target.relative_path));

    let reads = cursor_reads(&h);
    assert!(poll_step(&mut live, &mut h, false).is_empty());
    assert_eq!(cursor_reads(&h), reads + 1);
    // Growth the watcher never reported is judged against that committed
    // cursor, not against anything this process saw.
    append_user_line(&path, &target.native_session_id, "silent growth");
    assert_eq!(
        poll_step(&mut live, &mut h, false),
        [target.relative_path.clone()].into()
    );
    assert!(poll_step(&mut live, &mut h, false).is_empty());
}
