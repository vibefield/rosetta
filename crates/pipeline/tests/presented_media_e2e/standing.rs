//! Sources whose standing keeps them out of the projection: a ledger-only
//! source and a session in an excluded folder.

use super::*;

/// Claude Code, except that the session named `internal` is marked
/// ledger-only at the metadata skim, as Codex marks its guardian reviews.
/// Its records still normalize as Claude's do, so it states sent-file facts
/// a ledger-only source never should: the shape the capture guard is for.
/// No real Claude source is ledger-only. Only what a transcript-only root
/// needs is delegated.
struct Internal;

const INTERNAL: &str = "internal";

impl AgentAdapter for Internal {
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

    fn init_state(&self) -> Option<Box<dyn Any + Send>> {
        ClaudeCodeAdapter.init_state()
    }

    fn checkpoint_state(&self, state: Option<&(dyn Any + Send)>) -> Result<Option<Value>, String> {
        ClaudeCodeAdapter.checkpoint_state(state)
    }

    fn restore_state(&self, saved: Option<&Value>) -> Result<Option<Box<dyn Any + Send>>, String> {
        ClaudeCodeAdapter.restore_state(saved)
    }

    fn skim(&self, stream_id: &str, stat: &FileStat, head: &[u8], tail: &[u8]) -> SkimResult {
        let mut skim = ClaudeCodeAdapter.skim(stream_id, stat, head, tail);
        skim.ledger_only = skim.session_id.as_deref() == Some(INTERNAL);
        skim
    }

    fn ingest_line(&self, ctx: &mut NormalizeCtx, stream_id: &str, line: &[u8]) -> LineOutcome {
        ClaudeCodeAdapter.ingest_line(ctx, stream_id, line)
    }
}

// 9. A source that is captured raw and never projected. The store refuses
//    what such a source states, after the ledger append; the capture seam
//    must already have declined to read the file it names.
#[test]
fn a_ledger_only_source_gets_no_frame_for_the_file_it_sent() {
    let mut f = Fixture::new();
    let private = mp4(2048, 12);
    let clip = f.media("private.mp4", &private);
    let offset = f.send(INTERNAL, "send-1", &[(&clip, 2048, MP4)]);

    let (runtime, root, ledger) = (&f.runtime, &f.root, Some(&mut f.ledger));
    let refused = try_ingest(&Internal, runtime, root, &mut f.store, ledger, &options());
    let refused = refused.unwrap_err().to_string();
    let refusal = "ledger-only source cannot project operations";
    assert!(refused.contains(refusal), "{refused}");

    // Ledger-first: the raw records are there, flagged ledger-only, and no
    // frame is. The file was present with its recorded size, and unread.
    let entries = scan_ledger(&f.ledger_dir).unwrap().entries;
    let raw = entries.iter().filter_map(|entry| match entry {
        LedgerEntry::Raw { source, .. } => Some(source.ledger_only),
        _ => None,
    });
    assert_eq!(raw.collect::<Vec<_>>(), [true, true]);
    assert!(
        media_frames(&f.ledger_dir).is_empty(),
        "no frame was written"
    );
    assert_eq!(verify_ledger(&f.ledger_dir).unwrap().tally.documents, 0);
    assert_eq!(std::fs::read(&clip).unwrap(), private);
    assert_eq!(shown(&f.store, &f.db), (Vec::new(), BTreeMap::new()));
    assert!(queries::all_sessions(f.store.conn()).unwrap().is_empty());

    // The same transcript as an ordinary session is captured, so only the
    // source's standing kept the file out of the ledger.
    let visible_db = f.temp.path().join("visible.sqlite");
    let visible_ledger = f.temp.path().join("visible-ledger");
    let mut visible = Store::open(&visible_db).unwrap();
    let mut ledger = Ledger::open(&visible_ledger).unwrap();
    let ledger = Some(&mut ledger);
    try_ingest(
        &ClaudeCodeAdapter,
        runtime,
        root,
        &mut visible,
        ledger,
        &options(),
    )
    .unwrap();
    assert_eq!(
        frame_statuses(&visible_ledger),
        [(1, offset, Status::Captured)]
    );
    assert_eq!(
        shown(&visible, &visible_db).1,
        members(&[("mp4", &private)])
    );
}

// 9, as Claude Code meets it: no Claude source is ledger-only, and a session
//    in an excluded folder is not captured at all, so neither is its file.
#[test]
fn an_excluded_folders_session_is_not_captured_and_neither_is_the_file_it_sent() {
    let mut f = Fixture::new();
    let (seen, private) = (mp4(2048, 13), mp4(2048, 14));
    let seen_clip = f.media("seen.mp4", &seen);
    let private_clip = f.media("private.mp4", &private);
    let offset = f.send(NATIVE, "send-1", &[(&seen_clip, 2048, MP4)]);
    let client = f.temp.path().join("client");
    let files = [(private_clip.as_str(), 2048, MP4)];
    let excluded = f.transcript("excluded");
    append_line(&excluded, &opener("excluded", &client, "send-2", &files));
    let sent = result("excluded", &client, "send-2", json!("sent"), &files);
    let private_offset = append_line(&excluded, &sent);
    f.ingest_with(&IngestOptions {
        exclude_cwds: CwdExclusions::new([client.to_str().unwrap().to_owned()]).unwrap(),
        ..options()
    });

    let native_sessions = |store: &Store| {
        let sessions = queries::all_sessions(store.conn()).unwrap();
        let mut ids: Vec<String> = sessions.into_iter().map(|s| s.native_session_id).collect();
        ids.sort();
        ids
    };
    assert_eq!(native_sessions(&f.store), [NATIVE]);
    // One frame: the visible session's.
    assert_eq!(
        frame_statuses(&f.ledger_dir),
        [(1, offset, Status::Captured)]
    );
    let frames = media_frames(&f.ledger_dir);
    assert_eq!(frames[0].source, "projects/synthetic/native.jsonl");
    assert_eq!(frames[0].document.path, seen_clip);
    let entries = scan_ledger(&f.ledger_dir).unwrap().entries;
    let of_excluded = |entry: &LedgerEntry| matches!(entry, LedgerEntry::Raw { source, .. } if source.native_session_id == "excluded");
    assert!(
        !entries.iter().any(of_excluded),
        "nothing of it reaches the ledger"
    );
    let live = shown(&f.store, &f.db);
    assert_eq!(live.1, members(&[("mp4", &seen)]));
    assert_eq!(
        f.rebuilt("excluding"),
        live,
        "a rebuild of that ledger agrees"
    );

    // Removing the rule resumes capture, and the file it sent is observed
    // then, as of then.
    f.ingest();
    assert_eq!(native_sessions(&f.store), ["excluded", NATIVE]);
    let frames = media_frames(&f.ledger_dir);
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[1].source, "projects/synthetic/excluded.jsonl");
    let observed = (frames[1].offset, frames[1].document.status);
    assert_eq!(observed, (private_offset, Status::Captured));
    let sent = [asset(Video, &private, "private.mp4", MP4)];
    assert_eq!(slots(&f.store, &f.session("excluded")), sent);
}
