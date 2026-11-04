//! The real `serve`, in process, for the R1 tests that need a hidden knob no
//! flag spells: a walk probe (`ColdWalkOptions.probe`) and its writer
//! counters, the chunk bound, the lane split. Each run gets its own home,
//! database and announcement directory under a temporary directory; nothing
//! here reads a live home or the user's announcement directory.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rosetta_capture::{LedgerEntry, scan_ledger};
use serde_json::Value;

use crate::Flags;

/// `serve --db DB --home HOME --agent AGENT --ticks 0`, announcing itself in
/// `announce`, then `tune`d.
pub(super) fn flags(
    home: &Path,
    db: &Path,
    agent: &str,
    announce: &Path,
    tune: impl FnOnce(&mut Flags),
) -> Flags {
    let args: Vec<String> = [
        "--db",
        &db.display().to_string(),
        "--home",
        &home.display().to_string(),
        "--agent",
        agent,
        "--addr",
        "127.0.0.1:0",
        "--ticks",
        "0",
        "--announce",
        "--announce-dir",
        &announce.display().to_string(),
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();
    let mut flags = crate::parse_flags(&args).unwrap();
    tune(&mut flags);
    flags
}

/// One in-process serve at a time. A serve names its shared-cache RAM
/// generation by process id and start millisecond — unique per process, as
/// a daemon runs one — so two serves of one test process starting in the
/// same millisecond would open the same database.
static SERVES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One serve on this thread, to its end.
pub(super) fn run(flags: Flags) -> Result<(), String> {
    let _one = SERVES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    super::serve(flags, Instant::now())
}

/// A serve on its own thread, and the origin it announced.
pub(super) struct Running {
    pub(super) origin: String,
    handle: JoinHandle<Result<(), String>>,
}

impl Running {
    pub(super) fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub(super) fn join(self) -> Result<(), String> {
        self.handle.join().expect("serve thread panicked")
    }
}

/// Start `flags` (announcing into `announce`) on a thread; wait for its
/// announcement.
pub(super) fn spawn(flags: Flags, announce: &Path) -> Running {
    std::fs::create_dir_all(announce).unwrap();
    let handle = std::thread::spawn(move || run(flags));
    // Other tests' serves may run first (one at a time).
    let deadline = Instant::now() + Duration::from_secs(900);
    loop {
        let announced = std::fs::read_dir(announce)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read(entry.path()).ok())
            .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .find_map(|value| value["origin"].as_str().map(str::to_owned));
        if let Some(origin) = announced {
            return Running { origin, handle };
        }
        assert!(
            !handle.is_finished(),
            "serve ended before announcing: {:?}",
            handle.join()
        );
        assert!(Instant::now() < deadline, "serve never announced");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// `GET path` on `origin`: the status and the JSON body (`Null` when none);
/// status 0 once the server has gone — including a server that shuts down
/// between accepting the connection and reading the request (a resumed
/// serve ending while it is sampled), whose socket refuses the write.
pub(super) fn get(origin: &str, path: &str) -> (u16, Value) {
    let address = origin.strip_prefix("http://").unwrap();
    let Ok(mut stream) = TcpStream::connect(address) else {
        return (0, Value::Null);
    };
    let mut response = String::new();
    let sent = stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .and_then(|()| {
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
            )
        })
        .and_then(|()| stream.read_to_string(&mut response));
    if sent.is_err() {
        return (0, Value::Null);
    }
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default();
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

/// `POST /api/ingestions/{id}/{action}` for the run `status` reports, as the
/// app's control does; returns the status code.
pub(super) fn control(origin: &str, status: &Value, action: &str, key: usize) -> u16 {
    let run = &status["daemon"]["ingest"]["run"];
    let address = origin.strip_prefix("http://").unwrap();
    let path = format!("/api/ingestions/{}/{action}", run["id"].as_str().unwrap());
    let body = serde_json::json!({"expected_revision": run["revision"]}).to_string();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nX-Rosetta-Index-Id: {}\r\nIdempotency-Key: 00000000-0000-4000-8000-{key:012}\r\nContent-Length: {}\r\n\r\n{body}",
        run["index_id"].as_str().unwrap(),
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

/// The staged generation a fresh build that did not publish leaves.
pub(super) fn staged_ledger(db: &Path) -> PathBuf {
    let ledger = db.with_extension("ledger");
    ledger.with_file_name(format!(
        "{}.staged",
        ledger.file_name().unwrap().to_string_lossy()
    ))
}

/// The paths whose newest raw generation no seal covers (`replay.rs`'s
/// unsealed map), each with that generation.
pub(super) fn unsealed(dir: &Path) -> std::collections::BTreeMap<String, u64> {
    use std::collections::BTreeMap;
    let mut raw: BTreeMap<String, u64> = BTreeMap::new();
    let mut sealed: BTreeMap<String, u64> = BTreeMap::new();
    for entry in scan_ledger(dir).unwrap().entries {
        match entry {
            LedgerEntry::Raw {
                source, generation, ..
            } => {
                let newest = raw.entry(source.relative_path).or_default();
                *newest = (*newest).max(generation);
            }
            LedgerEntry::GenerationComplete {
                source, generation, ..
            } => {
                let newest = sealed.entry(source.relative_path).or_default();
                *newest = (*newest).max(generation);
            }
            LedgerEntry::Document { .. } | LedgerEntry::Meta { .. } => {}
        }
    }
    raw.into_iter()
        .filter(|(path, generation)| sealed.get(path).is_none_or(|seal| seal < generation))
        .collect()
}

/// Every seal of the ledger at `dir`: path → newest sealed generation.
pub(super) fn seals(dir: &Path) -> std::collections::BTreeMap<String, u64> {
    let mut out = std::collections::BTreeMap::new();
    for entry in scan_ledger(dir).unwrap().entries {
        if let LedgerEntry::GenerationComplete {
            source, generation, ..
        } = entry
        {
            let newest: &mut u64 = out.entry(source.relative_path).or_default();
            *newest = (*newest).max(generation);
        }
    }
    out
}
