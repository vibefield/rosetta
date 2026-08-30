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

use serde::{Deserialize, Serialize};

pub use rosetta_core::rows::{
    Ms, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState, TokenSnapshot,
};
pub use rosetta_core::{TimelineBody, TimelineEntry};
pub use rosetta_search::{SearchHit, SearchReadiness, SearchState};
pub use rosetta_store::hot::{Mutation, Stamp};
pub use rosetta_store::queries::{ContributionRow, HeatmapDay, HourTotal, ModelDay};

/// Bumped on any breaking change to routes or frame shapes.
pub const PROTOCOL_VERSION: u32 = 4;

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
    Done,
}

/// The ingest block on [`DaemonStatus`]: phase plus per-runtime meters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct IngestProgress {
    pub phase: IngestPhase,
    /// Installed runtimes in serve order.
    pub runtimes: Vec<RuntimeBackfill>,
}

/// Cold-start progress for one runtime. Denominators come from discovery's
/// census walk; numerators advance live while the writer streams, so both
/// meters are determinate: skims measure in files, streams in bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RuntimeBackfill {
    pub kind: String,
    pub state: BackfillState,
    /// Census denominators from discovery's walk.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub files_total: u64,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes_total: u64,
    /// Skim meter numerator: files skimmed so far.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub files_skimmed: u64,
    /// Stream meter numerator: transcript bytes read so far.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub bytes_streamed: u64,
    /// Sessions the skim identified; 0 until the skim lands.
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct RuntimeIngestStatus {
    pub kind: String,
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
    /// Opaque continuation for the next older page; `None` at the top of
    /// the timeline. Expires (HTTP 410, [`ErrorCode::CursorExpired`]) when
    /// the session's projection was rewritten under it.
    pub cursor: Option<String>,
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
/// `is_error` omitted includes true, false, and unreported; explicit false
/// never matches unreported tool state.
/// `agent` selects a runtime family (`claude-code`, `codex`, …), while
/// `runtime` selects one concrete runtime id.
/// `kind` uses the canonical wire vocabulary (`user`, `agent`, `tool_use`, …).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct SearchRequest {
    pub q: String,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub limit: Option<i64>,
    pub session: Option<String>,
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

/// What one project's heatmap figure decomposes into over the same window:
/// per-model days (they sum back to the heatmap's exactly) and an
/// hour-of-day profile. Both obey the request's timezone offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct UsageResponse {
    pub models: Vec<ModelDay>,
    pub hours: Vec<HourTotal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum ErrorCode {
    NotFound,
    BadRequest,
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
/// `Hello` stamps the position, a snapshot frame covers everything up to it,
/// mutations continue from there (at-least-once; clients key by
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
    },
    /// The full metadata surface, served from the resident mirror.
    MetaSnapshot {
        runtimes: Vec<RuntimeRow>,
        projects: Vec<ProjectRow>,
        sessions: Vec<SessionRow>,
    },
    /// Replayed ring history between the client's resume stamp and `Hello`'s
    /// stamp. Same shape as live mutations.
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
