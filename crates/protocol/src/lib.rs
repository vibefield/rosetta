//! rosetta-protocol — the L3 wire contract (docs/ARCHITECTURE.md §8).
//!
//! One backend-neutral contract, per the 2026-08-26 search specification in
//! docs/ingest-engine.html: structured filters only (no engine query syntax
//! on the wire), ordering by score then a stable `timeline_id` tie-break,
//! numerical scores diagnostic-only, and every search response carrying
//! readiness + projection identity so a lagging index can never masquerade
//! as a complete empty result. Row payloads are the `rosetta-core` types
//! themselves (their serde shape IS the wire shape) — no DTO mirrors, the
//! spaghetti anti-pattern. TypeScript bindings generate from these types via
//! ts-rs under the `ts` feature; consumers never hand-write a second parser.
//!
//! Pagination follows the exact-watermark rule: a cursor binds the session
//! watermark at first page; on mismatch the server answers
//! [`ErrorCode::CursorExpired`] — match or expired, never a silent rebase.
//!
//! @atlas layer wire-types · domain api
//! @atlas rule thin-protocol "protocol depends on almost nothing" {
//!   only this -> crate:rosetta-core
//!   see "docs/ARCHITECTURE.md#8-workspace-layout"
//! }

use serde::{Deserialize, Serialize};

pub use rosetta_core::assets::ImageVariant;
pub use rosetta_core::rows::{
    Ms, ProjectRow, RuntimeProvenance, RuntimeRow, SessionActivity, SessionRole, SessionRow,
    StreamState, TokenSnapshot,
};
pub use rosetta_core::{TimelineBody, TimelineEntry, UsageCost};
pub use rosetta_search::{SearchHit, SearchReadiness, SearchState};
pub use rosetta_store::hot::{Mutation, Stamp};
pub use rosetta_store::queries::{
    ContributionRow, HeatmapDay, HourTotal, ModelDay, ModelHour, ResponseRuntime, Spend,
};
pub use rosetta_store::timeline_meta::{
    TimelineMeta, TimelineMetaArtifact, TimelineMetaArtifactSource, TimelineMetaPlan,
    TimelineMetaPlanSource, TimelineMetaTask, TimelineMetaWorkflow,
};
pub use rosetta_store::turns::{TurnGap, TurnSpan};

/// Bumped on any breaking change to routes or frame shapes.
/// 21 (2026-09-22): usage facts, read-time response cost, spend beside every
/// aggregate token figure, and `/api/pricing` (ARCHITECTURE §4.1.3).
/// 22 (2026-09-23): `TokenSnapshot.requests`, the request count of an aggregate.
/// 23 (2026-09-24): `unrecorded_output` beside every spend figure, and the
/// unpriced-model coverage report on `/api/pricing`.
/// 24 (2026-09-30): cumulative reported-cost adjustment beside aggregate spend.
/// 25 (2026-10-01): WebP image renditions on asset and canonical file-read URLs.
/// 27 (2026-10-09): `/api/activity`, every top-level session's turn spans in
/// a time window (ARCHITECTURE §5).
pub const PROTOCOL_VERSION: u32 = 27;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct StatusResponse {
    pub protocol_version: u32,
    pub schema_version: i64,
    /// The open projection passed fail-closed schema admission; canonical
    /// queries are serveable.
    pub canonical_ready: bool,
    pub search: SearchState,
    /// Live-daemon health; `None` when the projection is served without an
    /// ingest loop (tests, static serving).
    pub daemon: Option<DaemonStatus>,
}

/// The serve process's liveness surface — the health panel's one read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DaemonStatus {
    pub started_at: Ms,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub uptime_ms: u64,
    pub runtimes: Vec<RuntimeIngestStatus>,
    pub hub: HubStatus,
    /// Cold-start progress. Answers from process start, so the app can
    /// render honest meters before the projection is even readable.
    pub ingest: IngestProgress,
}

/// Where the daemon's ingest is in its life. `AwaitingConsent` means the
/// census is on the board and nothing has been read — the USER decides
/// which runtimes enroll. `MetadataReady` is the exact phase boundary where
/// every bounded skim has committed but no complete source record has been
/// streamed. `Live` means the watch loop owns the store and the cold backfill
/// is history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum IngestPhase {
    Discovering,
    AwaitingConsent,
    Backfilling,
    MetadataReady,
    Restoring,
    Finalizing,
    Publishing,
    Live,
}

/// One runtime's position in the cold backfill queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum BackfillState {
    Queued,
    Skimming,
    Streaming,
    Preparing,
    Applying,
    Done,
}

/// The ingest block on [`DaemonStatus`]: phase plus per-runtime meters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct IngestProgress {
    pub phase: IngestPhase,
    /// Installed runtimes in serve order.
    pub runtimes: Vec<RuntimeBackfill>,
    /// The ProgressBoard's per-project fold, largest plans first. Empty
    /// until the skims settle the plan, and absent from daemons that
    /// predate it.
    #[serde(default)]
    pub projects: Vec<ProjectBackfill>,
}

/// Cold-start progress for one project: the RAM board's fold of pass
/// outcomes over the settled ingest plan. Denominators are census figures;
/// `sessions_done` counts sessions whose last queued cold task finished.
/// Nothing here is durable — a restart rebuilds it from the plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ProjectBackfill {
    /// Canonical project id; `None` is the unassigned bucket (sessions
    /// whose ownership bounded meta could not prove).
    pub project_id: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub sessions_total: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub sessions_done: u64,
    /// Source-size estimate; zero when the payload total is unknown, including
    /// database/document snapshots and recovery from a partially captured run.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes_total: u64,
    /// Framed payload bytes processed for this project's sessions — never
    /// physical read amplification, so the meter tracks `bytes_total`
    /// honestly. May still run past it when sources grow mid-backfill;
    /// meters clamp.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes_streamed: u64,
}

/// Cold-start progress for one runtime. Denominators come from discovery's
/// census walk; numerators advance live while the writer streams, so both
/// meters are determinate: skims measure in files, streams in bytes.
///
/// Entries are keyed by `runtime_id` since 2026-09-12: a supervisor may enroll
/// several roots of one kind, so `kind` alone no longer identifies a row.
/// Both additions carry serde defaults, so an older daemon's block still parses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RuntimeBackfill {
    pub kind: String,
    /// Canonical runtime id (`RuntimeRow.id`); empty from a daemon that
    /// predates it.
    #[serde(default)]
    pub runtime_id: String,
    /// The enrollment file's presentation label, for enrolled runtimes.
    #[serde(default)]
    pub label: Option<String>,
    pub state: BackfillState,
    /// Census file denominator from discovery's walk.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub files_total: u64,
    /// Streamable source size; zero when the payload total is unknown (for
    /// example serialized database rows). It excludes inventory-only files
    /// once the fresh plan settles. Not an estimate of total ingest work.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes_total: u64,
    /// Skim meter numerator: files skimmed so far.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub files_skimmed: u64,
    /// Stream meter numerator: transcript bytes read so far.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes_streamed: u64,
    /// Sessions identified so far; advances during the bounded skim before
    /// the complete, ownership-settled metadata batch becomes public.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub sessions_total: u64,
    /// Transcript records parsed so far.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub records: u64,
}

/// One probed agent adapter — the roll call's row. Census facts only
/// (counts, sizes, a newest-touch stamp), never content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DiscoveryRow {
    pub kind: String,
    /// The layout's `~/...` form — machine-independent, names no home.
    pub data_root: String,
    /// The root exists and holds at least one file a layout stream claims.
    pub installed: bool,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub files: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes: u64,
    pub newest_modified_ms: Ms,
    /// Consent already granted — persisted from an earlier run, the CLI,
    /// or this run's enable call.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DiscoveriesResponse {
    pub discoveries: Vec<DiscoveryRow>,
}

/// Consent: the kinds the user chose on the roll call. Only installed
/// kinds are grantable, and only while the daemon is awaiting consent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct EnableRequest {
    pub kinds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct EnableResponse {
    pub enabled: Vec<String>,
}

/// Attention-directed ingestion: the reader opened a session that is still
/// in the backfill queue — move it to the front.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct BoostRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct BoostResponse {
    /// Recorded. Advisory: an id the queue never meets simply expires.
    pub queued: bool,
}

/// One watched runtime's live-ingest counters, cumulative since start.
/// Keyed by `runtime_id` since 2026-09-12 (see [`RuntimeBackfill`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RuntimeIngestStatus {
    pub kind: String,
    /// Canonical runtime id (`RuntimeRow.id`); empty from a daemon that
    /// predates it.
    #[serde(default)]
    pub runtime_id: String,
    /// The enrollment file's presentation label, for enrolled runtimes.
    #[serde(default)]
    pub label: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub ticks: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub events: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub polls: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub rescans: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub admissions: u64,
    /// Rows written by the live lane (inserts + replacements).
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub live_rows: u64,
    /// Last tick that moved data (events, admissions, rescans, or rows) —
    /// idle polling does not count as activity.
    pub last_activity_ms: Option<Ms>,
}

/// Hub fan-out counters, cumulative since start.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HubStatus {
    /// Live subscriptions across all topics.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub subscribers: u64,
    /// Warm per-session tail rings.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub rings: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub session_mutations: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub meta_events: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub resyncs: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub rings_evicted: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub overflow_rewarms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RuntimesResponse {
    pub runtimes: Vec<RuntimeRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ProjectsResponse {
    pub projects: Vec<ProjectRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SessionsResponse {
    pub sessions: Vec<SessionRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SessionResponse {
    pub session: SessionRow,
}

/// One backward page of a session timeline, oldest-first within the page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TimelinePage {
    pub entries: Vec<TimelineEntry>,
    /// Projection generation shared by the snapshot, pages, and exact reads.
    pub watermark: String,
    /// Opaque continuation for the next older page; `None` at the top of
    /// the timeline. Expires (HTTP 410, [`ErrorCode::CursorExpired`]) when
    /// the session's projection was rewritten under it.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TimelineEntryResponse {
    pub entry: TimelineEntry,
    pub watermark: String,
}

/// Complete shared speech for one team-member session. Private work is not
/// part of this response; response runtimes carry no content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CollaborationTimeline {
    pub entries: Vec<TimelineEntry>,
    pub response_runtimes: Vec<ResponseRuntime>,
}

/// Exact shell/code source recovered from the raw ledger only after an
/// explicit disclosure. Timeline pages never carry this payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct CommandResponse {
    pub command: String,
}

/// Internals of the opaque timeline cursor. Serialized as JSON inside the
/// opaque string; clients must never construct or inspect it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineCursor {
    pub session_id: String,
    /// Lowest ordinal already delivered; the next page is strictly older.
    pub before: i64,
    /// Exact commit watermark bound at first page.
    pub watermark: String,
}

impl TimelineCursor {
    pub fn encode(&self) -> String {
        serde_json::to_string(self).expect("cursor serialization is infallible")
    }

    /// `None` = malformed or foreign — treat exactly like an expired cursor.
    pub fn decode(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }
}

/// Search request: structured text plus filters. No backend query syntax.
/// Search covers prompts and replies only (2026-09-09): `kind` accepts `user`
/// or `agent`, and any other kind is HTTP 400 rather than an empty answer.
/// `is_error` is retained on the wire for tool-status filtering but is
/// rejected while tool activity is not indexed.
/// `agent` selects a runtime family (`claude-code`, `codex`, …), while
/// `runtime` selects one concrete runtime id.
///
/// Protocol 18 (2026-09-09) adds the live-monitor shape of a search: `sessions`
/// names the sessions a reader can jump to and `prefix` lets a remembered
/// fragment of a word match, so a monitor of running agents can ask one
/// question per keystroke instead of one per agent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SearchRequest {
    pub q: String,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub limit: Option<i64>,
    pub session: Option<String>,
    /// Comma-separated canonical session ids searched as one scope, at most
    /// 256. Each id is widened to its subagent tree, so a hit inside a
    /// delegated child still answers for the root the reader navigates to.
    /// An alternative to `session`, never combined with it.
    pub sessions: Option<String>,
    /// Widen every term of three or more characters to the tokens it begins;
    /// phrases stay exact. Absent means off.
    pub prefix: Option<bool>,
    pub runtime: Option<String>,
    pub agent: Option<String>,
    pub project: Option<String>,
    pub kind: Option<String>,
    pub is_error: Option<bool>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub after_ts: Option<Ms>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub before_ts: Option<Ms>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SearchResponse {
    pub state: SearchState,
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HeatmapResponse {
    pub days: Vec<HeatmapDay>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ContributionResponse {
    pub rows: Vec<ContributionRow>,
}

/// The price catalog behind every cost figure (ARCHITECTURE §4.1.3): one
/// list-price-equivalent basis for every agent, the compiled pricing version
/// (catalog plus calculator revision), and the version the projection's
/// rollup was priced with (the daemon re-prices on open when they differ).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PricingResponse {
    /// Always `list_price_equivalent`: what recorded usage would cost at the
    /// model creator's API list price. Provider-billed amounts are separate.
    pub basis: String,
    pub catalog_version: String,
    pub projection_version: Option<String>,
    pub models: u32,
    pub sources: Vec<PricingSource>,
    /// Coverage: every model in the open projection with usage the catalog
    /// could not price, largest first. A new model shows up here the day it
    /// is first used, before anyone notices a low figure.
    pub unpriced: Vec<UnpricedModel>,
}

/// One model's unpriced usage in the open projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct UnpricedModel {
    /// The model id as the agent recorded it.
    pub model: String,
    /// `unknown_model` when no catalog id or alias matches it; otherwise
    /// `unpriced` (a tier, route, missing rate or uncounted aggregate).
    pub reason: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub tokens: i64,
    /// First and last hour buckets holding unpriced usage, UTC ms.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub first_hour_ms: i64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub last_hour_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct PricingSource {
    pub name: String,
    pub url: String,
    pub revision: String,
    pub sha256: String,
    pub license: String,
}

/// What one project's heatmap figure decomposes into over the same window:
/// per-model days (they sum back to the heatmap's exactly) and an
/// hour-of-day profile. Both obey the request's timezone offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct UsageResponse {
    pub models: Vec<ModelDay>,
    pub hours: Vec<HourTotal>,
}

/// Hourly model usage for one calendar day at the request's timezone offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct DayUsageResponse {
    pub day: String,
    pub models: Vec<ModelHour>,
}

/// One top-level session with recorded activity in an `/api/activity`
/// window: its row as `/api/sessions` serves it, and its tree's turns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ActivitySession {
    pub session: SessionRow,
    /// The session tree's turns touching the window, oldest first.
    pub turns: Vec<TurnSpan>,
    /// True while the session's rows are behind the cold admission barrier:
    /// its turns are not public yet, so `turns` is empty without meaning
    /// "no turns".
    pub turns_withheld: bool,
}

/// `GET /api/activity`: which top-level sessions were active in a window and
/// when, newest recorded activity first. `truncated` says sessions beyond
/// the limit were left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ActivityResponse {
    pub since_ms: Ms,
    pub until_ms: Ms,
    /// The threshold a silence inside a turn passed to be reported as a gap.
    pub gap_ms: Ms,
    pub sessions: Vec<ActivitySession>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum ErrorCode {
    IncompatibleProtocol,
    NotFound,
    BadRequest,
    /// The requested derived surface is unavailable until canonical publish.
    NotReady,
    /// The pagination watermark no longer matches: refetch from the head.
    CursorExpired,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

/// Server→client WebSocket frames. The subscribe protocol is the hub's:
/// `Hello` advertises the target position; only an applied snapshot or complete
/// replay (`CaughtUp`) acknowledges it. Mutations continue from there (at-least-once; clients key by
/// `timeline_id`), and `Resync` announces a barrier — the server re-stamps
/// and sends a fresh snapshot immediately after.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "frame", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum ServerFrame {
    Hello {
        protocol_version: u32,
        stamp: Option<Stamp>,
    },
    /// Recent window of one session's timeline, oldest-first; older history
    /// pages through the REST timeline endpoint.
    TimelineSnapshot {
        stamp: Stamp,
        entries: Vec<TimelineEntry>,
        timeline_meta: TimelineMeta,
        cursor: Option<String>,
        watermark: String,
    },
    /// Commits the resume position only after the entire replay was applied.
    /// Also completes a reconnect with an empty replay.
    CaughtUp {
        stamp: Stamp,
    },
    /// Complete shared conversation; private work stays behind exact lazy reads.
    CollaborationSnapshot {
        entries: Vec<TimelineEntry>,
        response_runtimes: Vec<ResponseRuntime>,
    },
    ResponseRuntime {
        observation: ResponseRuntime,
    },
    /// A coalesced replacement of the session work index after a relevant
    /// committed mutation. Bodies remain behind their lazy read references.
    TimelineMetaSnapshot {
        stamp: Option<Stamp>,
        timeline_meta: TimelineMeta,
    },
    /// The full metadata surface, served from the resident mirror.
    MetaSnapshot {
        runtimes: Vec<RuntimeRow>,
        projects: Vec<ProjectRow>,
        sessions: Vec<SessionRow>,
    },
    /// Live mutations carry a stamp on session streams. Replayed mutations
    /// carry None and commit together at CaughtUp; collaboration/meta are unstamped.
    Mutation {
        stamp: Option<Stamp>,
        mutation: Mutation,
    },
    /// Barrier: discard buffered state; a fresh snapshot follows.
    Resync,
    Error {
        error: ApiError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_reject_foreign_text() {
        let cursor = TimelineCursor {
            session_id: "s1".into(),
            before: 42,
            watermark: "2.3.7".into(),
        };
        assert_eq!(TimelineCursor::decode(&cursor.encode()), Some(cursor));
        assert_eq!(TimelineCursor::decode("not json"), None);
        assert_eq!(TimelineCursor::decode("{\"v\":9}"), None);
    }

    #[test]
    fn readiness_serializes_in_spec_vocabulary() {
        assert_eq!(
            serde_json::to_string(&SearchReadiness::CatchingUp).unwrap(),
            "\"catching-up\""
        );
        assert_eq!(
            serde_json::to_string(&SearchReadiness::Ready).unwrap(),
            "\"ready\""
        );
    }

    #[test]
    fn server_frames_tag_by_frame_kind() {
        let frame = ServerFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            stamp: Some(Stamp { epoch: 7, seq: 3 }),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
        assert_eq!(json["frame"], "hello");
        assert_eq!(json["stamp"]["epoch"], 7);

        let resync = serde_json::to_string(&ServerFrame::Resync).unwrap();
        assert_eq!(resync, "{\"frame\":\"resync\"}");
    }
}
