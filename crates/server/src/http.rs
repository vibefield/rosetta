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
    ApiError, ContributionResponse, ErrorCode, HeatmapResponse, PROTOCOL_VERSION, ProjectsResponse,
    RuntimesResponse, SearchRequest, SearchResponse, SessionResponse, SessionsResponse,
    StatusResponse, TimelineCursor, TimelinePage,
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
}

impl AppState {
    /// Build the shipped synchronous-FTS5 service state.
    pub fn new(hub: Arc<Hub>, db: PathBuf) -> Self {
        let search = Arc::new(Fts5Search::new(db.clone()));
        Self {
            hub,
            db: Arc::new(db),
            search,
        }
    }

    /// Inject a backend while retaining the same canonical projection. Used
    /// by sidecars and by readiness/coherence contract tests.
    pub fn with_search(hub: Arc<Hub>, db: PathBuf, search: Arc<dyn SearchBackend>) -> Self {
        Self {
            hub,
            db: Arc::new(db),
            search,
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/runtimes", get(runtimes))
        .route("/api/projects", get(projects))
        .route("/api/projects/{id}/heatmap", get(heatmap))
        .route("/api/projects/{id}/contribution", get(contribution))
        .route("/api/sessions", get(sessions))
        .route("/api/sessions/{id}", get(session))
        .route("/api/sessions/{id}/timeline", get(timeline))
        .route("/api/search", get(search))
        .route("/ws/meta", get(crate::ws::meta))
        .route("/ws/sessions/{id}", get(crate::ws::session))
        .with_state(state)
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

async fn status(State(state): State<AppState>) -> Result<Json<StatusResponse>, Failure> {
    let search_state = state.search.state();
    let response = with_read(state.db.clone(), |conn| {
        let schema_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| Failure::internal(format!("{error}")))?;
        let canonical_ready = schema_version == schema::SCHEMA_VERSION;
        Ok(StatusResponse {
            protocol_version: PROTOCOL_VERSION,
            schema_version,
            canonical_ready,
            // Synchronous FTS5 commits with canonical rows; a foreign schema
            // means neither surface is serveable.
            search: if canonical_ready {
                search_state
            } else {
                SearchState::failed()
            },
        })
    })
    .await?;
    Ok(Json(response))
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
