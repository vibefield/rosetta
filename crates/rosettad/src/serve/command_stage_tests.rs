//! T7's `commands.stage()` wiring (`serve/control.rs`), end to end on the
//! real `serve`: a tree flipped mid-build answers its full command — read
//! from the staged raw ledger, since the public ledger root does not exist
//! until publication — with 200, while the rest of the build is still
//! walking. A walk probe parks the sequencer right after the first flip, so
//! "mid-build" is a fixed point, not a race. (Mutation: never staging the
//! command reader makes it look under the public root, and the route fails.)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use rosetta_pipeline::{ProbeCounters, ProbeView, WalkProbe};
use tempfile::TempDir;

use super::inprocess::{flags, get, spawn, staged_ledger};
use crate::test_support::fixtures::{ClaudeTranscript, T0, claude_root, filler};

const HOUR: i64 = 3_600_000;

/// `alpha`, the newest tree, runs one long shell command; sixteen older
/// trees keep the walk going after it flips.
fn home(home: &std::path::Path) -> String {
    let projects = claude_root(home).join("projects");
    let command = format!("echo {}", filler("full", 0, 3_000));
    let mut alpha = ClaudeTranscript::new("alpha", None, T0);
    alpha
        .prompt("run it")
        .tool(&command, "done")
        .reply("ran it");
    alpha.write(&projects.join("-w-alpha/alpha.jsonl"), T0 + 100 * HOUR);
    for index in 0..16 {
        let id = format!("older{index:02}");
        let mut older = ClaudeTranscript::new(&id, None, T0);
        older.prompt("an older question").bulk(8, 1024);
        older.write(
            &projects.join(format!("-w-{id}/{id}.jsonl")),
            T0 + index * HOUR,
        );
    }
    command
}

#[test]
fn a_flipped_trees_full_command_is_read_from_the_staged_ledger_mid_build() {
    let temp = TempDir::new().unwrap();
    let home_dir = temp.path().join("home");
    let command = home(&home_dir);
    let db = temp.path().join("archive/rosetta.db");
    let announce = temp.path().join("announce");
    let counters = Arc::new(ProbeCounters::default());
    let (parked, on_park) = mpsc::channel::<()>();
    let (release, on_release) = mpsc::channel::<()>();
    let on_release = std::sync::Mutex::new(on_release);
    let once = AtomicBool::new(false);
    let probe: WalkProbe = {
        let counters = Arc::clone(&counters);
        Arc::new(move |_: &ProbeView| {
            let flipped = (0..1_000).any(|_| {
                let flipped = counters.flips_committed.load(Ordering::SeqCst) > 0;
                if !flipped {
                    std::thread::sleep(Duration::from_millis(1));
                }
                flipped
            });
            if flipped && !once.swap(true, Ordering::SeqCst) {
                parked.send(()).unwrap();
                // Parked until the test has read; never forever.
                let _ = on_release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(60));
            }
            false
        })
    };
    let running = spawn(
        flags(&home_dir, &db, "claude-code", &announce, |flags| {
            flags.cold_walk.probe = Some(probe);
            flags.cold_walk.probe_counters = Some(Arc::clone(&counters));
        }),
        &announce,
    );
    on_park
        .recv_timeout(Duration::from_secs(900))
        .expect("a tree flipped while the walk had sources left");

    // Mid-build: nothing published, the ledger still staged.
    let (_, status) = get(&running.origin, "/api/status");
    assert_eq!(status["canonical_ready"], false, "{status}");
    assert!(!db.exists());
    assert!(staged_ledger(&db).is_dir());
    assert!(!db.with_extension("ledger").exists());
    let (_, sessions) = get(&running.origin, "/api/sessions?limit=500");
    let sessions = sessions["sessions"].as_array().unwrap().clone();
    let readable: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|session| session["stream_state"] != "pending")
        .collect();
    assert_eq!(readable.len(), 1, "alpha alone has flipped: {sessions:?}");
    assert!(sessions.len() > readable.len(), "older trees still pending");
    let id = readable[0]["id"].as_str().unwrap();
    let (status, page) = get(&running.origin, &format!("/api/sessions/{id}/timeline"));
    assert_eq!(status, 200);
    let tool = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry.to_string().contains("echo full0"))
        .unwrap_or_else(|| panic!("no shell row in {page}"));
    let timeline = tool["timeline_id"].as_str().unwrap();
    // A record its capture worker has not written yet answers "still saving
    // · retry"; a client retries it.
    let mut answer = (0, serde_json::Value::Null);
    for _ in 0..200 {
        answer = get(
            &running.origin,
            &format!("/api/sessions/{id}/timeline/{timeline}/command"),
        );
        if answer.0 != 503 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let (status, body) = answer;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["command"], command.as_str());

    release.send(()).unwrap();
    running.join().unwrap();
    assert!(db.exists(), "the build published once released");
}
