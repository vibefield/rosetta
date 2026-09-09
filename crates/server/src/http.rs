//! The REST surface: axum over `store::queries` for canonical row reads, the
//! search backend boundary for ranked retrieval, and the hub's resident
//! mirror for list/browse (which therefore never touch the DB).
//! Every SQLite read opens its own connection inside `spawn_blocking`. A
//! fresh daemon attaches to the named canonical RAM projection; after its
//! atomic publication the same router opens the durable file read-only.
//!
//! Pagination follows the exact-watermark rule: the timeline cursor binds
//! `queries::session_watermark` at first page and answers HTTP 410
//! (`cursor_expired`) on any mismatch — never a silent rebase.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use rusqlite::{Connection, OpenFlags};
use serde_json::json;

use rosetta_protocol::{
    ApiError, BoostRequest, BoostResponse, CollaborationTimeline, CommandResponse,
    ContributionResponse, DiscoveriesResponse, EnableRequest, EnableResponse, ErrorCode,
    HeatmapResponse, PROTOCOL_VERSION, ProjectsResponse, RuntimesResponse, SearchRequest,
    SearchResponse, SessionResponse, SessionsResponse, StatusResponse, TimelineCursor,
    TimelineEntryResponse, TimelinePage, UsageResponse,
};
use rosetta_search::{
    Fts5SidecarSearch, SearchBackend, SearchError, SearchQuery, SearchState, TextQuery,
};
use rosetta_store::{StoreError, schema};
use rosetta_store::{assets, queries, timeline_meta};

use crate::hub::Hub;

const DEFAULT_LIST_LIMIT: i64 = 100;
const DEFAULT_SEARCH_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 1_000;
/// Reserved transport id for the client-only bucket of sessions whose source
/// format exposes no workspace. It is a query scope, never a canonical row.
const UNASSIGNED_PROJECT_ID: &str = "__rosetta_unassigned__";

#[derive(Clone)]
pub struct AppState {
    pub hub: Arc<Hub>,
    /// Stable projection location for content-addressed assets and the
    /// detached search sidecar.
    pub db: Arc<PathBuf>,
    /// Canonical read target. A fresh daemon initially points this at its
    /// named in-memory cold projection, then atomically switches to `db`
    /// after the validated durable copy is installed.
    pub projection: Arc<ProjectionReader>,
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
    /// Lossless command bodies stay out of the projection and are recovered
    /// through this synchronous engine boundary only on explicit disclosure.
    pub raw_commands: Option<Arc<dyn RawCommandReader>>,
}

pub trait RawCommandReader: Send + Sync {
    fn read_command(&self, source: &queries::CommandSource) -> Result<Option<String>, String>;
}

impl AppState {
    /// Build the shipped detached-FTS5 service state. The persistent daemon
    /// also starts its worker; a static reader honestly reports building when
    /// no complete sidecar generation exists.
    pub fn new(hub: Arc<Hub>, db: PathBuf) -> Self {
        let search = Arc::new(Fts5SidecarSearch::new(db.clone()));
        Self {
            hub,
            projection: Arc::new(ProjectionReader::file(db.clone())),
            db: Arc::new(db),
            search,
            health: None,
            consent: None,
            boost: None,
            raw_commands: None,
        }
    }

    /// Inject a backend while retaining the same canonical projection. Used
    /// by sidecars and by readiness/coherence contract tests.
    pub fn with_search(hub: Arc<Hub>, db: PathBuf, search: Arc<dyn SearchBackend>) -> Self {
        Self {
            hub,
            projection: Arc::new(ProjectionReader::file(db.clone())),
            db: Arc::new(db),
            search,
            health: None,
            consent: None,
            boost: None,
            raw_commands: None,
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

    /// Attach the daemon's append-only-ledger reader. Static projection
    /// servers omit it and answer honestly that source expansion is absent.
    pub fn with_raw_commands(mut self, reader: Arc<dyn RawCommandReader>) -> Self {
        self.raw_commands = Some(reader);
        self
    }

    /// Override the canonical read target without changing where durable
    /// assets and detached search live. Used only by fresh cold serve.
    pub fn with_projection(mut self, projection: Arc<ProjectionReader>) -> Self {
        self.projection = projection;
        self
    }
}

#[derive(Clone)]
enum ProjectionTarget {
    File(Arc<PathBuf>),
    SharedMemory(Arc<SharedMemoryTarget>),
}

/// Keeps a named in-memory SQLite database alive for every reader that
/// captured the old generation while publication switches future reads to
/// disk. The keeper is never queried; its lifetime closes the handoff race.
struct SharedMemoryTarget {
    uri: Arc<str>,
    _keeper: Mutex<Connection>,
}

/// Atomic canonical read router. It changes location, never schema: both
/// targets expose the same canonical tables and queries.
pub struct ProjectionReader {
    target: RwLock<ProjectionTarget>,
}

impl ProjectionReader {
    pub fn file(path: PathBuf) -> Self {
        Self {
            target: RwLock::new(ProjectionTarget::File(Arc::new(path))),
        }
    }

    /// Attach to a named SQLite memory URI already owned by the cold writer.
    /// A keeper connection makes the target safe to clone across an atomic
    /// switch even if the writer is dropped immediately afterwards.
    pub fn shared_memory(uri: &str) -> Result<Self, String> {
        let keeper = open_shared_memory(uri)
            .map_err(|error| format!("attaching canonical RAM reader: {error}"))?;
        Ok(Self {
            target: RwLock::new(ProjectionTarget::SharedMemory(Arc::new(
                SharedMemoryTarget {
                    uri: Arc::from(uri),
                    _keeper: Mutex::new(keeper),
                },
            ))),
        })
    }

    /// Future requests open the installed durable projection. Requests that
    /// already captured the RAM target retain its keeper and finish against
    /// the old, identical generation.
    pub fn publish_file(&self, path: PathBuf) {
        *self.target.write().expect("projection reader poisoned") =
            ProjectionTarget::File(Arc::new(path));
    }

    pub(crate) fn is_published(&self) -> bool {
        matches!(
            &*self.target.read().expect("projection reader poisoned"),
            ProjectionTarget::File(_)
        )
    }

    fn open(&self) -> Result<Connection, Failure> {
        let target = self
            .target
            .read()
            .expect("projection reader poisoned")
            .clone();
        match target {
            ProjectionTarget::File(path) => open_read(&path),
            ProjectionTarget::SharedMemory(memory) => open_shared_memory(&memory.uri)
                .map_err(|error| Failure::internal(format!("opening canonical RAM view: {error}"))),
        }
    }
}

fn open_shared_memory(uri: &str) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(uri, flags)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
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
        .route(
            "/api/sessions/{id}/collaboration",
            get(collaboration_timeline),
        )
        .route("/api/sessions/{id}/timeline", get(timeline))
        .route(
            "/api/sessions/{id}/timeline/{timeline_id}",
            get(timeline_entry),
        )
        .route(
            "/api/sessions/{id}/timeline/{timeline_id}/command",
            get(command),
        )
        .route(
            "/api/sessions/{id}/timeline/{timeline_id}/work",
            get(report_work),
        )
        .route(
            "/api/sessions/{id}/timeline/{timeline_id}/read/{target_index}",
            get(read_file_target)
                .route_layer(axum::middleware::from_fn(crate::ws::require_local_origin)),
        )
        .route(
            "/api/sessions/{id}/plans/revisions/{revision}",
            get(read_plan_revision),
        )
        .route(
            "/api/sessions/{id}/timeline/{timeline_id}/plan",
            get(read_timeline_plan),
        )
        .route("/api/assets/{name}", get(asset))
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
        .route(
            "/ws/collaboration/{id}",
            get(crate::ws::collaboration)
                .route_layer(axum::middleware::from_fn(crate::ws::require_local_origin)),
        )
        .layer(axum::middleware::from_fn(
            |request: axum::extract::Request, next: axum::middleware::Next| async move {
                let mut response = next.run(request).await;
                response.headers_mut().insert(
                    "x-rosetta-protocol-version",
                    HeaderValue::from_str(&PROTOCOL_VERSION.to_string())
                        .expect("numeric protocol version"),
                );
                response
            },
        ))
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
        .expose_headers([axum::http::HeaderName::from_static(
            "x-rosetta-protocol-version",
        )])
}

/// The origin policy, shared with the WebSocket upgrade guard: any port on a
/// loopback host, plus the Tauri and Electron shells. A `null` origin (sandboxed
/// iframes, some file:// contexts) is deliberately NOT allowed — any site
/// can manufacture one.
pub(crate) fn origin_allowed(origin: &axum::http::HeaderValue) -> bool {
    let Ok(text) = origin.to_str() else {
        return false;
    };
    if text == "tauri://localhost" || text == "rosetta://app" {
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

    fn not_ready(message: impl Into<String>) -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            ApiError {
                code: ErrorCode::NotReady,
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
pub(crate) async fn with_read<T, F>(
    projection: Arc<ProjectionReader>,
    read: F,
) -> Result<T, Failure>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T, Failure> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let conn = projection.open()?;
        // Watermarks and their rows must come from the same SQLite snapshot.
        conn.execute_batch("BEGIN DEFERRED")
            .map_err(|error| store_err(error.into()))?;
        read(&conn)
    })
    .await
    .map_err(|error| Failure::internal(format!("read task failed: {error}")))?
}

fn store_err(error: StoreError) -> Failure {
    Failure::internal(format!("{error}"))
}

/// Global aggregates are deliberately outside the progressive RAM surface.
/// A project/session/timeline read is bounded and useful while cold ingest
/// advances; a heatmap or contribution fold scans cross-session derived data
/// and can starve the sole shared-cache writer. Canonical publication is the
/// exact seam where these reads become both stable and safe.
fn require_canonical_aggregates(projection: &ProjectionReader) -> Result<(), Failure> {
    if projection.is_published() {
        Ok(())
    } else {
        Err(Failure::not_ready(
            "global aggregates are available after canonical publication",
        ))
    }
}

async fn asset(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
) -> Result<Response, Failure> {
    let media_type =
        asset_media_type(&name).ok_or_else(|| Failure::not_found("no such projection asset"))?;
    let db = state.db.clone();
    let expected_digest = name[..64].to_owned();
    let path_name = name.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        let path = assets::asset_root(&db).join(path_name);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Failure::not_found("no such projection asset")
            } else {
                Failure::internal(format!("stating projection asset: {error}"))
            }
        })?;
        if !metadata.file_type().is_file() {
            return Err(Failure::not_found("no such projection asset"));
        }
        let bytes = std::fs::read(path)
            .map_err(|error| Failure::internal(format!("reading projection asset: {error}")))?;
        if blake3::hash(&bytes).to_hex().as_str() != expected_digest {
            return Err(Failure::internal(
                "projection asset failed its content hash",
            ));
        }
        Ok(bytes)
    })
    .await
    .map_err(|error| Failure::internal(format!("asset read task failed: {error}")))??;

    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

/// The file viewer's largest admitted payload. The UI performs its more
/// specific text/image/binary limit after this read; this ceiling keeps a
/// direct request from turning one historical path into an unbounded read.
const FILE_VIEW_MAX_BYTES: u64 = 32 * 1024 * 1024;

fn plan_response(document: timeline_meta::PlanDocument) -> Response {
    let content_type = if document.name.ends_with(".json") {
        "application/json; charset=utf-8"
    } else {
        "text/markdown; charset=utf-8"
    };
    let mut response = Response::new(Body::from(document.content));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn read_plan_revision(
    State(state): State<AppState>,
    UrlPath((session_id, revision)): UrlPath<(String, i64)>,
) -> Result<Response, Failure> {
    let document = with_read(state.projection.clone(), move |conn| {
        timeline_meta::revision_plan_document(conn, &session_id, revision)
            .map_err(store_err)?
            .ok_or_else(|| Failure::not_found("that plan revision is not available"))
    })
    .await?;
    Ok(plan_response(document))
}

async fn read_timeline_plan(
    State(state): State<AppState>,
    UrlPath((session_id, timeline_id)): UrlPath<(String, String)>,
) -> Result<Response, Failure> {
    let document = with_read(state.projection.clone(), move |conn| {
        timeline_meta::timeline_plan_document(conn, &session_id, &timeline_id)
            .map_err(store_err)?
            .ok_or_else(|| Failure::not_found("that recorded plan is not available"))
    })
    .await?;
    Ok(plan_response(document))
}

async fn read_file_target(
    State(state): State<AppState>,
    UrlPath((session_id, timeline_id, target_index)): UrlPath<(String, String, usize)>,
) -> Result<Response, Failure> {
    // The path is looked up from the canonical row. There is intentionally no
    // path query parameter: the browser can ask for target 0, but it cannot
    // substitute a different file for the one the transcript recorded.
    let target = with_read(state.projection.clone(), move |conn| {
        queries::file_read_target(conn, &session_id, &timeline_id, target_index)
            .map_err(store_err)?
            .ok_or_else(|| Failure::not_found("no file at that recorded target"))
    })
    .await?;

    let path = {
        let recorded = PathBuf::from(&target.path);
        if recorded.is_absolute() {
            recorded
        } else {
            let cwd = target
                .cwd
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or_else(|| {
                    Failure::not_found("that relative read has no recorded working directory")
                })?;
            cwd.join(recorded)
        }
    };

    let bytes = tokio::task::spawn_blocking(move || {
        use std::io::Read;

        let file = std::fs::File::open(&path)
            .map_err(|_| Failure::not_found("the recorded file is no longer available"))?;
        let metadata = file
            .metadata()
            .map_err(|_| Failure::not_found("the recorded file is no longer available"))?;
        if !metadata.is_file() {
            return Err(Failure::not_found("the recorded target is not a file"));
        }
        if metadata.len() > FILE_VIEW_MAX_BYTES {
            return Err(Failure::bad_request(
                "the recorded file is too large to open here",
            ));
        }

        // Re-check the cap while reading: a file can grow after metadata was
        // sampled, and `std::fs::read` would discover that only after it had
        // allocated the new size.
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(FILE_VIEW_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| Failure::internal(format!("reading recorded file: {error}")))?;
        if bytes.len() as u64 > FILE_VIEW_MAX_BYTES {
            return Err(Failure::bad_request(
                "the recorded file is too large to open here",
            ));
        }
        Ok(bytes)
    })
    .await
    .map_err(|error| Failure::internal(format!("recorded file read task failed: {error}")))??;

    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

fn asset_media_type(name: &str) -> Option<&'static str> {
    let (digest, extension) = name.split_once('.')?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }
    match extension {
        "png" => Some("image/png"),
        "jpg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "pdf" => Some("application/pdf"),
        "json" => Some("application/vnd.rosetta.canonical-document+json"),
        "txt" => Some("text/plain; charset=utf-8"),
        _ => None,
    }
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
    if !state.projection.is_published() {
        return Json(StatusResponse {
            protocol_version: PROTOCOL_VERSION,
            schema_version: schema::SCHEMA_VERSION,
            canonical_ready: false,
            search: SearchState::building(),
            daemon,
        });
    }
    let projection = with_read(state.projection.clone(), |conn| {
        let schema_version = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| Failure::internal(format!("{error}")))?;
        let canonical_ready = if schema_version == schema::SCHEMA_VERSION {
            queries::projection_state(conn)
                .map_err(store_err)?
                .canonical_ready
        } else {
            false
        };
        Ok((schema_version, canonical_ready))
    })
    .await;
    let response = match projection {
        Ok((schema_version, canonical_ready)) => StatusResponse {
            protocol_version: PROTOCOL_VERSION,
            schema_version,
            canonical_ready: canonical_ready && state.projection.is_published(),
            search: if schema_version == schema::SCHEMA_VERSION {
                search_state
            } else {
                SearchState::failed()
            },
            daemon,
        },
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
    /// Comma-separated provider-native session ids. A consumer that knows a
    /// running agent only by the id its provider minted resolves the canonical
    /// row here without listing history (protocol 18).
    native_session_id: Option<String>,
}

/// Comma-separated selector values, trimmed, deduplicated, with blank entries
/// refused rather than silently matching nothing.
fn selector_list(raw: &str, name: &str) -> Result<Vec<String>, Failure> {
    let mut values: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let value = part.trim();
        if value.is_empty() {
            return Err(Failure::bad_request(format!(
                "{name} must be a comma-separated list of non-blank values"
            )));
        }
        if !values.iter().any(|known| known == value) {
            values.push(value.to_owned());
        }
    }
    Ok(values)
}

async fn sessions(
    State(state): State<AppState>,
    Query(query): Query<SessionListQuery>,
) -> Result<Json<SessionsResponse>, Failure> {
    let limit = clamp_limit(query.limit, DEFAULT_LIST_LIMIT) as usize;
    let mut sessions = state.hub.sessions();
    if let Some(raw) = query.native_session_id.as_deref() {
        // A native-id lookup answers children as well: the caller resolves the
        // navigable root through `root_session_id`, and a provider may have
        // minted the id the caller holds for a delegated child.
        let natives = selector_list(raw, "native_session_id")?;
        sessions.retain(|session| natives.contains(&session.native_session_id));
    } else {
        // Child sessions are transcript structure, not browseable top-level
        // history. Parent absence is the authoritative navigation fact; some
        // native formats reveal an immediate parent before the denormalized root
        // can be transitively refined. Children remain addressable by id.
        sessions.retain(|session| session.parent_session_id.is_none());
    }
    if let Some(project) = &query.project {
        sessions.retain(|session| session.project_id.as_deref() == Some(project));
    }
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    sessions.truncate(limit);
    Ok(Json(SessionsResponse { sessions }))
}

/// `sessions=` names roots a reader can navigate to, while search documents
/// carry only their own session id. Each named session is therefore widened
/// to every session that rolls up to it before the backend sees the scope. An
/// id the mirror does not know is kept as given: it may be a session the hub
/// has not absorbed yet, and an honest empty answer beats a refusal.
fn expand_session_scope(state: &AppState, raw: Option<&str>) -> Result<Vec<String>, Failure> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let requested = selector_list(raw, "sessions")?;
    if requested.len() > rosetta_search::MAX_SEARCH_SESSIONS {
        return Err(Failure::bad_request(format!(
            "sessions exceeds {} ids",
            rosetta_search::MAX_SEARCH_SESSIONS
        )));
    }
    let mut scope = requested.clone();
    for session in state.hub.sessions() {
        let Some(parent) = session.parent_session_id.as_deref() else {
            continue;
        };
        let rolls_up = requested
            .iter()
            .any(|id| *id == session.root_session_id || id == parent);
        if rolls_up && !scope.contains(&session.id) {
            scope.push(session.id);
        }
    }
    Ok(scope)
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
    let hide_pending = !state.projection.is_published();
    let page = with_read(state.projection.clone(), move |conn| {
        read_timeline_page(conn, &id, limit, query.cursor.as_deref(), hide_pending)
    })
    .await?;
    Ok(Json(page))
}

pub(crate) fn read_timeline_page(
    conn: &Connection,
    id: &str,
    limit: i64,
    cursor: Option<&str>,
    hide_pending: bool,
) -> Result<TimelinePage, Failure> {
    let Some(session) = queries::session(conn, id).map_err(store_err)? else {
        return Err(Failure::not_found(format!("no session {id}")));
    };
    let watermark = queries::session_watermark(conn, id).map_err(store_err)?;
    // A fresh runtime's source batches and RAM reconciliation use
    // separate physical commits. Metadata remains immediately public,
    // but its payload is admitted only when the final readiness commit
    // changes Pending to Streaming/Indexed.
    if hide_pending && session.stream_state == rosetta_core::rows::StreamState::Pending {
        return Ok(TimelinePage {
            entries: Vec::new(),
            cursor: None,
            watermark,
        });
    }
    let before = match cursor {
        None => None,
        Some(text) => {
            let cursor = TimelineCursor::decode(text).ok_or_else(Failure::cursor_expired)?;
            if cursor.session_id != id || cursor.watermark != watermark {
                return Err(Failure::cursor_expired());
            }
            Some(cursor.before)
        }
    };
    let entries = queries::timeline_page(conn, id, before, limit).map_err(store_err)?;
    let cursor = if entries.len() as i64 == limit {
        entries.first().map(|oldest| {
            TimelineCursor {
                session_id: id.to_owned(),
                before: oldest.ordinal,
                watermark: watermark.clone(),
            }
            .encode()
        })
    } else {
        None
    };
    Ok(TimelinePage {
        entries,
        cursor,
        watermark,
    })
}

async fn collaboration_timeline(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<CollaborationTimeline>, Failure> {
    let hide_pending = !state.projection.is_published();
    with_read(state.projection.clone(), move |conn| {
        let Some(session) = queries::session(conn, &id).map_err(store_err)? else {
            return Err(Failure::not_found(format!("no session {id}")));
        };
        if hide_pending && session.stream_state == rosetta_core::rows::StreamState::Pending {
            return Ok(Json(CollaborationTimeline {
                entries: Vec::new(),
                response_runtimes: Vec::new(),
            }));
        }
        let (entries, response_runtimes) =
            queries::collaboration_timeline(conn, &id).map_err(store_err)?;
        Ok(Json(CollaborationTimeline {
            entries,
            response_runtimes,
        }))
    })
    .await
}

async fn timeline_entry(
    State(state): State<AppState>,
    UrlPath((session_id, timeline_id)): UrlPath<(String, String)>,
) -> Result<Json<TimelineEntryResponse>, Failure> {
    let hide_pending = !state.projection.is_published();
    with_read(state.projection.clone(), move |conn| {
        let Some(session) = queries::session(conn, &session_id).map_err(store_err)? else {
            return Err(Failure::not_found(format!("no session {session_id}")));
        };
        if hide_pending && session.stream_state == rosetta_core::rows::StreamState::Pending {
            return Err(Failure::not_found("that timeline row is not available"));
        }
        let watermark = queries::session_watermark(conn, &session_id).map_err(store_err)?;
        queries::timeline_entry(conn, &session_id, &timeline_id)
            .map_err(store_err)?
            .map(|entry| Json(TimelineEntryResponse { entry, watermark }))
            .ok_or_else(|| Failure::not_found("that timeline row is not available"))
    })
    .await
}

/// The report itself authorizes the exact source session and inclusive bounds.
/// Private worker content is loaded only when the work disclosure is opened.
async fn report_work(
    State(state): State<AppState>,
    UrlPath((session_id, timeline_id)): UrlPath<(String, String)>,
) -> Result<Json<Vec<rosetta_core::TimelineEntry>>, Failure> {
    with_read(state.projection.clone(), move |conn| {
        queries::report_work(conn, &session_id, &timeline_id)
            .map_err(store_err)?
            .map(Json)
            .ok_or_else(|| Failure::not_found("that report has no available recorded work"))
    })
    .await
}

async fn command(
    State(state): State<AppState>,
    UrlPath((session_id, timeline_id)): UrlPath<(String, String)>,
) -> Result<Json<CommandResponse>, Failure> {
    if !state.projection.is_published() {
        return Err(Failure::not_ready(
            "full commands are available after raw-ledger publication",
        ));
    }
    let source = with_read(state.projection.clone(), {
        let session_id = session_id.clone();
        let timeline_id = timeline_id.clone();
        move |conn| {
            queries::command_source(conn, &session_id, &timeline_id)
                .map_err(store_err)?
                .ok_or_else(|| Failure::not_found("no raw command source for this row"))
        }
    })
    .await?;
    let reader = state
        .raw_commands
        .clone()
        .ok_or_else(|| Failure::not_ready("this server has no raw-ledger command reader"))?;
    let command = tokio::task::spawn_blocking(move || reader.read_command(&source))
        .await
        .map_err(|error| Failure::internal(format!("raw command task failed: {error}")))?
        .map_err(Failure::internal)?
        .ok_or_else(|| Failure::not_found("the raw command record is unavailable"))?;
    Ok(Json(CommandResponse { command }))
}

async fn search(
    State(state): State<AppState>,
    Query(request): Query<SearchRequest>,
) -> Result<Json<SearchResponse>, Failure> {
    let limit = clamp_limit(request.limit, DEFAULT_SEARCH_LIMIT);
    let text =
        TextQuery::parse(&request.q).map_err(|error| Failure::bad_request(error.to_string()))?;
    let sessions = expand_session_scope(&state, request.sessions.as_deref())?;
    let query = SearchQuery {
        text,
        prefix: request.prefix.unwrap_or(false),
        limit,
        session: request.session,
        sessions,
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
    if !state.projection.is_published() {
        return Ok(Json(SearchResponse {
            state: SearchState::building(),
            hits: Vec::new(),
        }));
    }
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
    require_canonical_aggregates(&state.projection)?;
    let response = with_read(state.projection.clone(), move |conn| {
        let project_id = (id != UNASSIGNED_PROJECT_ID).then_some(id.as_str());
        let days = queries::heatmap_days_in_timezone_for_scope(
            conn,
            project_id,
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
    require_canonical_aggregates(&state.projection)?;
    let response = with_read(state.projection.clone(), move |conn| {
        let since_ms = query.since_ms.unwrap_or(0);
        let offset_minutes = query.offset_minutes.unwrap_or(0);
        let project_id = (id != UNASSIGNED_PROJECT_ID).then_some(id.as_str());
        let models =
            queries::model_days_in_timezone_for_scope(conn, project_id, since_ms, offset_minutes)
                .map_err(store_err)?;
        let hours =
            queries::hour_profile_in_timezone_for_scope(conn, project_id, since_ms, offset_minutes)
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
    require_canonical_aggregates(&state.projection)?;
    let response = with_read(state.projection.clone(), move |conn| {
        let project_id = (id != UNASSIGNED_PROJECT_ID).then_some(id.as_str());
        let rows = queries::contribution_for_scope(conn, project_id).map_err(store_err)?;
        Ok(ContributionResponse { rows })
    })
    .await?;
    Ok(Json(response))
}
