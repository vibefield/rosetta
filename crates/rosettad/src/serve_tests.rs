use std::io::Write;
use std::sync::atomic::AtomicUsize;

use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_capture::{SourceEvent, SourceEvents};
use rosetta_server::{HubItem, Subscription};
use rosetta_store::hot::Mutation;

use super::*;

fn append(path: &Path, uuid: &str) {
    let line = serde_json::json!({
        "type": "user", "sessionId": "live-session", "uuid": uuid,
        "parentUuid": null, "isSidechain": false, "userType": "external",
        "timestamp": "2026-05-01T00:00:00.000Z", "cwd": "/tmp/live-project",
        "gitBranch": "main", "version": "2.1.240",
        "message": {"role": "user", "content": uuid},
    });
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

struct AppendAfterCatchUp {
    path: PathBuf,
    relative: String,
    calls: usize,
}

impl SourceEvents for AppendAfterCatchUp {
    fn next_event(&mut self, timeout: Duration) -> Option<SourceEvent> {
        assert_eq!(timeout, Duration::ZERO);
        self.calls += 1;
        if self.calls == 2 {
            append(&self.path, "live-append");
            Some(SourceEvent::Changed(self.relative.clone()))
        } else {
            None
        }
    }
}

struct QuietSubscriber {
    subscription: Subscription,
    checked: Arc<AtomicUsize>,
    calls: usize,
}

impl SourceEvents for QuietSubscriber {
    fn next_event(&mut self, timeout: Duration) -> Option<SourceEvent> {
        assert_eq!(timeout, Duration::ZERO, "quiet runtimes must not wait");
        self.calls += 1;
        if self.calls == 2 {
            let mut delivered = false;
            while let Some(item) = self.subscription.try_recv() {
                if let HubItem::Mutation { mutation, .. } = item
                    && let Mutation::Message(row) = &*mutation
                    && row.native_id.as_deref() == Some("live-append")
                {
                    delivered = true;
                }
            }
            assert!(
                delivered,
                "preceding runtime's commit must already be published"
            );
            self.checked.fetch_add(1, Ordering::SeqCst);
        }
        None
    }
}

#[test]
fn live_commit_reaches_subscribers_before_eight_quiet_runtimes_are_polled() {
    let home = tempfile::TempDir::new().unwrap();
    let root = home.path().join(".claude");
    let relative = "projects/live/live-session.jsonl";
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, []).unwrap();
    append(&path, "initial");
    let quiet_root = home.path().join("quiet");
    std::fs::create_dir(&quiet_root).unwrap();
    let instrument = Instrument::new();
    let opts = IngestOptions {
        audit_files_per_run: 0,
        ..IngestOptions::default()
    };
    let discovery =
        rosetta_pipeline::discover(&[&ClaudeCodeAdapter], home.path(), &opts, &instrument)
            .unwrap()
            .remove(0);
    let runtime = discovery.runtime;
    let db = home.path().join("projection.sqlite");
    let mut store = Store::open(&db).unwrap();
    let (_, files) = rosetta_pipeline::meta_ingest(
        &ClaudeCodeAdapter,
        &runtime,
        &root,
        &mut store,
        &opts,
        &instrument,
    )
    .unwrap();
    rosetta_pipeline::stream_backfill(
        &ClaudeCodeAdapter,
        &runtime,
        &root,
        &mut store,
        &files,
        &opts,
        &instrument,
    )
    .unwrap();
    assert_eq!(files.len(), 1);
    let hub = Hub::new(HubConfig::default(), 1);
    hub.warm(store.conn()).unwrap();
    let live = LiveOptions {
        tick: Duration::from_secs(60),
        poll_interval: Duration::from_secs(3600),
        ..LiveOptions::default()
    };
    let active = LiveRunner::resume_from_roster(
        &ClaudeCodeAdapter,
        runtime.clone(),
        root,
        Box::new(AppendAfterCatchUp {
            path,
            relative: relative.into(),
            calls: 0,
        }),
        files.clone(),
        opts.clone(),
        live.clone(),
        None,
    );
    let mut runners = vec![(
        RuntimeIdentity::new(runtime.id.clone(), "claude-code", None),
        active,
    )];
    let checked = Arc::new(AtomicUsize::new(0));
    for index in 0..8 {
        let (subscription, _) = hub.subscribe_session(&files[0].session_id, now_ms());
        let quiet = LiveRunner::resume_from_roster(
            &ClaudeCodeAdapter,
            runtime.clone(),
            quiet_root.clone(),
            Box::new(QuietSubscriber {
                subscription,
                checked: checked.clone(),
                calls: 0,
            }),
            vec![],
            opts.clone(),
            live.clone(),
            None,
        );
        runners.push((
            RuntimeIdentity::new(format!("quiet-{index}"), "quiet", None),
            quiet,
        ));
    }
    let worker = Fts5SearchWorker::start(Fts5SidecarSearch::new(db));
    let flags = parse_flags(&["--ticks".into(), "1".into()]).unwrap();
    let result = drive_live_runners(
        &flags,
        &mut store,
        &mut runners,
        &SourceWake::default(),
        Duration::ZERO,
        &instrument,
        &DaemonHealth::new(now_ms()),
        &hub,
        &worker,
        &AtomicBool::new(false),
    );
    worker.stop();
    result.unwrap();
    assert_eq!(checked.load(Ordering::SeqCst), 8);
    assert_eq!(runners[0].1.report().stream.stats.messages_inserted, 1);
}
