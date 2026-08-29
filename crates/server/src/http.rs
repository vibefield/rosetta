//! The REST surface: axum over `store::queries` for canonical row reads, the
//! search backend boundary for ranked retrieval, and the hub's resident
//! mirror for list/browse (which therefore never touch the DB).
//! Every SQLite read opens its own read-only connection against the
//! published projection file inside `spawn_blocking` — the single writer
//! owns its connection exclusively, and WAL keeps readers unblocked.
//!
//! Pagination follows the exact-watermark rule: the timeline cursor binds
//! `queries::session_watermark` at first page and answers HTTP 410
//! (`cursor_expired`) on any mismatch — never a silent rebase.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

use rosetta_protocol::{
    ApiError, BoostRequest, BoostResponse, ContributionResponse, DiscoveriesResponse,
    EnableRequest, EnableResponse, ErrorCode, HeatmapResponse, PROTOCOL_VERSION, ProjectsResponse,
    RuntimesResponse, SearchRequest, SearchResponse, SessionResponse, SessionsResponse,
    StatusResponse, TimelineCursor, TimelinePage, UsageResponse,
};
use rosetta_search::{Fts5Search, SearchBackend, SearchError, SearchQuery, SearchState, TextQuery};
use rosetta_store::queries;
use rosetta_store::{StoreError, schema};

use crate::hub::Hub;

const DEFAULT_LIST_LIMIT: i64 = 100;
const DEFAULT_SEARCH_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 1_000;

#[derive(Clone)]
pub struct AppState {
    pub hub: Arc<Hub>,
    /// The published projection file; readers open their own connections.
    pub db: Arc<PathBuf>,
    /// Search is deliberately independent from canonical row reads. The
    /// shipped backend points at `db`; a detached backend can report its own
    /// readiness and answer without changing this transport contract.
    pub search: Arc<dyn SearchBackend>,
    /// Daemon liveness, present only when a live ingest loop feeds it.
    pub health: Option<Arc<crate::health::DaemonHealth>>,
    /// The roll call's board and gate, present only on a consent-aware serve.
    pub consent: Option<Arc<crate::consent::ConsentGate>>,
    /// The boost queue, present only while a serve loop drains it.
    pub boost: Option<Arc<crate::consent::BoostLedger>>,
}

impl AppState {
    /// Build the shipped synchronous-FTS5 service state.
    pub fn new(hub: Arc<Hub>, db: PathBuf) -> Self {
        let search = Arc::new(Fts5Search::new(db.clone()));
        Self {
            hub,
            db: Arc::new(db),
            search,
            health: None,
            consent: None,
            boost: None,
        }
    }

    /// Inject a backend while retaining the same canonical projection. Used
    /// by sidecars and by readiness/coherence contract tests.
    pub fn with_search(hub: Arc<Hub>, db: PathBuf, search: Arc<dyn SearchBackend>) -> Self {
        Self {
            hub,
            db: Arc::new(db),
            search,
            health: None,
            consent: None,
            boost: None,
        }
    }

    /// Attach the daemon's health surface (the serve loop updates it).
    pub fn with_daemon_health(mut self, health: Arc<crate::health::DaemonHealth>) -> Self {
        self.health = Some(health);
        self
    }

    /// Attach the consent gate (the serve loop arms and awaits it).
    pub fn with_consent(mut self, consent: Arc<crate::consent::ConsentGate>) -> Self {
        self.consent = Some(consent);
        self
    }

    /// Attach the boost ledger (the serve loop drains it between files).
    pub fn with_boost(mut self, boost: Arc<crate::consent::BoostLedger>) -> Self {
        self.boost = Some(boost);
        self
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/discoveries", get(discoveries))
        // The two writes a reader's intent can make. Both carry the same
        // origin guard the WS upgrades do: a browser page from a foreign
        // origin is refused outright, preflight or not.
        .route(
            "/api/runtimes/enable",
            axum::routing::post(enable)
                .route_layer(axum::middleware::from_fn(crate::ws::require_local_origin)),
        )
        .route(
            "/api/ingest/boost",
            axum::routing::post(boost)
                .route_layer(axum::middleware::from_fn(crate::ws::require_local_origin)),
        )
        .route("/api/runtimes", get(runtimes))
        .route("/api/projects", get(projects))
        .route("/api/projects/{id}/heatmap", get(heatmap))
        .route("/api/projects/{id}/usage", get(usage))
        .route("/api/projects/{id}/contribution", get(contribution))
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/{id}", get(session))
        .route("/api/sessions/{id}/timeline", get(timeline))
        .route("/api/search", get(search))
        .route(
            "/ws/meta",
            get(crate::ws::meta)
                .route_layer(axum::middleware::from_fn(crate::ws::require_local_origin)),
        )
        .route(
            "/ws/sessions/{id}",
            get(crate::ws::session)
                .route_layer(axum::middleware::from_fn(crate::ws::require_local_origin)),
        )
        .layer(cors_layer())
        .with_state(state)
}

/// Local-app CORS. The daemon serves personal transcript data on localhost,
/// so a permissive `Any` origin would hand every website in the user's
/// browser a read API — only local app origins (a dev server, the packaged
/// Tauri/Electron shell) may cross origins. Non-browser clients send no
/// Origin and are unaffected.
fn cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{AllowOrigin, CorsLayer};
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| origin_allowed(origin)))
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers(tower_http::cors::Any)
}

/// The origin policy, shared with the WebSocket upgrade guard: any port on a
/// loopback host, plus the packaged Tauri shell. A `null` origin (sandboxed
/// iframes, some file:// contexts) is deliberately NOT allowed — any site
/// can manufacture one.
pub(crate) fn origin_allowed(origin: &axum::http::HeaderValue) -> bool {
    let Ok(text) = origin.to_str() else {
        return false;
    };
    if text == "tauri://localhost" {
        return true;
    }
    let rest = match text.split_once("://") {
        Some(("http" | "https", rest)) => rest,
        _ => return false,
    };
    let host_port = rest.split('/').next().unwrap_or("");
    let host = match host_port.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|c| c.is_ascii_digit()) => name,
        _ => host_port,
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// A typed failure that renders as `{code, message}` with the right status.
pub(crate) struct Failure(StatusCode, ApiError);

impl Failure {
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiError {
                code: ErrorCode::Internal,
                message: message.into(),
            },
        )
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self(
            StatusCode::NOT_FOUND,
            ApiError {
                code: ErrorCode::NotFound,
                message: message.into(),
            },
        )
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self(
            StatusCode::BAD_REQUEST,
            ApiError {
                code: ErrorCode::BadRequest,
                message: message.into(),
            },
        )
    }

    fn cursor_expired() -> Self {
        Self(
            StatusCode::GONE,
            ApiError {
                code: ErrorCode::CursorExpired,
                message: "the timeline was rewritten; refetch from the head".into(),
            },
        )
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

pub(crate) fn open_read(db: &Path) -> Result<Connection, Failure> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| Failure::internal(format!("opening projection: {error}")))
}

/// Run one read against a fresh read-only connection on the blocking pool.
pub(crate) async fn with_read<T, F>(db: Arc<PathBuf>, read: F) -> Result<T, Failure>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T, Failure> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let conn = open_read(&db)?;
        read(&conn)
    })
    .await
    .map_err(|error| Failure::internal(format!("read task failed: {error}")))?
}

fn store_err(error: StoreError) -> Failure {
    Failure::internal(format!("{error}"))
}

fn clamp_limit(limit: Option<i64>, default: i64) -> i64 {
    limit.unwrap_or(default).clamp(1, MAX_LIMIT)
}

async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    let search_state = state.search.state();
    let daemon = state
        .health
        .as_ref()
        .map(|health| health.snapshot(&state.hub, now_ms()));
    let schema_version = with_read(state.db.clone(), |conn| {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| Failure::internal(format!("{error}")))
    })
    .await;
    let response = match schema_version {
        Ok(schema_version) => {
            let canonical_ready = schema_version == schema::SCHEMA_VERSION;
            StatusResponse {
                protocol_version: PROTOCOL_VERSION,
                schema_version,
                canonical_ready,
                // Synchronous FTS5 commits with canonical rows; a foreign
                // schema means neither surface is serveable.
                search: if canonical_ready {
                    search_state
                } else {
                    SearchState::failed()
                },
                daemon,
            }
        }
        // No readable projection — a fresh install still cold-loading its
        // private file. Status is the app's connectivity truth and answers
        // anyway; the daemon block carries the backfill meters.
        Err(_) => StatusResponse {
            protocol_version: PROTOCOL_VERSION,
            schema_version: 0,
            canonical_ready: false,
            search: SearchState::failed(),
            daemon,
        },
    };
    Json(response)
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// The roll call's census. Empty until discovery has run (or on a serve
/// without a consent gate, e.g. static serving).
async fn discoveries(State(state): State<AppState>) -> Json<DiscoveriesResponse> {
    Json(DiscoveriesResponse {
        discoveries: state
            .consent
            .as_ref()
            .map(|gate| gate.rows())
            .unwrap_or_default(),
    })
}

async fn enable(
    State(state): State<AppState>,
    Json(request): Json<EnableRequest>,
) -> Result<Json<EnableResponse>, Failure> {
    let Some(gate) = state.consent.as_ref() else {
        return Err(Failure::bad_request("this daemon takes no enrollment"));
    };
    match gate.grant(&request.kinds) {
        Ok(enabled) => Ok(Json(EnableResponse { enabled })),
        Err(crate::consent::GrantError::NotOpen) => Err(Failure::bad_request(
            "enrollment is closed — this daemon already has its consented set; restart to change it",
        )),
        Err(crate::consent::GrantError::NotGrantable) => Err(Failure::bad_request(
            "only installed agent kinds from /api/discoveries can enroll",
        )),
    }
}

async fn boost(
    State(state): State<AppState>,
    Json(request): Json<BoostRequest>,
) -> Result<Json<BoostResponse>, Failure> {
    let Some(ledger) = state.boost.as_ref() else {
        // No backfill loop to reorder — nothing to do, honestly said.
        return Ok(Json(BoostResponse { queued: false }));
    };
    if request.session_id.is_empty() {
        return Err(Failure::bad_request("boost needs a session_id"));
    }
    ledger.push(&request.session_id);
    Ok(Json(BoostResponse { queued: true }))
}

async fn runtimes(State(state): State<AppState>) -> Json<RuntimesResponse> {
    let mut runtimes = state.hub.runtimes();
    runtimes.sort_by(|a, b| a.id.cmp(&b.id));
    Json(RuntimesResponse { runtimes })
}

async fn projects(State(state): State<AppState>) -> Json<ProjectsResponse> {
    let mut projects = state.hub.projects();
    projects.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    Json(ProjectsResponse { projects })
}

#[derive(serde::Deserialize)]
struct SessionListQuery {
    project: Option<String>,
    limit: Option<i64>,
}

async fn sessions(
    State(state): State<AppState>,
    Query(query): Query<SessionListQuery>,
) -> Json<SessionsResponse> {
    let limit = clamp_limit(query.limit, DEFAULT_LIST_LIMIT) as usize;
    let mut sessions = state.hub.sessions();
    if let Some(project) = &query.project {
        sessions.retain(|session| session.project_id.as_deref() == Some(project));
    }
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    sessions.truncate(limit);
    Json(SessionsResponse { sessions })
}

async fn session(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<SessionResponse>, Failure> {
    match state.hub.session(&id) {
        Some(session) => Ok(Json(SessionResponse { session })),
        None => Err(Failure::not_found(format!("no session {id}"))),
    }
}

#[derive(serde::Deserialize)]
struct TimelineQuery {
    limit: Option<i64>,
    cursor: Option<String>,
}

async fn timeline(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<TimelineQuery>,
) -> Result<Json<TimelinePage>, Failure> {
    let limit = clamp_limit(query.limit, DEFAULT_LIST_LIMIT);
    let page = with_read(state.db.clone(), move |conn| {
        if queries::session(conn, &id).map_err(store_err)?.is_none() {
            return Err(Failure::not_found(format!("no session {id}")));
        }
        let watermark = queries::session_watermark(conn, &id).map_err(store_err)?;
        let before = match &query.cursor {
            None => None,
            Some(text) => {
                let cursor = TimelineCursor::decode(text).ok_or_else(Failure::cursor_expired)?;
                if cursor.session_id != id || cursor.watermark != watermark {
                    return Err(Failure::cursor_expired());
                }
                Some(cursor.before)
            }
        };
        let messages = queries::timeline_page(conn, &id, before, limit).map_err(store_err)?;
        let cursor = if messages.len() as i64 == limit {
            messages.first().map(|oldest| {
                TimelineCursor {
                    session_id: id.clone(),
                    before: oldest.ordinal,
                    watermark,
                }
                .encode()
            })
        } else {
            None
        };
        Ok(TimelinePage { messages, cursor })
    })
    .await?;
    Ok(Json(page))
}

async fn search(
    State(state): State<AppState>,
    Query(request): Query<SearchRequest>,
) -> Result<Json<SearchResponse>, Failure> {
    let limit = clamp_limit(request.limit, DEFAULT_SEARCH_LIMIT);
    let text =
        TextQuery::parse(&request.q).map_err(|error| Failure::bad_request(error.to_string()))?;
    let query = SearchQuery {
        text,
        limit,
        session: request.session,
        runtime: request.runtime,
        agent: request.agent,
        project: request.project,
        kind: request.kind,
        is_error: request.is_error,
        after_ts: request.after_ts,
        before_ts: request.before_ts,
    };
    query.validate().map_err(|error| match error {
        SearchError::InvalidQuery(message) => Failure::bad_request(message),
        SearchError::Backend(_) => unreachable!("validation cannot produce a backend error"),
    })?;
    let backend = state.search.clone();
    let output = tokio::task::spawn_blocking(move || backend.search(query))
        .await
        .map_err(|error| Failure::internal(format!("search task failed: {error}")))?
        .map_err(|error| match error {
            SearchError::InvalidQuery(message) => {
                Failure::bad_request(format!("search failed: {message}"))
            }
            SearchError::Backend(message) => Failure::internal(format!("search failed: {message}")),
        })?;
    Ok(Json(SearchResponse {
        state: output.state,
        hits: output.hits,
    }))
}

#[derive(serde::Deserialize)]
struct HeatmapQuery {
    since_ms: Option<i64>,
    offset_minutes: Option<i32>,
}

async fn heatmap(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<HeatmapQuery>,
) -> Result<Json<HeatmapResponse>, Failure> {
    let response = with_read(state.db.clone(), move |conn| {
        let days = queries::heatmap_days_in_timezone(
            conn,
            &id,
            query.since_ms.unwrap_or(0),
            query.offset_minutes.unwrap_or(0),
        )
        .map_err(store_err)?;
        Ok(HeatmapResponse { days })
    })
    .await?;
    Ok(Json(response))
}

/// The heatmap figure's decomposition: per-model days plus an hour-of-day
/// profile, both over the same `since_ms` window in the same offset — so
/// every number here sums back to a `/heatmap` figure exactly.
async fn usage(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<HeatmapQuery>,
) -> Result<Json<UsageResponse>, Failure> {
    let response = with_read(state.db.clone(), move |conn| {
        let since_ms = query.since_ms.unwrap_or(0);
        let offset_minutes = query.offset_minutes.unwrap_or(0);
        let models = queries::model_days_in_timezone(conn, &id, since_ms, offset_minutes)
            .map_err(store_err)?;
        let hours = queries::hour_profile_in_timezone(conn, &id, since_ms, offset_minutes)
            .map_err(store_err)?;
        Ok(UsageResponse { models, hours })
    })
    .await?;
    Ok(Json(response))
}

async fn contribution(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<ContributionResponse>, Failure> {
    let response = with_read(state.db.clone(), move |conn| {
        let rows = queries::contribution(conn, &id).map_err(store_err)?;
        Ok(ContributionResponse { rows })
    })
    .await?;
    Ok(Json(response))
}
