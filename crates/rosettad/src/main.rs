//! rosettad — ingestion, live watching, and ledger-backed projection rebuilds.
//!
//!   rosettad discover [--home DIR]
//!   rosettad sources [--home DIR] [--agent KIND ...]
//!   rosettad streams [--agent KIND ...]
//!   rosettad meta --db PATH [--home DIR] [--agent KIND ...] [--profile]
//!   rosettad ingest --db PATH [--home DIR] [--agent KIND ...] [--enroll FILE] [--profile] [--audit-boundaries]
//!   rosettad watch --db PATH [--home DIR] [--agent KIND ...] [--enroll FILE] [--ticks N]
//!   rosettad serve --db PATH [--home DIR] [--agent KIND ...] [--enroll FILE] ...
//!                  [--token-file FILE [--grant-key-file FILE]] [--allow-origin ORIGIN ...]
//!   rosettad rebuild --db PATH [--ledger DIR] [--agent KIND ...] [--enroll FILE]
//!   rosettad ledger compact --db PATH [--ledger DIR] [--level N] [--long]
//!   rosettad ledger verify --db PATH | --ledger DIR [--json]
//!
//! `ingest` implies consent for the selected agents (the CLI user asked);
//! the app flow instead runs `discover`, asks, and flips `enabled` itself.
//! `--enroll FILE` (2026-09-12) is a supervisor's enrollment file: consent for
//! discovered kinds given at start, plus extra managed data roots of a kind
//! that are consented by construction — see `enroll.rs`.
//!
//! @atlas layer root · domain ingest
//! @atlas rule rosettad-is-root "Nothing depends on rosettad" {
//!   deny * -> this on code, manifest
//!   see "docs/ARCHITECTURE.md#8-workspace-layout"
//! }

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rosetta_adapter_antigravity::AntigravityAdapter;
use rosetta_adapter_claude_code::ClaudeCodeAdapter;
use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_copilot::CopilotAdapter;
use rosetta_adapter_grok::GrokAdapter;
use rosetta_adapter_hermes::HermesAdapter;
use rosetta_adapter_kimi_code::KimiCodeAdapter;
use rosetta_adapter_opencode::OpenCodeAdapter;
use rosetta_adapter_pi::PiAdapter;
use rosetta_adapter_support::AgentAdapter;
use rosetta_capture::{Ledger, NotifyEvents, SourceWake};
use rosetta_pipeline::{
    IngestOptions, Instrument, InstrumentSnapshot, LiveOptions, LiveRunner, MetaReport, Metric,
    PollEvidence, PreparedRuntime, RebuildOptions, StreamReport, apply_prepared_runtime,
    meta_ingest_into, meta_ingest_inventory_into, prepare_captured_runtime, prepare_runtime,
    rebuild_runtime_with_options, stream_backfill_into,
};
use rosetta_search::{
    Fts5SearchWorker, Fts5SidecarSearch, SearchBackend, SearchReadiness, synchronize_fts5_once,
};
use rosetta_server::RuntimeIdentity;
use rosetta_store::Store;

use crate::enroll::{Enrollment, RuntimeTarget, resolve_targets};
use crate::runtime_ledger::{LedgerRuntime, ledger_runtimes, open_runtime_ledger};

mod announce;
mod auth_files;
mod bench;
mod brutal;
mod cold_progress;
mod cold_stage;
mod cold_stream;
mod engine;
mod enroll;
mod ledger_cmd;
mod ledger_compression;
mod ledger_verify;
mod publication;
mod recovery;
mod runtime_ledger;
mod serve;

fn registry() -> Vec<&'static dyn AgentAdapter> {
    vec![
        &AntigravityAdapter,
        &ClaudeCodeAdapter,
        &CodexAdapter,
        &CopilotAdapter,
        &GrokAdapter,
        &HermesAdapter,
        &KimiCodeAdapter,
        &OpenCodeAdapter,
        &PiAdapter,
    ]
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("discover") => bench::cmd_discover(&args[1..]),
        Some("sources") => bench::cmd_sources(&args[1..]),
        Some("streams") => bench::cmd_streams(&args[1..]),
        Some("meta") => bench::cmd_meta(&args[1..]),
        Some("brutal-ingest") => brutal::cmd_brutal_ingest(&args[1..], registry()),
        Some("ingest") => cmd_ingest(&args[1..]),
        Some("watch") => cmd_watch(&args[1..]),
        Some("serve") => serve::cmd_serve(&args[1..]),
        Some("rebuild") => cmd_rebuild(&args[1..]),
        Some("ledger") => ledger_cmd::cmd_ledger(&args[1..]),
        _ => {
            eprintln!(
                "usage: rosettad <discover|sources|streams|meta|brutal-ingest|ingest|watch|serve|rebuild|ledger> [options]"
            );
            eprintln!("  discover [--home DIR] [--agent KIND ...] [--json]");
            eprintln!("  sources [--home DIR] [--agent KIND ...] [--json]");
            eprintln!("  streams [--agent KIND ...] [--json]");
            eprintln!("  meta --db PATH [--home DIR] [--agent KIND ...] [--profile] [--json]");
            eprintln!(
                "  brutal-ingest --db PATH [--home DIR] [--agent KIND ...] [--granularity file|line] [--durability none|fsync|sqlite-full] [--page-size 4096|65536] [--sqlite-location disk|memory] [--source-cache default|bypass] [--quick-check] [--json]"
            );
            eprintln!(
                "  ingest --db PATH [--home DIR] [--agent KIND ...] [--enroll FILE] [--profile] [--json] [--audit-boundaries]"
            );
            eprintln!(
                "  watch --db PATH [--home DIR] [--agent KIND ...] [--enroll FILE] [--ticks N] [--profile]"
            );
            eprintln!(
                "  serve --db PATH [--addr HOST:PORT] [--home DIR] [--agent KIND ...] [--enroll FILE] [--profile] [--stop-after-meta] [--restart-incomplete] [--slice-budget BYTES|0] [--capture-workers N|0] [--capture-lookahead BYTES] [--cold-ledger-codec plain|zstd-frames:LEVEL] [--announce [--announce-dir DIR]] [--token-file FILE [--grant-key-file FILE]] [--allow-origin ORIGIN ...] [--debug-auth]"
            );
            eprintln!(
                "  rebuild --db PATH [--ledger DIR] [--agent KIND ...] [--enroll FILE] [--profile]"
            );
            eprintln!(
                "  ledger compact --db PATH [--ledger DIR] [--level N] [--long] [--force] [--dry-run] [--json]"
            );
            eprintln!("  ledger verify --db PATH | --ledger DIR [--json]");
            eprintln!(
                "  (ingest and watch keep a raw ledger at <db>.ledger/ unless --ledger overrides)"
            );
            eprintln!(
                "  (--enroll FILE: a supervisor's JSON enrollment — {{\"format\":1,\"enabled_kinds\":[KIND...],\"runtimes\":[{{\"kind\",\"data_root\",\"label\"}}...]}};"
            );
            eprintln!(
                "   enabled_kinds consents discovered kinds at start, runtimes adds managed absolute roots that are consented by construction;"
            );
            eprintln!(
                "   the file is authoritative: with it, consent is exactly enabled_kinds plus --agent/--all-agents, so dropping a kind REVOKES it)"
            );
            eprintln!(
                "  (serve access, docs/AUTH.md: --token-file FILE holds the bearer (32+ visible ASCII chars; the file is trimmed) and then EVERY route and"
            );
            eprintln!(
                "   WebSocket needs `Authorization: Bearer <token>`; an allowed Origin is never a credential. --grant-key-file FILE (32+ raw bytes, needs"
            );
            eprintln!(
                "   --token-file) also accepts `Authorization: Rosetta-Grant <grant>` on the routes of the grant's one session. Both files must be"
            );
            eprintln!(
                "   owner-only (chmod 600) and are read once at start. --allow-origin ORIGIN (repeatable) adds a browser origin (scheme://host[:port])"
            );
            eprintln!(
                "   to the built-in loopback/app ones; --debug-auth logs each grant decision (client id and route) to stderr)"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rosettad: {message}");
            ExitCode::FAILURE
        }
    }
}

struct Flags {
    home: PathBuf,
    db: Option<PathBuf>,
    agents: Vec<String>,
    profile: bool,
    /// ingest only: emit one bounded, private before/after sample for steps 5–8.
    audit_boundaries: bool,
    /// Emit newline-delimited JSON events for repository tools.
    json: bool,
    /// `serve`: publish this daemon's origin in a per-user directory so
    /// sibling apps can find an ephemeral port (see `announce.rs`).
    announce: bool,
    announce_dir: Option<PathBuf>,
    /// serve only: the bearer every route then requires (docs/AUTH.md).
    /// Owner-only file, read once at start.
    token_file: Option<PathBuf>,
    /// serve only: the HMAC key per-session grants are verified with.
    /// Owner-only file, read once at start; needs `token_file`.
    grant_key_file: Option<PathBuf>,
    /// serve only: browser origins admitted beside the built-in ones.
    allow_origins: Vec<String>,
    /// serve only: one stderr line per grant decision (client id and route).
    debug_auth: bool,
    ticks: Option<u64>,
    ledger: Option<PathBuf>,
    /// serve only: the localhost listen address.
    addr: Option<String>,
    /// serve only: consent for every installed runtime from the command
    /// line — the headless stand-in for the app's roll call.
    all_agents: bool,
    /// The supervisor's enrollment file (`enroll.rs`): consent for
    /// discovered kinds plus managed extra roots. Read once at start.
    enroll: Option<PathBuf>,
    /// serve only: expose the completed metadata projection and keep serving,
    /// but do not read any complete source records.
    stop_after_meta: bool,
    /// serve only: preserve an interrupted first projection under recovery/
    /// and restart it through the production RAM cold path. The app enables
    /// this because a progressive projection was never canonical.
    restart_incomplete: bool,
    /// serve only: starter-slice byte budget for the cold scheduler.
    /// `None` = the blessed default; `Some(0)` disables slicing.
    slice_budget: Option<u64>,
    /// serve only: workers of the fresh cold build's shared capture pool.
    /// `None` = the pool's default size; `Some(0)` captures each runtime's
    /// sources one after another on its producer.
    capture_workers: Option<usize>,
    /// serve only: the capture pool's look-ahead bound in bytes; `None` =
    /// the default.
    capture_look_ahead: Option<u64>,
    /// serve only: the container of a fresh cold build's raw ledger. Plain by
    /// default (a background pass compresses closed segments after
    /// readiness); `zstd-frames:LEVEL` has the capture workers compress each
    /// batch instead.
    cold_ledger_codec: rosetta_capture::SegmentCodec,
    /// `ledger compact` only: the zstd level for rewritten segments.
    /// `None` = the write path's blessed default.
    level: Option<i32>,
    /// `ledger compact` only: rewrite with zstd's widest stock window
    /// (`MAX_WINDOW_LOG`, 128 MiB) for about 7% more ratio. The write path
    /// never pays for it, but every later decoder of those segments allocates
    /// that window while the segment is open — a replay across nine runtimes
    /// or the exact-command fetch included.
    long_window: bool,
    /// `ledger compact` only: rewrite segments that are already compressed,
    /// for instance to a higher level or a wider window.
    force: bool,
    /// `ledger compact` only: report what a pass would do and write nothing.
    dry_run: bool,
    /// Rebuild experiments only: trust the ordinary election/replay scans to
    /// detect corrupt tails instead of first reading the whole ledger once.
    experiment_skip_ledger_preflight: bool,
    /// Rebuild experiments only: discard the private projection immediately
    /// after ledger replay, before global finalization and publication.
    experiment_stop_after_replay: bool,
    /// Cold experiments: publish canonical SQLite without waiting for the
    /// disposable search sidecar.
    experiment_skip_search: bool,
    /// Rebuild experiments only: run the payload-free election pass and stop.
    experiment_election_only: bool,
    /// Rebuild-only operation count per private SQLite transaction.
    rebuild_chunk_ops: Option<usize>,
    /// Rebuild experiment: directory containing one writer-maintained
    /// generation-election cache per runtime.
    experiment_election_cache: Option<PathBuf>,
    /// Rebuild experiment: prefetch/checksum ledger frames concurrently with
    /// normalization and canonical application.
    experiment_prefetch_ledger: bool,
    /// Rebuild experiment: suppress per-row FK/CHECK enforcement inside the
    /// unpublished scratch projection. Final validation is deliberately not
    /// implied; this is a cost-floor measurement.
    experiment_unsafe_sqlite: bool,
    /// Rebuild experiment: keep the private projection's hot B-trees in a
    /// large process-local page cache and retain its exclusive lock. This is
    /// independently switchable from constraint suppression so the two costs
    /// remain measurable.
    experiment_sqlite_memory_cache: bool,
    /// Rebuild experiment: normalize every runtime concurrently into RAM,
    /// then feed closed source batches to the sole SQLite writer.
    experiment_parallel_prepare: bool,
    /// First-ingest experiment: capture and normalize installed runtimes in
    /// parallel into closed RAM batches, then feed the sole canonical writer
    /// in deterministic registry order.
    experiment_parallel_cold: bool,
    /// First-ingest lower bound: do not duplicate source bytes into the raw
    /// recovery ledger. This deliberately gives up ledger-backed rebuild and
    /// exists only to measure the readiness cost of that durability policy.
    experiment_skip_raw_ledger: bool,
    /// Apply completed runtimes from the smallest discovered source footprint
    /// upward, overlapping their canonical writes with the largest runtime's
    /// still-running capture. The byte census makes the order deterministic.
    experiment_cold_small_first: bool,
    /// Consume capture workers as they actually finish. This maximizes
    /// producer/writer overlap but makes surrogate SQLite row ids depend on
    /// scheduling; canonical ids and presentation ordinals remain stable.
    experiment_cold_completion_order: bool,
    /// Queue moved raw payloads to a bounded per-runtime ledger worker so
    /// checksumming/IO overlaps normalization and canonical application.
    experiment_async_ledger: bool,
    /// Build the private canonical projection in RAM, then stream its
    /// validated pages once to the staged SQLite file before publication.
    experiment_sqlite_in_memory: bool,
}

fn parse_flags(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        home: default_home()?,
        db: None,
        agents: Vec::new(),
        profile: false,
        audit_boundaries: false,
        json: false,
        announce: false,
        announce_dir: None,
        token_file: None,
        grant_key_file: None,
        allow_origins: Vec::new(),
        debug_auth: false,
        ticks: None,
        ledger: None,
        addr: None,
        all_agents: false,
        enroll: None,
        stop_after_meta: false,
        restart_incomplete: false,
        slice_budget: None,
        capture_workers: None,
        capture_look_ahead: None,
        cold_ledger_codec: DEFAULT_COLD_LEDGER_CODEC,
        level: None,
        long_window: false,
        force: false,
        dry_run: false,
        experiment_skip_ledger_preflight: false,
        experiment_stop_after_replay: false,
        experiment_skip_search: false,
        experiment_election_only: false,
        rebuild_chunk_ops: None,
        experiment_election_cache: None,
        experiment_prefetch_ledger: false,
        experiment_unsafe_sqlite: false,
        experiment_sqlite_memory_cache: false,
        experiment_parallel_prepare: false,
        experiment_parallel_cold: false,
        experiment_skip_raw_ledger: false,
        experiment_cold_small_first: false,
        experiment_cold_completion_order: false,
        experiment_async_ledger: false,
        experiment_sqlite_in_memory: false,
    };
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--home" => {
                flags.home = PathBuf::from(take_value(args, &mut at)?);
            }
            "--db" => {
                flags.db = Some(PathBuf::from(take_value(args, &mut at)?));
            }
            "--agent" => {
                flags.agents.push(take_value(args, &mut at)?);
            }
            "--profile" => {
                flags.profile = true;
                at += 1;
            }
            "--audit-boundaries" => {
                flags.audit_boundaries = true;
                at += 1;
            }
            "--all-agents" => {
                flags.all_agents = true;
                at += 1;
            }
            "--enroll" => {
                flags.enroll = Some(PathBuf::from(take_value(args, &mut at)?));
            }
            "--stop-after-meta" => {
                flags.stop_after_meta = true;
                at += 1;
            }
            "--restart-incomplete" => {
                flags.restart_incomplete = true;
                at += 1;
            }
            "--slice-budget" => {
                let value = take_value(args, &mut at)?;
                flags.slice_budget = Some(parse_byte_size("--slice-budget", &value)?);
            }
            "--capture-workers" => {
                let value = take_value(args, &mut at)?;
                flags.capture_workers = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--capture-workers needs a number, got {value}"))?,
                );
            }
            "--capture-lookahead" => {
                let value = take_value(args, &mut at)?;
                let bytes = parse_byte_size("--capture-lookahead", &value)?;
                if bytes == 0 {
                    return Err("--capture-lookahead must be greater than zero".to_string());
                }
                flags.capture_look_ahead = Some(bytes);
            }
            "--cold-ledger-codec" => {
                let value = take_value(args, &mut at)?;
                flags.cold_ledger_codec = parse_cold_ledger_codec(&value)?;
            }
            "--level" => {
                let value = take_value(args, &mut at)?;
                flags.level = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--level needs a number, got {value}"))?,
                );
            }
            "--long" => {
                flags.long_window = true;
                at += 1;
            }
            "--force" => {
                flags.force = true;
                at += 1;
            }
            "--dry-run" => {
                flags.dry_run = true;
                at += 1;
            }
            "--experiment-skip-ledger-preflight" => {
                flags.experiment_skip_ledger_preflight = true;
                at += 1;
            }
            "--experiment-stop-after-replay" => {
                flags.experiment_stop_after_replay = true;
                at += 1;
            }
            "--experiment-skip-search" => {
                flags.experiment_skip_search = true;
                at += 1;
            }
            "--experiment-election-only" => {
                flags.experiment_election_only = true;
                at += 1;
            }
            "--rebuild-chunk-ops" => {
                let value = take_value(args, &mut at)?;
                let parsed = value.parse::<usize>().map_err(|_| {
                    format!("--rebuild-chunk-ops needs a positive integer, got {value}")
                })?;
                if parsed == 0 {
                    return Err("--rebuild-chunk-ops must be greater than zero".to_string());
                }
                flags.rebuild_chunk_ops = Some(parsed);
            }
            "--experiment-election-cache" => {
                flags.experiment_election_cache = Some(PathBuf::from(take_value(args, &mut at)?));
            }
            "--experiment-prefetch-ledger" => {
                flags.experiment_prefetch_ledger = true;
                at += 1;
            }
            "--experiment-unsafe-sqlite" => {
                flags.experiment_unsafe_sqlite = true;
                at += 1;
            }
            "--experiment-sqlite-memory-cache" => {
                flags.experiment_sqlite_memory_cache = true;
                at += 1;
            }
            "--experiment-parallel-prepare" => {
                flags.experiment_parallel_prepare = true;
                at += 1;
            }
            "--experiment-parallel-cold" => {
                flags.experiment_parallel_cold = true;
                at += 1;
            }
            "--experiment-skip-raw-ledger" => {
                flags.experiment_skip_raw_ledger = true;
                at += 1;
            }
            "--experiment-cold-small-first" => {
                flags.experiment_cold_small_first = true;
                at += 1;
            }
            "--experiment-cold-completion-order" => {
                flags.experiment_cold_completion_order = true;
                at += 1;
            }
            "--experiment-async-ledger" => {
                flags.experiment_async_ledger = true;
                at += 1;
            }
            "--experiment-sqlite-in-memory" => {
                flags.experiment_sqlite_in_memory = true;
                at += 1;
            }
            "--json" => {
                flags.json = true;
                at += 1;
            }
            "--announce" => {
                flags.announce = true;
                at += 1;
            }
            "--announce-dir" => {
                flags.announce_dir = Some(PathBuf::from(take_value(args, &mut at)?));
            }
            "--token-file" => {
                flags.token_file = Some(PathBuf::from(take_value(args, &mut at)?));
            }
            "--grant-key-file" => {
                flags.grant_key_file = Some(PathBuf::from(take_value(args, &mut at)?));
            }
            "--allow-origin" => {
                flags.allow_origins.push(take_value(args, &mut at)?);
            }
            "--debug-auth" => {
                flags.debug_auth = true;
                at += 1;
            }
            "--ledger" => {
                flags.ledger = Some(PathBuf::from(take_value(args, &mut at)?));
            }
            "--addr" => {
                flags.addr = Some(take_value(args, &mut at)?);
            }
            "--ticks" => {
                let value = take_value(args, &mut at)?;
                flags.ticks = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--ticks needs a number, got {value}"))?,
                );
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(flags)
}

/// `256k`-style byte sizes for the slice budget: plain digits, or a `k`/`m`
/// suffix (KiB/MiB). `0` means slicing off.
fn parse_byte_size(flag: &str, value: &str) -> Result<u64, String> {
    let lower = value.trim().to_ascii_lowercase();
    let (digits, unit) = match lower.strip_suffix(['k', 'm']) {
        Some(digits) => (
            digits,
            if lower.ends_with('k') {
                1024
            } else {
                1024 * 1024
            },
        ),
        None => (lower.as_str(), 1),
    };
    let number: u64 = digits
        .parse()
        .map_err(|_| format!("{flag} wants BYTES, `NNNk`, `NNNm`, or 0 — got {value}"))?;
    number
        .checked_mul(unit)
        .ok_or_else(|| format!("{flag} {value} overflows"))
}

/// The fresh and resumed cold builds' raw-ledger codec when `--cold-ledger-codec`
/// is not given: capture workers compress each batch into its own zstd frame.
/// Level 1 against plain on the live all-agent corpus (2026-09-28): the same
/// readiness, +12.5 CPU-s per start, 11 GB fewer bytes written, a 4.4 GB instead
/// of a 15.6 GB ledger, and no hour-long recompression pass after readiness.
pub(crate) const DEFAULT_COLD_LEDGER_CODEC: rosetta_capture::SegmentCodec =
    rosetta_capture::SegmentCodec::ZstdFrames { level: 1 };

/// `--cold-ledger-codec`: `plain`, or `zstd-frames:LEVEL` with a zstd level
/// in −7..=19 other than 0 (zstd's alias for 3).
fn parse_cold_ledger_codec(value: &str) -> Result<rosetta_capture::SegmentCodec, String> {
    if value == "plain" {
        return Ok(rosetta_capture::SegmentCodec::Plain);
    }
    match value
        .strip_prefix("zstd-frames:")
        .and_then(|level| level.parse::<i32>().ok())
    {
        Some(level) if (-7..=19).contains(&level) && level != 0 => {
            Ok(rosetta_capture::SegmentCodec::ZstdFrames { level })
        }
        _ => Err(format!(
            "--cold-ledger-codec wants plain or zstd-frames:LEVEL (−7..=19, not 0), got {value}"
        )),
    }
}

fn take_value(args: &[String], at: &mut usize) -> Result<String, String> {
    let flag = &args[*at];
    let value = args
        .get(*at + 1)
        .ok_or_else(|| format!("{flag} needs a value"))?;
    *at += 2;
    Ok(value.clone())
}

/// The raw ledger lives beside the projection unless overridden — the
/// projection is the disposable one of the pair.
fn ledger_root(flags: &Flags, db: &std::path::Path) -> PathBuf {
    flags
        .ledger
        .clone()
        .unwrap_or_else(|| db.with_extension("ledger"))
}

/// A fresh cold ingest writes its ledger into a same-filesystem private
/// directory. The tree is promoted only after every segment and directory
/// entry is durable and immediately before the validated SQLite projection
/// is installed. A crash before promotion leaves an unscanned staging tree;
/// a crash after promotion leaves a complete ledger that `rebuild` can use.
#[derive(Debug)]
pub(crate) struct ColdLedgerStage {
    temp: tempfile::TempDir,
    destination: PathBuf,
    publication_barriers: Vec<PathBuf>,
}

impl ColdLedgerStage {
    pub(crate) fn new(destination: &std::path::Path) -> Result<Self, String> {
        match std::fs::symlink_metadata(destination) {
            Ok(_) => {
                return Err(format!(
                    "projection is missing but ledger {} already exists; use `rosettad rebuild` so generations cannot collide",
                    destination.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "stating ledger destination {}: {error}",
                    destination.display()
                ));
            }
        }

        let parent = projection_parent(destination);
        let publication_barriers = directory_publication_barriers(parent);
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        let temp = tempfile::Builder::new()
            .prefix(".rosetta-ledger-")
            .tempdir_in(parent)
            .map_err(|error| {
                format!(
                    "creating private ledger beside {}: {error}",
                    destination.display()
                )
            })?;
        Ok(Self {
            temp,
            destination: destination.to_path_buf(),
            publication_barriers,
        })
    }

    pub(crate) fn path(&self) -> &std::path::Path {
        self.temp.path()
    }

    pub(crate) fn publish(self) -> Result<(), String> {
        // The projection lock excludes Rosetta writers. Recheck anyway so an
        // unrelated process cannot silently replace a ledger destination.
        match std::fs::symlink_metadata(&self.destination) {
            Ok(_) => {
                return Err(format!(
                    "ledger destination {} appeared during cold ingest",
                    self.destination.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "restating ledger destination {}: {error}",
                    self.destination.display()
                ));
            }
        }
        std::fs::rename(self.temp.path(), &self.destination).map_err(|error| {
            format!(
                "atomically publishing ledger {}: {error}",
                self.destination.display()
            )
        })?;
        for path in &self.publication_barriers {
            std::fs::File::open(path)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("syncing ledger directory {}: {error}", path.display()))?;
        }
        Ok(())
    }
}

/// Child-first barriers for publishing a new entry in `parent`. If the
/// parent itself must be created, include every new directory plus the first
/// existing ancestor whose child entry changes.
fn directory_publication_barriers(parent: &std::path::Path) -> Vec<PathBuf> {
    let mut barriers = Vec::new();
    let mut cursor = parent;
    while !cursor.exists() {
        barriers.push(cursor.to_path_buf());
        let Some(next) = cursor.parent() else {
            return barriers;
        };
        cursor = next;
    }
    if barriers.is_empty() {
        barriers.push(parent.to_path_buf());
    } else {
        barriers.push(cursor.to_path_buf());
    }
    barriers
}

/// Cross-process writer/rebuild lock. SQLite owns the lock lifetime, so a
/// crash releases it automatically instead of leaving a stale pid file.
struct ProjectionLock {
    _connection: rusqlite::Connection,
}

fn projection_lock(db: &std::path::Path) -> Result<ProjectionLock, String> {
    let mut lock_name = db.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    if let Some(parent) = lock_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    let connection = rusqlite::Connection::open(&lock_path)
        .map_err(|error| format!("opening projection lock {}: {error}", lock_path.display()))?;
    connection
        .busy_timeout(Duration::ZERO)
        .map_err(|error| format!("configuring projection lock: {error}"))?;
    connection
        .execute_batch("PRAGMA journal_mode = DELETE; BEGIN EXCLUSIVE;")
        .map_err(|error| {
            format!(
                "projection {} is already owned by another ingest/watch/rebuild process: {error}",
                db.display()
            )
        })?;
    Ok(ProjectionLock {
        _connection: connection,
    })
}

/// The supervisor's enrollment file, validated before any work starts.
fn load_enrollment(flags: &Flags) -> Result<Option<Enrollment>, String> {
    flags
        .enroll
        .as_deref()
        .map(|path| Enrollment::load(path, &registry(), &flags.home))
        .transpose()
}

/// Consent for a finite command's targets, as runtime ids. Enrolled targets
/// are consented by construction. For discovered targets, `--agent` names
/// kinds outright; otherwise an `--enroll` file's `enabled_kinds` is the
/// headless roll call; without either, every installed adapter (the CLI user
/// asked). Only installed discovered targets can be consented.
///
/// The projection is never read here — a finite command's consent is this
/// invocation's file and command line, which is the authoritative rule `serve`
/// also follows wherever a file is given (2026-09-12).
fn cli_consent(
    flags: &Flags,
    enrollment: Option<&Enrollment>,
    targets: &[RuntimeTarget],
) -> std::collections::BTreeSet<String> {
    let mut consented: std::collections::BTreeSet<String> = targets
        .iter()
        .filter(|target| target.is_enrolled())
        .map(|target| target.id().to_string())
        .collect();
    match (flags.agents.is_empty(), enrollment) {
        (false, _) | (true, None) => consented.extend(
            targets
                .iter()
                .filter(|target| !target.is_enrolled() && target.installed())
                .map(|target| target.id().to_string()),
        ),
        (true, Some(enrollment)) => {
            let (ids, warnings) = enrollment.enabled_discovered(targets);
            for warning in warnings {
                eprintln!("{warning}");
            }
            consented.extend(ids);
        }
    }
    consented
}

/// Why a target is skipped by a finite command, or `None` when it runs.
fn skip_reason(
    target: &RuntimeTarget,
    consented: &std::collections::BTreeSet<String>,
) -> Option<&'static str> {
    if !target.installed() && !target.is_enrolled() {
        Some("not-installed")
    } else if !consented.contains(target.id()) {
        Some("not-enrolled")
    } else {
        None
    }
}

fn acknowledge_search(store: &mut Store, worker: &Fts5SearchWorker) -> Result<(), String> {
    for ack in worker.drain_acknowledgements() {
        store
            .acknowledge_search(&ack.projection_epoch, ack.revision)
            .map_err(|error| format!("acknowledging search revision: {error}"))?;
    }
    Ok(())
}

fn build_search_wait(db: &std::path::Path) -> Result<rosetta_search::SearchAck, String> {
    let search = Fts5SidecarSearch::new(db);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let ack = synchronize_fts5_once(&search, &stop)
        .map_err(|error| format!("building search projection: {error}"))?
        .ok_or_else(|| "canonical projection is not ready for search".to_string())?;
    if search.state().readiness != SearchReadiness::Ready {
        return Err("search projection did not become ready after synchronization".into());
    }
    Ok(ack)
}

fn synchronize_search_wait(db: &std::path::Path, store: &mut Store) -> Result<(), String> {
    let ack = build_search_wait(db)?;
    store
        .acknowledge_search(&ack.projection_epoch, ack.revision)
        .map_err(|error| format!("acknowledging search projection: {error}"))?;
    Ok(())
}

fn default_home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "no HOME in the environment; pass --home".to_string())
}

fn selected(flags: &Flags) -> Result<Vec<&'static dyn AgentAdapter>, String> {
    let all = registry();
    if flags.agents.is_empty() {
        return Ok(all);
    }
    flags
        .agents
        .iter()
        .map(|wanted| {
            all.iter()
                .copied()
                .find(|adapter| adapter.kind() == wanted)
                .ok_or_else(|| format!("unknown agent kind {wanted}"))
        })
        .collect()
}

pub(crate) fn record_backfill_failures(
    kind: &str,
    discovery_truncated: bool,
    meta: &rosetta_pipeline::MetaReport,
    stream: &rosetta_pipeline::StreamReport,
    failures: &mut Vec<String>,
) {
    if discovery_truncated {
        failures.push(format!("{kind} discovery walk was truncated"));
    }
    if meta.walk_truncated {
        failures.push(format!("{kind} meta walk was truncated"));
    }
    if meta.files_unidentified > 0 {
        failures.push(format!(
            "{kind} left {} streamable files unidentified",
            meta.files_unidentified
        ));
    }
    if meta.files_unfinished > 0 {
        failures.push(format!(
            "{kind} left {} streamable files with an unfinished first record",
            meta.files_unfinished
        ));
    }
    if meta.files_vanished > 0 {
        failures.push(format!(
            "{kind} lost {} files during meta ingest",
            meta.files_vanished
        ));
    }
    if stream.hard_errors > 0
        || stream.oversized > 0
        || stream.files_missing > 0
        || stream.retries_exhausted > 0
        || stream.conflicts > 0
        || stream.files_incomplete > 0
    {
        failures.push(format!(
            "{kind} ingest incomplete: {} hard, {} oversized, {} missing, {} retry-exhausted, {} conflicts, {} incomplete files",
            stream.hard_errors,
            stream.oversized,
            stream.files_missing,
            stream.retries_exhausted,
            stream.conflicts,
            stream.files_incomplete,
        ));
    }
}

/// [`record_backfill_failures`] for a fresh start's private build, which
/// refuses publication only for what no later read can repair: a truncated
/// walk hides sources, a checkpoint conflict means a second writer, and a
/// record past the raw frame limit was never captured. Every other shortfall
/// is a per-file imperfection the live path already absorbs — a file that
/// vanished, is not yet a session, or kept changing, and malformed records,
/// whose parseable rows publish with their session held Streaming — so the
/// build publishes past it; the returned line says what it tolerated.
pub(crate) fn record_fresh_failures(
    kind: &str,
    discovery_truncated: bool,
    meta: &rosetta_pipeline::MetaReport,
    stream: &rosetta_pipeline::StreamReport,
    failures: &mut Vec<String>,
) -> Option<String> {
    if discovery_truncated {
        failures.push(format!("{kind} discovery walk was truncated"));
    }
    if meta.walk_truncated {
        failures.push(format!("{kind} meta walk was truncated"));
    }
    if stream.oversized > 0 || stream.conflicts > 0 {
        failures.push(format!(
            "{kind} ingest incomplete: {} oversized, {} conflicts",
            stream.oversized, stream.conflicts,
        ));
    }
    let tolerated = [
        meta.files_vanished as u64,
        meta.files_unfinished as u64,
        meta.files_unidentified as u64,
        stream.files_missing as u64,
        stream.retries_exhausted,
        stream.hard_errors,
        stream.files_incomplete,
    ];
    tolerated.iter().any(|count| *count > 0).then(|| {
        format!(
            "{kind}: tolerated per-file imperfections (vanished={}, unfinished={}, unidentified={}, missing={}, unstable={}, hard_errors={}, incomplete={}); withheld files wait for live admission and affected sessions stay Streaming",
            meta.files_vanished,
            meta.files_unfinished,
            meta.files_unidentified,
            stream.files_missing,
            stream.retries_exhausted,
            stream.hard_errors,
            stream.files_incomplete,
        )
    })
}

pub(crate) struct CapturedColdRuntime {
    pub(crate) adapter: &'static dyn AgentAdapter,
    pub(crate) runtime: rosetta_core::rows::RuntimeRow,
    pub(crate) identity: RuntimeIdentity,
    pub(crate) root: PathBuf,
    pub(crate) roster: Vec<rosetta_pipeline::SessionFile>,
    pub(crate) discovery_truncated: bool,
    pub(crate) meta: MetaReport,
    pub(crate) stream: StreamReport,
    pub(crate) prepared: PreparedRuntime,
    pub(crate) ledger: Option<Ledger>,
    meta_profile: InstrumentSnapshot,
    stream_profile: InstrumentSnapshot,
    profile: InstrumentSnapshot,
    meta_wall_ms: f64,
    stream_wall_ms: f64,
    wall_ms: f64,
}

pub(crate) struct ColdCaptureRequest {
    pub(crate) adapter: &'static dyn AgentAdapter,
    pub(crate) runtime: rosetta_core::rows::RuntimeRow,
    pub(crate) identity: RuntimeIdentity,
    pub(crate) root: PathBuf,
    pub(crate) inventory: Vec<rosetta_capture::ScannedFile>,
    pub(crate) discovery_truncated: bool,
    pub(crate) ledger_root: PathBuf,
    pub(crate) opts: IngestOptions,
    pub(crate) write_raw_ledger: bool,
    pub(crate) async_raw_ledger: bool,
    pub(crate) ledger_codec: rosetta_capture::SegmentCodec,
    pub(crate) instrument: Arc<Instrument>,
}

pub(crate) struct PlannedColdRuntime {
    pub(crate) adapter: &'static dyn AgentAdapter,
    pub(crate) runtime: rosetta_core::rows::RuntimeRow,
    pub(crate) identity: RuntimeIdentity,
    pub(crate) root: PathBuf,
    pub(crate) files: Vec<rosetta_pipeline::SessionFile>,
    pub(crate) discovery_truncated: bool,
    pub(crate) meta: MetaReport,
    pub(crate) planner: Store,
    pub(crate) ledger: Option<Ledger>,
    pub(crate) instrument: Arc<Instrument>,
    opts: IngestOptions,
    meta_profile: InstrumentSnapshot,
    meta_wall_ms: f64,
    capture_started: Instant,
}

pub(crate) fn plan_parallel_cold_runtime(
    request: ColdCaptureRequest,
) -> Result<PlannedColdRuntime, String> {
    let ColdCaptureRequest {
        adapter,
        mut runtime,
        identity,
        root,
        inventory,
        discovery_truncated,
        ledger_root,
        opts,
        write_raw_ledger,
        async_raw_ledger,
        ledger_codec,
        instrument,
    } = request;
    runtime.enabled = true;
    let capture_started = Instant::now();
    let mut planner = Store::open_in_memory_for_cold_planning()
        .map_err(|error| format!("opening {} cold planner: {error}", adapter.kind()))?;
    let mut ledger = write_raw_ledger
        .then(|| open_runtime_ledger(&ledger_root, &runtime))
        .transpose()?
        .map(|ledger| ledger.with_codec(ledger_codec).defer_sync_until_flush())
        .map(|ledger| {
            if async_raw_ledger {
                ledger.into_async_bounded(runtime_ledger::COLD_LEDGER_QUEUE_BYTES)
            } else {
                ledger
            }
        });

    let meta_before = instrument.snapshot();
    let meta_started = Instant::now();
    let (meta, files) = meta_ingest_inventory_into(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &inventory,
        discovery_truncated,
        &opts,
        &instrument,
        ledger.as_mut(),
    )
    .map_err(|error| error.to_string())?;
    let meta_wall_ms = meta_started.elapsed().as_secs_f64() * 1_000.0;
    let meta_profile = instrument.snapshot().delta_since(&meta_before);

    Ok(PlannedColdRuntime {
        adapter,
        runtime,
        identity,
        root,
        files,
        discovery_truncated,
        meta,
        planner,
        ledger,
        instrument,
        opts,
        meta_profile,
        meta_wall_ms,
        capture_started,
    })
}

pub(crate) fn finish_parallel_cold_runtime(
    planned: PlannedColdRuntime,
) -> Result<CapturedColdRuntime, String> {
    let PlannedColdRuntime {
        adapter,
        runtime,
        identity,
        root,
        files,
        discovery_truncated,
        meta,
        mut planner,
        mut ledger,
        instrument,
        opts,
        meta_profile,
        meta_wall_ms,
        capture_started,
    } = planned;

    let stream_before = instrument.snapshot();
    let stream_started = Instant::now();
    let stream = stream_backfill_into(
        adapter,
        &runtime,
        &root,
        &mut planner,
        &files,
        &opts,
        &instrument,
        ledger.as_mut(),
    )
    .map_err(|error| error.to_string())?;
    let stream_wall_ms = stream_started.elapsed().as_secs_f64() * 1_000.0;
    let stream_profile = instrument.snapshot().delta_since(&stream_before);
    let prepared = instrument
        .time(Metric::ColdPrepareNanos, || {
            prepare_captured_runtime(adapter, &runtime, &files, &mut planner, &stream)
        })
        .map_err(|error| error.to_string())?;

    Ok(CapturedColdRuntime {
        adapter,
        runtime,
        identity,
        root,
        roster: files,
        discovery_truncated,
        meta,
        stream,
        prepared,
        ledger,
        meta_profile,
        stream_profile,
        profile: instrument.snapshot(),
        meta_wall_ms,
        stream_wall_ms,
        wall_ms: capture_started.elapsed().as_secs_f64() * 1_000.0,
    })
}

fn capture_parallel_cold_runtime(
    request: ColdCaptureRequest,
) -> Result<CapturedColdRuntime, String> {
    finish_parallel_cold_runtime(plan_parallel_cold_runtime(request)?)
}

pub(crate) struct AppliedColdRuntime {
    pub(crate) adapter: &'static dyn AgentAdapter,
    pub(crate) runtime: rosetta_core::rows::RuntimeRow,
    pub(crate) identity: RuntimeIdentity,
    pub(crate) root: PathBuf,
    pub(crate) roster: Vec<rosetta_pipeline::SessionFile>,
    pub(crate) ledger: Option<Ledger>,
    /// Committed cursor evidence for the live runner's first poll; empty
    /// where the writer did not collect it, and the poll reads cursor rows.
    pub(crate) poll_evidence: HashMap<String, PollEvidence>,
}

/// The log name of a runtime: its kind, with the enrollment label when one
/// exists.
pub(crate) fn identity_name(identity: &RuntimeIdentity) -> String {
    enroll::runtime_name(&identity.kind, identity.label.as_deref(), None)
}

pub(crate) fn apply_captured_cold_runtime(
    flags: &Flags,
    captured: CapturedColdRuntime,
    store: &mut Store,
    instrument: &Instrument,
    failures: &mut Vec<String>,
) -> Result<AppliedColdRuntime, String> {
    instrument.merge_snapshot(&captured.profile);
    let name = identity_name(&captured.identity);
    if flags.json && flags.profile {
        bench::emit(&serde_json::json!({
            "event": "stage-profile",
            "command": "ingest",
            "stage": "parallel-metadata",
            "agent": captured.adapter.kind(),
            "runtime": captured.runtime.id,
            "wallMs": captured.meta_wall_ms,
            "report": captured.meta_profile.report(),
        }));
        bench::emit(&serde_json::json!({
            "event": "stage-profile",
            "command": "ingest",
            "stage": "parallel-stream-prepare",
            "agent": captured.adapter.kind(),
            "runtime": captured.runtime.id,
            "wallMs": captured.stream_wall_ms,
            "captureWallMs": captured.wall_ms,
            "report": captured.stream_profile.report(),
        }));
        bench::emit(&serde_json::json!({
            "event": "stage-profile",
            "command": "ingest",
            "stage": "cold-plan-preparation",
            "agent": captured.adapter.kind(),
            "runtime": captured.runtime.id,
            "wallMs": captured.profile.get(Metric::ColdPrepareNanos) as f64 / 1_000_000.0,
        }));
    }
    if flags.json {
        bench::emit(&bench::meta_event(captured.adapter.kind(), &captured.meta));
        bench::emit(&serde_json::json!({
            "event": "stream-prepared",
            "agent": captured.adapter.kind(),
            "runtime": captured.runtime.id,
            "records": captured.stream.records,
            "parsed": captured.stream.parsed,
            "unknown": captured.stream.unknown,
            "hardErrors": captured.stream.hard_errors,
            "oversized": captured.stream.oversized,
            "quarantined": captured.stream.quarantined,
            "filesMissing": captured.stream.files_missing,
            "retriesExhausted": captured.stream.retries_exhausted,
            "conflicts": captured.stream.conflicts,
            "filesIncomplete": captured.stream.files_incomplete,
            "captureWallMs": captured.wall_ms,
        }));
    } else {
        println!(
            "{name}: prepared {} records and {} sessions in {:.2}s",
            captured.stream.records,
            captured.meta.sessions,
            captured.wall_ms / 1_000.0,
        );
    }
    record_backfill_failures(
        &name,
        captured.discovery_truncated,
        &captured.meta,
        &captured.stream,
        failures,
    );

    let apply_started = Instant::now();
    let apply_before = instrument.snapshot();
    let report = apply_prepared_runtime(
        captured.adapter,
        &captured.runtime,
        captured.prepared,
        store,
        instrument,
    )
    .map_err(|error| error.to_string())?;
    if flags.json && flags.profile {
        bench::emit_command_stage_profile(
            "ingest",
            "apply-prepared",
            Some(captured.adapter.kind()),
            apply_started,
            &apply_before,
            instrument,
        );
    }
    if flags.json {
        bench::emit(&serde_json::json!({
            "event": "canonical-applied",
            "agent": captured.adapter.kind(),
            "runtime": captured.runtime.id,
            "files": report.files,
            "messagesInserted": report.stats.messages_inserted,
            "messagesUpdated": report.stats.messages_updated,
            "toolResultsMerged": report.stats.tool_results_merged,
        }));
    }
    // A runtime's session ids are namespaced by that runtime, so no tool or
    // usage correlation can meet a later adapter. Reconcile this closed
    // runtime now; completion-order scheduling overlaps the work with larger
    // capture producers instead of leaving one global serial tail.
    instrument
        .flush_bulk_reconciliation(store)
        .map_err(|error| format!("reconciling {name}: {error}"))?;
    Ok(AppliedColdRuntime {
        adapter: captured.adapter,
        runtime: captured.runtime,
        identity: captured.identity,
        root: captured.root,
        roster: captured.roster,
        ledger: captured.ledger,
        poll_evidence: HashMap::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn ingest_parallel_cold(
    flags: &Flags,
    db: &std::path::Path,
    targets: &[RuntimeTarget],
    consented: &std::collections::BTreeSet<String>,
    opts: &IngestOptions,
    instrument: &Instrument,
    mut store: Store,
    temp: tempfile::TempPath,
    ledger_stage: ColdLedgerStage,
) -> Result<(), String> {
    if flags.audit_boundaries {
        return Err("--audit-boundaries is not supported by --experiment-parallel-cold".into());
    }
    store
        .use_precomputed_bulk_ordinals()
        .map_err(|error| format!("enabling precomputed cold ordinals: {error}"))?;
    if flags.experiment_unsafe_sqlite {
        store
            .conn()
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 PRAGMA ignore_check_constraints = ON;",
            )
            .map_err(|error| format!("configuring unsafe SQLite experiment: {error}"))?;
    }
    if flags.experiment_sqlite_memory_cache {
        store
            .conn()
            .execute_batch(
                "PRAGMA locking_mode = EXCLUSIVE;
                 PRAGMA temp_store = MEMORY;
                 PRAGMA cache_size = -2097152;",
            )
            .map_err(|error| format!("configuring SQLite memory-cache experiment: {error}"))?;
    }

    let ledger_root = ledger_stage.path().to_path_buf();
    let mut work = Vec::new();
    for target in targets {
        if let Some(reason) = skip_reason(target, consented) {
            if flags.json {
                bench::emit(&serde_json::json!({
                    "event": "skipped",
                    "phase": "ingest",
                    "agent": target.kind(),
                    "runtime": target.id(),
                    "reason": reason,
                }));
            } else {
                println!("{}: {reason}, skipping", target.name());
            }
            continue;
        }
        work.push((
            target.adapter,
            target.runtime.clone(),
            target.identity(),
            target.root.clone(),
            target.census.inventory.clone(),
            target.census.walk_truncated,
            target.census.streamable_bytes,
        ));
    }
    if work.is_empty() {
        return Err("nothing to ingest: no selected runtime is installed".into());
    }
    if flags.experiment_cold_small_first {
        work.sort_by(|left, right| {
            (left.6, left.0.kind(), &left.1.id).cmp(&(right.6, right.0.kind(), &right.1.id))
        });
    }

    let (mut ingest_ledgers, failures) = std::thread::scope(|scope| {
        let mut ledgers = Vec::with_capacity(work.len());
        let mut failures = Vec::new();
        if flags.experiment_cold_completion_order {
            let worker_count = work.len();
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut handles = Vec::with_capacity(worker_count);
            for (adapter, runtime, identity, root, inventory, discovery_truncated, _) in work {
                let ledger_root = ledger_root.clone();
                let opts = opts.clone();
                let sender = sender.clone();
                let write_raw_ledger = !flags.experiment_skip_raw_ledger;
                let async_raw_ledger = flags.experiment_async_ledger;
                handles.push(scope.spawn(move || {
                    let result = capture_parallel_cold_runtime(ColdCaptureRequest {
                        adapter,
                        runtime,
                        identity,
                        root,
                        inventory,
                        discovery_truncated,
                        ledger_root,
                        opts,
                        write_raw_ledger,
                        async_raw_ledger,
                        ledger_codec: rosetta_capture::SegmentCodec::default(),
                        instrument: Arc::new(Instrument::new()),
                    });
                    sender
                        .send(result)
                        .map_err(|_| "parallel cold consumer stopped".to_string())
                }));
            }
            drop(sender);
            for _ in 0..worker_count {
                let captured = receiver
                    .recv()
                    .map_err(|_| "parallel cold capture channel closed".to_string())??;
                if let Some(ledger) = apply_captured_cold_runtime(
                    flags,
                    captured,
                    &mut store,
                    instrument,
                    &mut failures,
                )?
                .ledger
                {
                    ledgers.push(ledger);
                }
            }
            for handle in handles {
                handle
                    .join()
                    .map_err(|_| "parallel cold capture worker panicked".to_string())??;
            }
        } else {
            let mut handles = Vec::with_capacity(work.len());
            for (adapter, runtime, identity, root, inventory, discovery_truncated, _) in work {
                let ledger_root = ledger_root.clone();
                let opts = opts.clone();
                let write_raw_ledger = !flags.experiment_skip_raw_ledger;
                let async_raw_ledger = flags.experiment_async_ledger;
                handles.push(scope.spawn(move || {
                    capture_parallel_cold_runtime(ColdCaptureRequest {
                        adapter,
                        runtime,
                        identity,
                        root,
                        inventory,
                        discovery_truncated,
                        ledger_root,
                        opts,
                        write_raw_ledger,
                        async_raw_ledger,
                        ledger_codec: rosetta_capture::SegmentCodec::default(),
                        instrument: Arc::new(Instrument::new()),
                    })
                }));
            }
            // Registry/size order keeps surrogate row ids deterministic.
            for handle in handles {
                let captured = handle
                    .join()
                    .map_err(|_| "parallel cold capture worker panicked".to_string())??;
                if let Some(ledger) = apply_captured_cold_runtime(
                    flags,
                    captured,
                    &mut store,
                    instrument,
                    &mut failures,
                )?
                .ledger
                {
                    ledgers.push(ledger);
                }
            }
        }
        Ok::<_, String>((ledgers, failures))
    })?;

    let stage_started = Instant::now();
    let stage_before = instrument.snapshot();
    ingest_ledgers.iter_mut().try_for_each(|ledger| {
        instrument
            .ledger_write(ledger, |ledger| ledger.flush())
            .map_err(|error| error.to_string())
    })?;
    if flags.json && flags.profile {
        bench::emit_stage_profile(
            "ledger-durability",
            None,
            stage_started,
            &stage_before,
            instrument,
        );
    }
    if !failures.is_empty() {
        if flags.json {
            bench::emit_profile("ingest", instrument);
        } else if flags.profile {
            println!("\n== profile ==\n{}", instrument.report());
        }
        return Err(failures.join("; "));
    }

    instrument
        .flush_bulk_reconciliation(&mut store)
        .map_err(|error| format!("reconciling whole projection: {error}"))?;
    drop(ingest_ledgers);
    let stage_started = Instant::now();
    let stage_before = instrument.snapshot();
    publish_private_projection(
        store,
        temp,
        db,
        instrument,
        |_| Ok(()),
        || {
            if flags.experiment_skip_raw_ledger {
                drop(ledger_stage);
                Ok(())
            } else {
                ledger_stage.publish()
            }
        },
    )?;
    if flags.json && flags.profile {
        bench::emit_stage_profile(
            "finalize-publish",
            None,
            stage_started,
            &stage_before,
            instrument,
        );
    }
    if !flags.experiment_skip_search {
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        instrument.time(Metric::SearchSyncNanos, || build_search_wait(db))?;
        if flags.json && flags.profile {
            bench::emit_stage_profile("search", None, stage_started, &stage_before, instrument);
        }
    }
    if flags.json {
        bench::emit_profile("ingest", instrument);
    } else if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    Ok(())
}

fn cmd_ingest(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("ingest needs --db PATH")?;
    if flags.audit_boundaries && (!flags.json || !flags.profile) {
        return Err("--audit-boundaries requires --json and --profile".to_string());
    }
    if flags.audit_boundaries && db.exists() {
        return Err("--audit-boundaries requires a new private cold projection".to_string());
    }
    let instrument = Instrument::new();
    if flags.audit_boundaries {
        instrument.enable_boundary_evidence();
    }
    let adapters = selected(&flags)?;
    let enrollment = load_enrollment(&flags)?;
    let opts = Enrollment::ingest_options(enrollment.as_ref());
    let _projection_lock = projection_lock(&db)?;

    let (mut store, private_projection) = open_for_backfill(&db)?;
    if flags.experiment_sqlite_in_memory {
        if private_projection.is_none() {
            return Err("--experiment-sqlite-in-memory requires a new private projection".into());
        }
        store = Store::open_in_memory_for_rebuild_with_assets(&db)
            .map_err(|error| format!("opening in-memory cold projection: {error}"))?;
    }
    let public_ledgers = ledger_root(&flags, &db);
    // Only a brand-new private projection may defer per-batch ledger sync.
    // Existing projections are public and retain the live durability order.
    let defer_ledger_sync = private_projection.is_some();
    let ledger_stage = defer_ledger_sync
        .then(|| ColdLedgerStage::new(&public_ledgers))
        .transpose()?;
    let ledgers = ledger_stage
        .as_ref()
        .map_or(public_ledgers.as_path(), ColdLedgerStage::path);
    let mut ingest_ledgers = Vec::new();
    let stage_started = Instant::now();
    let stage_before = instrument.snapshot();
    let targets = resolve_targets(
        &adapters,
        &flags.home,
        enrollment.as_ref(),
        &opts,
        &instrument,
    )?;
    let consented = cli_consent(&flags, enrollment.as_ref(), &targets);
    if flags.json && flags.profile {
        bench::emit_stage_profile("discovery", None, stage_started, &stage_before, &instrument);
    }
    if flags.experiment_parallel_cold {
        let temp = private_projection.ok_or_else(|| {
            "--experiment-parallel-cold requires a new private projection".to_string()
        })?;
        let ledger_stage = ledger_stage.expect("a private projection has a private ledger stage");
        return ingest_parallel_cold(
            &flags,
            &db,
            &targets,
            &consented,
            &opts,
            &instrument,
            store,
            temp,
            ledger_stage,
        );
    }
    let mut failures = Vec::new();

    for target in &targets {
        if let Some(reason) = skip_reason(target, &consented) {
            if flags.json {
                bench::emit(&serde_json::json!({
                    "event": "skipped",
                    "phase": "ingest",
                    "agent": target.kind(),
                    "runtime": target.id(),
                    "reason": reason,
                }));
            } else {
                println!("{}: {reason}, skipping", target.name());
            }
            continue;
        }
        let adapter = target.adapter;
        let name = target.name();
        let runtime = target.enabled_runtime(); // CLI ingest is explicit consent
        if target.census.walk_truncated {
            failures.push(format!("{name} discovery walk was truncated"));
        }
        let ledger = open_runtime_ledger(ledgers, &runtime)?;
        ingest_ledgers.push(if defer_ledger_sync {
            ledger.defer_sync_until_flush()
        } else {
            ledger
        });
        let ledger = ingest_ledgers
            .last_mut()
            .expect("the runtime ledger was just pushed");

        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        let (meta, files) = meta_ingest_into(
            adapter,
            &runtime,
            &target.root,
            &mut store,
            &opts,
            &instrument,
            Some(&mut *ledger),
        )
        .map_err(|error| error.to_string())?;
        if flags.json && flags.profile {
            bench::emit_stage_profile(
                "metadata",
                Some(adapter.kind()),
                stage_started,
                &stage_before,
                &instrument,
            );
        }
        if flags.json {
            bench::emit(&bench::meta_event(adapter.kind(), &meta));
        } else {
            println!(
                "{name}: meta — {} sessions, {} projects, {} files ({} inventory-only, {} unidentified)",
                meta.sessions,
                meta.projects,
                meta.files_matched,
                meta.files_inventory_only,
                meta.files_unidentified,
            );
        }
        if meta.walk_truncated {
            failures.push(format!("{name} meta walk was truncated"));
        }
        if meta.files_unidentified > 0 {
            failures.push(format!(
                "{name} left {} streamable files unidentified",
                meta.files_unidentified
            ));
        }
        if meta.files_unfinished > 0 {
            failures.push(format!(
                "{name} left {} streamable files with an unfinished first record",
                meta.files_unfinished
            ));
        }
        if meta.files_vanished > 0 {
            failures.push(format!(
                "{name} lost {} files during meta ingest",
                meta.files_vanished
            ));
        }

        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        let stream = stream_backfill_into(
            adapter,
            &runtime,
            &target.root,
            &mut store,
            &files,
            &opts,
            &instrument,
            Some(&mut *ledger),
        )
        .map_err(|error| error.to_string())?;
        if flags.json && flags.profile {
            bench::emit_stage_profile(
                "stream",
                Some(adapter.kind()),
                stage_started,
                &stage_before,
                &instrument,
            );
        }
        if flags.json {
            bench::emit(&serde_json::json!({
                "event": "stream",
                "agent": adapter.kind(),
                "runtime": runtime.id,
                "records": stream.records,
                "parsed": stream.parsed,
                "unknown": stream.unknown,
                "hardErrors": stream.hard_errors,
                "oversized": stream.oversized,
                "quarantined": stream.quarantined,
                "filesMissing": stream.files_missing,
                "retriesExhausted": stream.retries_exhausted,
                "conflicts": stream.conflicts,
                "filesIncomplete": stream.files_incomplete,
                "messagesInserted": stream.stats.messages_inserted,
                "messagesUpdated": stream.stats.messages_updated,
                "toolResultsMerged": stream.stats.tool_results_merged,
                "sessionsIndexed": stream.sessions_indexed,
            }));
        } else {
            println!(
                "{name}: stream — {} records ({} parsed, {} unknown, {} hard errors), \
                 {} rows inserted, {} tool results merged, {} sessions indexed",
                stream.records,
                stream.parsed,
                stream.unknown,
                stream.hard_errors,
                stream.stats.messages_inserted,
                stream.stats.tool_results_merged,
                stream.sessions_indexed,
            );
        }
        if stream.hard_errors > 0
            || stream.oversized > 0
            || stream.files_missing > 0
            || stream.retries_exhausted > 0
            || stream.conflicts > 0
            || stream.files_incomplete > 0
        {
            failures.push(format!(
                "{name} ingest incomplete: {} hard, {} oversized, {} missing, {} retry-exhausted, {} conflicts, {} incomplete files",
                stream.hard_errors,
                stream.oversized,
                stream.files_missing,
                stream.retries_exhausted,
                stream.conflicts,
                stream.files_incomplete,
            ));
        }
    }

    // A private projection has no externally visible commits yet. Make every
    // runtime ledger durable in one pre-publication barrier. Reported source
    // failures still cross the barrier before publication is refused; an IO
    // error may return earlier, but no private projection can be published.
    if defer_ledger_sync {
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        ingest_ledgers.iter_mut().try_for_each(|ledger| {
            instrument
                .ledger_write(ledger, |ledger| ledger.flush())
                .map_err(|error| error.to_string())
        })?;
        if flags.json && flags.profile {
            bench::emit_stage_profile(
                "ledger-durability",
                None,
                stage_started,
                &stage_before,
                &instrument,
            );
        }
    }

    if !failures.is_empty() {
        if flags.json {
            bench::emit_profile("ingest", &instrument);
        } else if flags.profile {
            println!("\n== profile ==\n{}", instrument.report());
        }
        return Err(failures.join("; "));
    }
    let boundary_before = if flags.audit_boundaries {
        let pipeline = instrument.boundary_evidence().unwrap_or_else(|| {
            serde_json::json!({
                "stream": null,
                "sourceOffset": null,
                "lineStatus": "no non-empty normalized record was available",
                "sampleTimelineId": null,
                "parsedRecord": null,
                "normalizedOps": null,
                "writeBatch": null,
            })
        });
        let timeline_id = pipeline
            .get("sampleTimelineId")
            .and_then(serde_json::Value::as_str);
        let started = Instant::now();
        let snapshot = bench::projection_snapshot(
            store.conn(),
            "after canonical apply / before global finalization",
            timeline_id,
            false,
        )?;
        Some((
            pipeline,
            snapshot,
            started.elapsed().as_secs_f64() * 1_000.0,
        ))
    } else {
        None
    };
    drop(ingest_ledgers);
    if let Some(temp) = private_projection {
        let ledger_stage = ledger_stage.expect("a private projection has a private ledger stage");
        let boundary_timeline_id = boundary_before
            .as_ref()
            .and_then(|(pipeline, _, _)| pipeline.get("sampleTimelineId"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let mut boundary_after = None;
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        publish_private_projection(
            store,
            temp,
            &db,
            &instrument,
            |finalized| {
                if flags.audit_boundaries {
                    let started = Instant::now();
                    let snapshot = bench::projection_snapshot(
                        finalized.conn(),
                        "after global finalization / before atomic publication",
                        boundary_timeline_id.as_deref(),
                        true,
                    )?;
                    boundary_after = Some((snapshot, started.elapsed().as_secs_f64() * 1_000.0));
                }
                Ok(())
            },
            || ledger_stage.publish(),
        )?;
        if flags.json && flags.profile {
            bench::emit_stage_profile(
                "finalize-publish",
                None,
                stage_started,
                &stage_before,
                &instrument,
            );
        }
        if let (Some((pipeline, before, before_ms)), Some((after, after_ms))) =
            (boundary_before, boundary_after)
        {
            bench::emit(&bench::boundary_evidence_event(
                &pipeline,
                before,
                after,
                before_ms + after_ms,
            ));
        }
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        instrument.time(Metric::SearchSyncNanos, || build_search_wait(&db))?;
        if flags.json && flags.profile {
            bench::emit_stage_profile("search", None, stage_started, &stage_before, &instrument);
        }
    } else {
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        instrument.time(Metric::SearchSyncNanos, || {
            synchronize_search_wait(&db, &mut store)
        })?;
        if flags.json && flags.profile {
            bench::emit_stage_profile("search", None, stage_started, &stage_before, &instrument);
        }
    }
    if flags.json {
        bench::emit_profile("ingest", &instrument);
    } else if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    Ok(())
}

/// Backfill, then hold the projection live: one watcher + runner per
/// installed runtime, round-robin ticks over a single writer. A hard kill is
/// always safe — every write is one transaction behind a checkpoint CAS.
fn cmd_watch(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("watch needs --db PATH")?;
    let live = LiveOptions::default();
    let wake = SourceWake::default();
    let instrument = Instrument::new();
    let adapters = selected(&flags)?;
    let enrollment = load_enrollment(&flags)?;
    let opts = Enrollment::ingest_options(enrollment.as_ref());
    let _projection_lock = projection_lock(&db)?;

    let mut store = open_progressive_backfill(&db)?;
    let progressive = store.is_progressive();
    let public_ledgers = ledger_root(&flags, &db);
    let search = Fts5SidecarSearch::new(db.clone());
    let search_worker = Fts5SearchWorker::start(search);
    let targets = resolve_targets(
        &adapters,
        &flags.home,
        enrollment.as_ref(),
        &opts,
        &instrument,
    )?;
    let consented = cli_consent(&flags, enrollment.as_ref(), &targets);

    let mut pending_runners = Vec::new();
    let mut failures = Vec::new();
    for target in &targets {
        if let Some(reason) = skip_reason(target, &consented) {
            println!("{}: {reason}, skipping", target.name());
            continue;
        }
        let adapter = target.adapter;
        let name = target.name();
        let runtime = target.enabled_runtime(); // watching is explicit consent

        // Watch BEFORE the backfill scan: a write landing mid-backfill still
        // produces a hint for the first live tick.
        let events = NotifyEvents::watch_with_wake(&target.root, &wake)
            .map_err(|error| error.to_string())?;
        let mut ledger = open_runtime_ledger(&public_ledgers, &runtime)?;

        let (meta, files) = meta_ingest_into(
            adapter,
            &runtime,
            &target.root,
            &mut store,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .map_err(|error| error.to_string())?;
        let stream = stream_backfill_into(
            adapter,
            &runtime,
            &target.root,
            &mut store,
            &files,
            &opts,
            &instrument,
            Some(&mut ledger),
        )
        .map_err(|error| error.to_string())?;
        search_worker.wake();
        acknowledge_search(&mut store, &search_worker)?;
        println!(
            "{name}: backfilled {} sessions ({} rows) — watching {}",
            meta.sessions,
            stream.stats.messages_inserted,
            target.root.display(),
        );
        record_backfill_failures(
            &name,
            target.census.walk_truncated,
            &meta,
            &stream,
            &mut failures,
        );

        pending_runners.push((
            adapter,
            runtime,
            target.identity(),
            target.root.clone(),
            events,
            Some(ledger),
        ));
    }
    if pending_runners.is_empty() {
        return Err("nothing to watch: no installed runtimes".to_string());
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }
    for (_, _, _, _, _, ledger) in &mut pending_runners {
        let ledger = ledger.as_mut().expect("a watch runner owns its ledger");
        instrument
            .ledger_write(ledger, |ledger| ledger.flush())
            .map_err(|error| error.to_string())?;
    }
    if progressive {
        instrument
            .finish_progressive_load(&mut store)
            .map_err(|error| format!("finalizing progressive projection: {error}"))?;
    }
    search_worker.wake();

    let mut runners = Vec::new();
    for (adapter, runtime, identity, root, events, ledger) in pending_runners {
        let ledger = match ledger {
            Some(ledger) => ledger,
            None => open_runtime_ledger(&public_ledgers, &runtime)?,
        };
        let runner = LiveRunner::start_with_ledger(
            adapter,
            runtime,
            root,
            Box::new(events),
            &mut store,
            opts.clone(),
            live.clone(),
            Some(ledger),
            &instrument,
        )
        .map_err(|error| error.to_string())?;
        runners.push((identity_name(&identity), runner, 0_u64));
    }

    let mut remaining = flags.ticks;
    let mut more_hints = false;
    loop {
        match &mut remaining {
            Some(0) => break,
            Some(count) => *count -= 1,
            None => {}
        }
        if !more_hints {
            wake.wait(live.tick);
        }
        more_hints = false;
        for (kind, runner, seen_batches) in &mut runners {
            more_hints |= runner
                .tick_ready(&mut store, &instrument)
                .map_err(|error| error.to_string())?;
            let report = runner.report();
            if report.stream.batches > *seen_batches {
                *seen_batches = report.stream.batches;
                println!(
                    "{kind}: live — {} rows, {} tool results, {} sessions touched",
                    report.stream.stats.messages_inserted + report.stream.stats.messages_updated,
                    report.stream.stats.tool_results_merged,
                    report.stream.sessions_indexed,
                );
            }
        }
        search_worker.wake();
        acknowledge_search(&mut store, &search_worker)?;
    }

    for (kind, runner, _) in &runners {
        let report = runner.report();
        println!(
            "{kind}: watched {} ticks — {} events ({} ignored), {} polls, {} rescans, {} admissions, {} rows",
            report.ticks,
            report.events,
            report.hints_ignored,
            report.polls,
            report.rescans,
            report.admissions,
            report.stream.stats.messages_inserted,
        );
    }
    if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    acknowledge_search(&mut store, &search_worker)?;
    search_worker.stop();
    Ok(())
}

/// Every runtime ledger under `ledgers`, each with the identity it replays
/// as: the stamp's (a discovered runtime's `~` root or an enrolled runtime's
/// absolute one), or the legacy per-kind convention for a stamp-less
/// directory. `--agent` selects by kind and may name every runtime of that
/// kind; `--enroll` only names a stamp-less enrolled directory.
fn rebuild_runtimes(
    flags: &Flags,
    ledgers: &std::path::Path,
    preflight: bool,
    enrollment: Option<&Enrollment>,
) -> Result<Vec<LedgerRuntime>, String> {
    let requested = selected(flags)?;
    let everything = ledger_runtimes(ledgers, &registry(), enrollment)?;
    let mut chosen: Vec<LedgerRuntime> = Vec::new();
    if flags.agents.is_empty() {
        chosen = everything;
    } else {
        let mut rest = Vec::new();
        for entry in everything {
            if requested
                .iter()
                .any(|adapter| adapter.kind() == entry.adapter.kind())
            {
                chosen.push(entry);
            } else {
                rest.push(entry);
            }
        }
        for adapter in &requested {
            if !chosen
                .iter()
                .any(|entry| entry.adapter.kind() == adapter.kind())
            {
                return Err(format!(
                    "no {} ledger at {}",
                    adapter.kind(),
                    ledgers.join(adapter.kind()).display()
                ));
            }
        }
        // A partial rebuild would silently erase every unselected runtime
        // from a shared projection. Require the selected set to cover every
        // ledger dir.
        if let Some(omitted) = rest.first() {
            return Err(format!(
                "ledger {} would be omitted; rebuild is whole-projection, so select every ledger runtime",
                omitted.dir_name()
            ));
        }
    }
    if chosen.is_empty() {
        return Err(format!("no runtime ledgers at {}", ledgers.display()));
    }

    if preflight {
        for entry in &chosen {
            let scan = rosetta_capture::visit_ledger::<rosetta_capture::CaptureError, _>(
                &entry.dir,
                |_| Ok(()),
            )
            .map_err(|error| error.to_string())?;
            if scan.entries == 0 {
                return Err(format!("{} ledger is empty", entry.dir.display()));
            }
            if scan.torn_segments > 0 {
                return Err(format!(
                    "{} has {} torn/corrupt segment tails; refusing to replace the projection",
                    entry.dir.display(),
                    scan.torn_segments
                ));
            }
        }
    }
    Ok(chosen)
}

fn projection_parent(db: &std::path::Path) -> &std::path::Path {
    db.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
}

/// Open the installed projection itself for a progressive first run. Unlike
/// private `ingest`/`rebuild`, every committed source is immediately readable;
/// durable projection_state records whether the final canonical gate has run.
fn open_progressive_backfill(db: &std::path::Path) -> Result<Store, String> {
    let parent = projection_parent(db);
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    Store::open_progressive(db)
        .map_err(|error| format!("opening progressive {}: {error:?}", db.display()))
}

/// A first ingest is a cold build, not a live mutation: construct it in a
/// private sibling file so derived indexes can be deferred without exposing
/// an inconsistent projection. Existing projections always stay in live
/// mode and retain per-transaction FTS/rollup maintenance.
fn open_for_backfill(db: &std::path::Path) -> Result<(Store, Option<tempfile::TempPath>), String> {
    if db.exists() {
        let store =
            Store::open(db).map_err(|error| format!("opening {}: {error:?}", db.display()))?;
        return Ok((store, None));
    }

    let parent = projection_parent(db);
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    let named = tempfile::Builder::new()
        .prefix(".rosetta-backfill-")
        .suffix(".sqlite")
        .tempfile_in(parent)
        .map_err(|error| {
            format!(
                "creating backfill database in {}: {error}",
                parent.display()
            )
        })?;
    let temp = named.into_temp_path();
    let store = Store::open_for_rebuild_with_assets(temp.as_ref(), db)
        .map_err(|error| format!("opening backfill database {}: {error:?}", temp.display()))?;
    Ok((store, Some(temp)))
}

fn publish_private_projection(
    mut store: Store,
    temp: tempfile::TempPath,
    db: &std::path::Path,
    instrument: &Instrument,
    after_finalize: impl FnOnce(&Store) -> Result<(), String>,
    before_install: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    instrument
        .finish_bulk_load(&mut store)
        .map_err(|error| format!("finalizing private projection: {error}"))?;
    after_finalize(&store)?;
    instrument.time(Metric::PublishNanos, || {
        if store.is_transient_memory() {
            publication::stage_ram_projection(store.conn(), &temp)
                .map_err(|error| format!("backing up in-memory projection: {error}"))?;
        }
        drop(store);
        let finished = rusqlite::Connection::open(&*temp)
            .map_err(|error| format!("reopening private projection: {error}"))?;
        make_single_file(&finished, "private projection")?;
        drop(finished);
        if let Ok(permissions) = std::fs::metadata(db).map(|metadata| metadata.permissions()) {
            std::fs::set_permissions(&*temp, permissions)
                .map_err(|error| format!("preserving projection permissions: {error}"))?;
        }
        std::fs::File::open(&*temp)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("syncing private projection: {error}"))?;
        before_install()?;
        install_rebuilt_projection(db, temp.as_ref())?;
        let _installed = temp.keep();
        // The old projection is no longer observable. Remove its unused PNG/JPEG
        // copies now, never during preparation or before atomic installation.
        let cleanup =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|error| error.to_string())
                .and_then(|installed| {
                    rosetta_store::assets::prune_unreferenced_images(&installed, db)
                        .map_err(|error| error.to_string())
                });
        if let Err(error) = cleanup {
            eprintln!("image cache cleanup deferred: {error}");
        }
        Ok(())
    })
}

fn sidecar(db: &std::path::Path, suffix: &str) -> PathBuf {
    let mut path = db.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

fn make_single_file(connection: &rusqlite::Connection, label: &str) -> Result<(), String> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| format!("configuring {label}: {error}"))?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|error| format!("checkpointing {label}: {error}"))?;
    let mode: String = connection
        .query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))
        .map_err(|error| format!("switching {label} to a single-file journal: {error}"))?;
    if mode != "delete" {
        return Err(format!("{label} stayed in unexpected journal mode {mode}"));
    }
    Ok(())
}

fn install_rebuilt_projection(db: &std::path::Path, temp: &std::path::Path) -> Result<(), String> {
    if db.exists() {
        let old = rusqlite::Connection::open(db)
            .map_err(|error| format!("opening old projection {}: {error}", db.display()))?;
        make_single_file(&old, "old projection")?;
        drop(old);
        for suffix in ["-wal", "-shm"] {
            let path = sidecar(db, suffix);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("removing {}: {error}", path.display())),
            }
        }
    }

    std::fs::rename(temp, db).map_err(|error| {
        format!(
            "atomically replacing {} with {}: {error}",
            db.display(),
            temp.display()
        )
    })?;
    std::fs::File::open(projection_parent(db))
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("syncing {}: {error}", projection_parent(db).display()))?;
    Ok(())
}

/// Rebuild into a private sibling database and atomically install it only
/// after every runtime replays cleanly. The live projection is never the work
/// surface, so validation or replay failure leaves it byte-for-byte usable.
fn cmd_rebuild(args: &[String]) -> Result<(), String> {
    let flags = parse_flags(args)?;
    let db = flags.db.clone().ok_or("rebuild needs --db PATH")?;
    let ledgers = ledger_root(&flags, &db);
    if !ledgers.is_dir() {
        return Err(format!("no ledger at {}", ledgers.display()));
    }
    let enrollment = load_enrollment(&flags)?;
    let instrument = Instrument::new();
    let policy = Enrollment::ingest_options(enrollment.as_ref()).exclude_cwds;
    let allow_retired_unsealed = !policy.is_empty() && !flags.experiment_election_only;
    let stage_started = Instant::now();
    let stage_before = instrument.snapshot();
    let runtimes = rebuild_runtimes(
        &flags,
        &ledgers,
        !flags.experiment_skip_ledger_preflight,
        enrollment.as_ref(),
    )?;
    if flags.json && flags.profile {
        bench::emit_command_stage_profile(
            "rebuild",
            "ledger-preflight",
            None,
            stage_started,
            &stage_before,
            &instrument,
        );
    }
    let _projection_lock = projection_lock(&db)?;

    let parent = projection_parent(&db);
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    let named = tempfile::Builder::new()
        .prefix(".rosetta-rebuild-")
        .suffix(".sqlite")
        .tempfile_in(parent)
        .map_err(|error| format!("creating rebuild database in {}: {error}", parent.display()))?;
    let temp = named.into_temp_path();

    let mut store = Store::open_for_rebuild_with_assets(temp.as_ref(), &db)
        .map_err(|error| format!("opening rebuild database {}: {error:?}", temp.display()))?;
    if flags.experiment_unsafe_sqlite {
        store
            .conn()
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
                 PRAGMA ignore_check_constraints = ON;",
            )
            .map_err(|error| format!("configuring unsafe SQLite experiment: {error}"))?;
    }
    if flags.experiment_sqlite_memory_cache {
        store
            .conn()
            .execute_batch(
                "PRAGMA locking_mode = EXCLUSIVE;
                 PRAGMA temp_store = MEMORY;
                 PRAGMA cache_size = -2097152;",
            )
            .map_err(|error| format!("configuring SQLite memory-cache experiment: {error}"))?;
    }
    let mut reports = Vec::new();
    if flags.experiment_parallel_prepare {
        store
            .use_precomputed_bulk_ordinals()
            .map_err(|error| format!("enabling precomputed cold ordinals: {error}"))?;
        let cache_root = flags.experiment_election_cache.clone().ok_or_else(|| {
            "--experiment-parallel-prepare requires --experiment-election-cache".to_string()
        })?;
        let prefetch_ledger = flags.experiment_prefetch_ledger;
        let prepared = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for entry in runtimes {
                let cache = cache_root.join(format!("{}.json", entry.dir_name()));
                let adapter = entry.adapter;
                let runtime = entry.runtime;
                let dir = entry.dir;
                handles.push(scope.spawn(move || {
                    let local = Instrument::new();
                    let started = Instant::now();
                    let prepared = prepare_runtime(
                        adapter,
                        &runtime,
                        &dir,
                        &local,
                        RebuildOptions {
                            chunk_ops: 1,
                            flush_reconciliation: false,
                            election_only: false,
                            election_cache: Some(cache),
                            prefetch_ledger,
                            allow_unsealed_sources: allow_retired_unsealed,
                            cancel: None,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                    Ok::<_, String>((
                        adapter,
                        runtime,
                        prepared,
                        local.snapshot(),
                        started.elapsed().as_secs_f64() * 1_000.0,
                    ))
                }));
            }
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "parallel preparation worker panicked".to_string())?
                })
                .collect::<Result<Vec<_>, String>>()
        })?;

        for (adapter, runtime, prepared, prepare_profile, prepare_wall_ms) in prepared {
            if prepared.report().hard_errors > 0 {
                return Err(format!(
                    "{}: {} hard errors during parallel preparation",
                    adapter.kind(),
                    prepared.report().hard_errors
                ));
            }
            instrument.merge_snapshot(&prepare_profile);
            if flags.json && flags.profile {
                bench::emit(&serde_json::json!({
                    "event": "stage-profile",
                    "command": "rebuild",
                    "stage": "parallel-prepare",
                    "agent": adapter.kind(),
                    "wallMs": prepare_wall_ms,
                    "report": prepare_profile.report(),
                }));
            }
            let stage_started = Instant::now();
            let profile_before = instrument.snapshot();
            let report =
                apply_prepared_runtime(adapter, &runtime, prepared, &mut store, &instrument)
                    .map_err(|error| error.to_string())?;
            if !flags.experiment_election_only {
                Enrollment::validate_rebuild_sources(
                    &policy,
                    &mut store,
                    adapter,
                    &runtime,
                    &report,
                    &instrument,
                )?;
            }
            let profile = instrument.snapshot().delta_since(&profile_before);
            if flags.json && flags.profile {
                bench::emit_command_stage_profile(
                    "rebuild",
                    "apply-prepared",
                    Some(adapter.kind()),
                    stage_started,
                    &profile_before,
                    &instrument,
                );
            }
            reports.push((adapter.kind(), report, profile));
        }
    } else {
        for entry in runtimes {
            let stage_started = Instant::now();
            let profile_before = instrument.snapshot();
            let dir_name = entry.dir_name();
            let adapter = entry.adapter;
            let runtime = entry.runtime;
            let report = rebuild_runtime_with_options(
                adapter,
                &runtime,
                &entry.dir,
                &mut store,
                &instrument,
                RebuildOptions {
                    chunk_ops: flags.rebuild_chunk_ops.unwrap_or(512),
                    flush_reconciliation: false,
                    election_only: flags.experiment_election_only,
                    election_cache: flags
                        .experiment_election_cache
                        .as_ref()
                        .map(|root| root.join(format!("{dir_name}.json"))),
                    prefetch_ledger: flags.experiment_prefetch_ledger,
                    allow_unsealed_sources: allow_retired_unsealed,
                    cancel: None,
                },
            )
            .map_err(|error| error.to_string())?;
            if !flags.experiment_election_only {
                Enrollment::validate_rebuild_sources(
                    &policy,
                    &mut store,
                    adapter,
                    &runtime,
                    &report,
                    &instrument,
                )?;
            }
            if report.hard_errors > 0 {
                return Err(format!(
                    "{}: {} hard errors during replay",
                    adapter.kind(),
                    report.hard_errors
                ));
            }
            if report.torn_segments > 0 {
                return Err(format!(
                    "{}: ledger changed or tore during replay",
                    adapter.kind()
                ));
            }
            let profile = instrument.snapshot().delta_since(&profile_before);
            if flags.json && flags.profile {
                bench::emit_command_stage_profile(
                    "rebuild",
                    "replay",
                    Some(adapter.kind()),
                    stage_started,
                    &profile_before,
                    &instrument,
                );
            }
            reports.push((adapter.kind(), report, profile));
        }
    }

    // The private projection has no observers between runtimes. Keeping every
    // runtime's lifecycle fragments in RAM lets us scan existing tool rows and
    // fold correlations exactly once for the whole projection.
    instrument
        .flush_bulk_reconciliation(&mut store)
        .map_err(|error| format!("reconciling whole projection: {error}"))?;

    if !flags.experiment_stop_after_replay && !flags.experiment_election_only {
        let stage_started = Instant::now();
        let stage_before = instrument.snapshot();
        publish_private_projection(store, temp, &db, &instrument, |_| Ok(()), || Ok(()))?;
        if flags.json && flags.profile {
            bench::emit_command_stage_profile(
                "rebuild",
                "finalize-publish",
                None,
                stage_started,
                &stage_before,
                &instrument,
            );
        }
        if !flags.experiment_skip_search {
            let stage_started = Instant::now();
            let stage_before = instrument.snapshot();
            instrument.time(Metric::SearchSyncNanos, || build_search_wait(&db))?;
            if flags.json && flags.profile {
                bench::emit_command_stage_profile(
                    "rebuild",
                    "search",
                    None,
                    stage_started,
                    &stage_before,
                    &instrument,
                );
            }
        }
    }

    for (kind, report, profile) in reports {
        if !flags.json {
            println!(
                "{kind}: rebuilt {} files from {} segments — {} records replayed \
             ({} stale, {} duplicate, {} already projected), {} meta batches, \
             {} rows inserted, {} sessions stubbed",
                report.files,
                report.segments,
                report.raw_records,
                report.stale_records_skipped,
                report.duplicate_records_skipped,
                report.records_already_projected,
                report.meta_batches,
                report.stats.messages_inserted,
                report.sessions_stubbed,
            );
            if flags.profile {
                print_agent_profile(kind, &profile);
            }
        }
    }
    if flags.json && flags.profile {
        bench::emit_profile("rebuild", &instrument);
    } else if flags.profile {
        println!("\n== profile ==\n{}", instrument.report());
    }
    Ok(())
}

fn print_agent_profile(kind: &str, profile: &InstrumentSnapshot) {
    println!("\n== profile: {kind} ==\n{}", profile.report());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_every_implemented_adapter() {
        let kinds = registry()
            .into_iter()
            .map(AgentAdapter::kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                "antigravity",
                "claude-code",
                "codex",
                "copilot",
                "grok",
                "hermes",
                "kimi-code",
                "opencode",
                "pi",
            ]
        );
    }

    #[test]
    fn standalone_meta_refuses_an_existing_projection() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("existing.sqlite");
        std::fs::write(&db, b"sentinel").unwrap();
        let error = bench::cmd_meta(&[
            "--db".into(),
            db.display().to_string(),
            "--home".into(),
            root.path().display().to_string(),
            "--agent".into(),
            "codex".into(),
        ])
        .unwrap_err();
        assert!(error.contains("needs new paths"));
        assert_eq!(std::fs::read(db).unwrap(), b"sentinel");
    }

    #[test]
    fn cold_ledger_stage_is_private_until_atomic_promotion() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("projection.ledger");
        let stage = ColdLedgerStage::new(&destination).unwrap();
        let staged_path = stage.path().to_path_buf();
        let mut ledger = Ledger::open(&staged_path.join("codex"))
            .unwrap()
            .defer_sync_until_flush();
        ledger
            .append_meta(rosetta_pipeline::LEDGER_META_SCHEMA_VERSION, "[]")
            .unwrap();
        ledger.checkpoint().unwrap();
        assert!(!destination.exists());
        ledger.flush().unwrap();
        drop(ledger);

        stage.publish().unwrap();
        assert!(!staged_path.exists());
        assert!(destination.join("codex/00000000.rledger").is_file());
        assert!(ColdLedgerStage::new(&destination).is_err());
    }

    #[test]
    fn cold_ledger_stage_refuses_a_preexisting_empty_destination() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("projection.ledger");
        std::fs::create_dir(&destination).unwrap();
        let error = ColdLedgerStage::new(&destination).unwrap_err();
        assert!(error.contains("use `rosettad rebuild`"));
    }

    /// A fresh start publishes past every per-file imperfection, naming them
    /// in one line, where the strict gate refuses each; both refuse what no
    /// later read can repair.
    #[test]
    fn a_fresh_start_refuses_only_what_no_later_read_repairs() {
        let meta = MetaReport {
            files_vanished: 1,
            files_unfinished: 2,
            files_unidentified: 3,
            ..MetaReport::default()
        };
        let stream = StreamReport {
            files_missing: 4,
            retries_exhausted: 5,
            hard_errors: 6,
            files_incomplete: 7,
            ..StreamReport::default()
        };
        let mut failures = Vec::new();
        let line = record_fresh_failures("codex", false, &meta, &stream, &mut failures);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(
            line.as_deref(),
            Some(
                "codex: tolerated per-file imperfections (vanished=1, unfinished=2, \
                 unidentified=3, missing=4, unstable=5, hard_errors=6, incomplete=7); \
                 withheld files wait for live admission and affected sessions stay Streaming"
            )
        );
        let mut strict = Vec::new();
        record_backfill_failures("codex", false, &meta, &stream, &mut strict);
        assert_eq!(strict.len(), 4, "{strict:?}");

        let mut failures = Vec::new();
        let clean = record_fresh_failures(
            "codex",
            false,
            &MetaReport::default(),
            &StreamReport::default(),
            &mut failures,
        );
        assert_eq!((clean, failures.len()), (None, 0));

        for (label, discovery_truncated, meta, stream, refusal) in [
            (
                "discovery",
                true,
                MetaReport::default(),
                StreamReport::default(),
                "discovery walk was truncated",
            ),
            (
                "metadata",
                false,
                MetaReport {
                    walk_truncated: true,
                    ..MetaReport::default()
                },
                StreamReport::default(),
                "meta walk was truncated",
            ),
            (
                "oversized",
                false,
                MetaReport::default(),
                StreamReport {
                    oversized: 1,
                    ..StreamReport::default()
                },
                "1 oversized, 0 conflicts",
            ),
            (
                "conflict",
                false,
                MetaReport::default(),
                StreamReport {
                    conflicts: 1,
                    ..StreamReport::default()
                },
                "0 oversized, 1 conflicts",
            ),
        ] {
            for fresh in [true, false] {
                let mut failures = Vec::new();
                if fresh {
                    record_fresh_failures(
                        "codex",
                        discovery_truncated,
                        &meta,
                        &stream,
                        &mut failures,
                    );
                } else {
                    record_backfill_failures(
                        "codex",
                        discovery_truncated,
                        &meta,
                        &stream,
                        &mut failures,
                    );
                }
                assert_eq!(failures.len(), 1, "{label}, fresh {fresh}: {failures:?}");
                if fresh {
                    assert!(failures[0].contains(refusal), "{label}: {failures:?}");
                }
            }
        }
    }

    fn args(db: &std::path::Path, ledger: &std::path::Path, agent: &str) -> Vec<String> {
        vec![
            "--db".to_string(),
            db.display().to_string(),
            "--ledger".to_string(),
            ledger.display().to_string(),
            "--agent".to_string(),
            agent.to_string(),
        ]
    }

    fn sentinel(db: &std::path::Path) {
        let connection = rusqlite::Connection::open(db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sentinel(value TEXT NOT NULL);
                 INSERT INTO sentinel VALUES ('keep-me');",
            )
            .unwrap();
    }

    fn assert_sentinel(db: &std::path::Path) {
        let connection = rusqlite::Connection::open(db).unwrap();
        let value: String = connection
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "keep-me");
    }

    #[test]
    fn unknown_agent_is_validated_before_the_live_projection_is_touched() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let ledgers = temp.path().join("ledger");
        std::fs::create_dir(&ledgers).unwrap();
        sentinel(&db);

        assert!(cmd_rebuild(&args(&db, &ledgers, "not-an-agent")).is_err());
        assert_sentinel(&db);
    }

    #[test]
    fn failed_replay_keeps_the_old_projection() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let ledgers = temp.path().join("ledger");
        sentinel(&db);
        let mut ledger = rosetta_capture::Ledger::open(&ledgers.join("claude-code")).unwrap();
        ledger.append_meta(999, "[]").unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        let error = cmd_rebuild(&args(&db, &ledgers, "claude-code")).unwrap_err();
        assert!(error.contains("unsupported ledger meta schema"));
        assert_sentinel(&db);
    }

    #[test]
    fn successful_replay_atomically_replaces_the_old_projection() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let ledgers = temp.path().join("ledger");
        sentinel(&db);
        let mut ledger = rosetta_capture::Ledger::open(&ledgers.join("claude-code")).unwrap();
        ledger
            .append_meta(rosetta_pipeline::LEDGER_META_SCHEMA_VERSION, "[]")
            .unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        cmd_rebuild(&args(&db, &ledgers, "claude-code")).unwrap();
        let connection = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM agent_runtime", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert!(connection.prepare("SELECT * FROM sentinel").is_err());
        assert!(!sidecar(&db, "-wal").exists());
    }

    #[test]
    fn first_backfill_stays_private_until_finalized_and_published() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let (store, private) = open_for_backfill(&db).unwrap();
        assert!(!db.exists());
        let private = private.expect("a missing projection takes the cold path");
        assert!(private.exists());

        publish_private_projection(
            store,
            private,
            &db,
            &Instrument::new(),
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap();
        assert!(db.exists());
        assert!(!sidecar(&db, "-wal").exists());
        Store::open(&db).unwrap();
    }

    #[test]
    fn in_memory_backfill_is_backed_up_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let (disk_store, private) = open_for_backfill(&db).unwrap();
        drop(disk_store);
        let private = private.expect("a missing projection takes the cold path");

        let store = Store::open_in_memory_for_rebuild_with_assets(&db).unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TABLE memory_sentinel(value TEXT NOT NULL);
                 INSERT INTO memory_sentinel VALUES ('from-memory');",
            )
            .unwrap();
        publish_private_projection(
            store,
            private,
            &db,
            &Instrument::new(),
            |_| Ok(()),
            || Ok(()),
        )
        .unwrap();

        let connection = rusqlite::Connection::open(&db).unwrap();
        let value: String = connection
            .query_row("SELECT value FROM memory_sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "from-memory");
        assert!(!sidecar(&db, "-wal").exists());
        Store::open(&db).unwrap();
    }

    #[test]
    fn existing_backfill_target_stays_live() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        drop(Store::open(&db).unwrap());
        let (_store, private) = open_for_backfill(&db).unwrap();
        assert!(private.is_none());
    }

    #[test]
    fn projection_lock_excludes_a_second_writer() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("projection.db");
        let first = projection_lock(&db).unwrap();
        assert!(projection_lock(&db).is_err());
        drop(first);
        assert!(projection_lock(&db).is_ok());
    }

    #[test]
    fn slice_budget_sizes_parse_with_suffixes() {
        let parse = |value| parse_byte_size("--slice-budget", value);
        assert_eq!(parse("0").unwrap(), 0);
        assert_eq!(parse("4096").unwrap(), 4096);
        assert_eq!(parse("256k").unwrap(), 256 * 1024);
        assert_eq!(parse("1M").unwrap(), 1024 * 1024);
        assert!(parse("many").is_err());
        assert!(parse("1g").is_err());
    }
}
