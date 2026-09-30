//! Access control against a real bound server (docs/AUTH.md): the bearer on
//! every REST route and WebSocket upgrade, per-session grants in place of it,
//! the Origin rule as a browser guard that never stands in for a credential,
//! and the token-less daemon exactly as it was.

use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use rosetta_core::IngestOp;
use rosetta_core::rows::{
    MessageKind, MessageRow, ProjectRow, RuntimeRow, SessionRole, SessionRow, StreamState,
};
use rosetta_search::{Fts5SidecarSearch, synchronize_fts5_once};
use rosetta_server::auth::{GRANT_ASSET_ROUTE, GRANT_SESSION_ROUTES};
use rosetta_server::grant::{self, GrantClaims};
use rosetta_server::{AppState, AuthPolicy, GrantEvent, Hub, HubConfig, Refusal, router};
use rosetta_store::{Batch, CheckpointUpdate, SourceMode, SourceWrite, Store, queries};
use tempfile::TempDir;

const TOKEN: &str = "rosetta-test-bearer-0123456789abcdef";
const KEY: [u8; 32] = *b"grant key: thirty-two bytes long";
const SHELL: &str = "vibefield-app://shell";
const FOREIGN: &str = "https://evil.example";

fn session(id: &str) -> SessionRow {
    SessionRow {
        id: id.into(),
        native_session_id: format!("native-{id}"),
        runtime_id: "rt1".into(),
        project_id: Some("p1".into()),
        cwd: Some("/w/rosetta".into()),
        parent_session_id: None,
        delegation: None,
        root_session_id: id.into(),
        role: SessionRole::Main,
        title: None,
        summary: None,
        started_at: 10_000,
        updated_at: 10_000,
        activity: None,
        stream_state: StreamState::Streaming,
    }
}

fn message(content: &str) -> MessageRow {
    MessageRow {
        timeline_id: "t1".into(),
        session_id: "s1".into(),
        ordinal: 0,
        kind: MessageKind::User,
        subtype: None,
        uuid: None,
        parent_uuid: None,
        ts: Some(3_600_000),
        model: None,
        effort: None,
        content: Some(content.to_string()),
        tool: None,
        attachments: Vec::new(),
        citations: Vec::new(),
        group: None,
        response: None,
        block_index: None,
        branch_tool_id: None,
        usage: None,
        agent_version: None,
        src_cursor: None,
        src_offset: None,
    }
}

struct Server {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
    _store: Store,
    _dir: TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Two sessions (s1 with one message, s2 empty), served under `policy` on a
/// real loopback listener.
async fn serve(policy: AuthPolicy) -> Server {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("projection.sqlite");
    let mut store = Store::open(&db).unwrap();
    store.observe_effects();
    store
        .apply(&Batch {
            ops: vec![
                IngestOp::Runtime(RuntimeRow {
                    id: "rt1".into(),
                    kind: "claude-code".into(),
                    data_root: "~/.claude".into(),
                    enabled: true,
                    account: None,
                    config: None,
                    skills: None,
                    versions_seen: None,
                    discovered_at: 1_000,
                    label: None,
                    provenance: Default::default(),
                }),
                IngestOp::Project(ProjectRow {
                    id: "p1".into(),
                    name: "rosetta".into(),
                    root_paths: vec!["/w/rosetta".into()],
                    git_repo: None,
                }),
                IngestOp::SessionMeta(session("s1")),
                IngestOp::SessionMeta(session("s2")),
            ],
            checkpoints: vec![CheckpointUpdate {
                runtime_id: "rt1".into(),
                stream: "transcript".into(),
                path: "p/x.jsonl".into(),
                session_id: Some("s1".into()),
                expected: None,
                next: "blob-v0".into(),
                updated_at: Some(1),
            }],
            source: None,
        })
        .unwrap();
    let cursor = queries::cursor_state(store.conn(), "rt1", "p/x.jsonl")
        .unwrap()
        .unwrap()
        .id;
    store
        .apply(&Batch {
            ops: IngestOp::project_message(message("hello from s1")),
            checkpoints: vec![],
            source: Some(SourceWrite {
                cursor_id: cursor,
                session_id: "s1".into(),
                generation: 0,
                first_sequence: 0,
                mode: SourceMode::Append,
            }),
        })
        .unwrap();
    let hub = Arc::new(Hub::new(HubConfig::default(), 1));
    hub.warm(store.conn()).unwrap();
    store.drain_effects();
    let search = Fts5SidecarSearch::new(&db);
    let ack = synchronize_fts5_once(&search, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    store
        .acknowledge_search(&ack.projection_epoch, ack.revision)
        .unwrap();
    let state = AppState::with_search(hub, db, Arc::new(search)).with_auth(Arc::new(policy));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    Server {
        addr,
        task,
        _store: store,
        _dir: dir,
    }
}

fn policy() -> AuthPolicy {
    AuthPolicy::new(Some(TOKEN), Some(KEY.to_vec()), &[SHELL.to_string()]).unwrap()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

fn grant_for(sid: &str) -> String {
    grant::mint(
        &KEY,
        &GrantClaims::read(sid, "fieldd/w1", now_ms() + 60_000),
    )
}

fn authorization(grant: &str) -> String {
    format!("Rosetta-Grant {grant}")
}

/// What a browser offers: the marker, then its credential entry.
fn ws_grant(grant: &str) -> String {
    format!("rosetta, rosetta.grant.{grant}")
}

fn ws_bearer() -> String {
    format!("rosetta, rosetta.bearer.{}", URL_SAFE_NO_PAD.encode(TOKEN))
}

#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    fn refusal(&self) -> (u16, Option<&str>) {
        (self.status, self.header("x-rosetta-auth"))
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl Server {
    /// One raw HTTP/1.1 exchange, so nothing between the test and the
    /// daemon adds or drops a header.
    async fn request(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> Reply {
        let mut stream = tokio::net::TcpStream::connect(self.addr).await.unwrap();
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: 0\r\n",
            self.addr
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
            .await
            .expect("the daemon answered")
            .unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status line in {text:?}"));
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        Reply {
            status,
            headers,
            body: body.to_string(),
        }
    }

    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> Reply {
        self.request("GET", path, headers).await
    }

    /// Open a WebSocket, or return the refused handshake's status and reason.
    async fn ws(
        &self,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Result<(Socket, Option<String>), (u16, Option<String>)> {
        let mut request = format!("ws://{}{path}", self.addr)
            .into_client_request()
            .unwrap();
        for (name, value) in headers {
            request.headers_mut().append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        match tokio_tungstenite::connect_async(request).await {
            Ok((socket, response)) => Ok((
                socket,
                response
                    .headers()
                    .get("sec-websocket-protocol")
                    .map(|value| value.to_str().unwrap().to_string()),
            )),
            Err(WsError::Http(response)) => Err((
                response.status().as_u16(),
                response
                    .headers()
                    .get("x-rosetta-auth")
                    .map(|value| value.to_str().unwrap().to_string()),
            )),
            Err(other) => panic!("{path}: {other}"),
        }
    }
}

async fn next_frame(socket: &mut Socket) -> serde_json::Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a frame arrived")
            .expect("the stream is open")
            .expect("the frame is readable");
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

/// The stream greets, then (for a session) snapshots its timeline.
async fn assert_greets(socket: &mut Socket) {
    assert_eq!(next_frame(socket).await["frame"], "hello");
}

fn refused(reason: Refusal) -> (u16, Option<String>) {
    (401, Some(reason.as_str().to_string()))
}

#[tokio::test]
async fn the_bearer_guards_every_rest_route_and_websocket() {
    let server = serve(policy()).await;
    for path in [
        "/api/status",
        "/api/runtimes",
        "/api/projects",
        "/api/sessions",
        "/api/sessions/s1",
        "/api/sessions/s1/timeline",
        "/api/search?q=hello",
        "/api/nowhere",
    ] {
        let missing = server.get(path, &[]).await;
        assert_eq!(missing.refusal(), (401, Some("missing")), "{path}");
        assert_eq!(
            missing.all("www-authenticate"),
            [
                r#"Bearer realm="rosetta""#,
                r#"Rosetta-Grant realm="rosetta""#
            ],
            "{path}"
        );
        // The version stays readable, so a client can tell "wrong daemon"
        // from "wrong credential".
        assert_eq!(
            missing.header("x-rosetta-protocol-version"),
            Some(rosetta_server::PROTOCOL_VERSION.to_string().as_str())
        );
        assert!(missing.body.contains("unauthorized (missing)"), "{path}");
        let wrong = server
            .get(
                path,
                &[(
                    "authorization",
                    "Bearer rosetta-test-bearer-0123456789abcdeX",
                )],
            )
            .await;
        assert_eq!(wrong.refusal(), (401, Some("bad-token")), "{path}");
        let right = server.get(path, &[("authorization", &bearer())]).await;
        let expected = if path == "/api/nowhere" { 404 } else { 200 };
        assert_eq!(right.status, expected, "{path}: {right:?}");
    }
    let search = server
        .get("/api/search?q=hello", &[("authorization", &bearer())])
        .await;
    assert!(
        search
            .body
            .contains(r#""timeline_id":"t1","session_id":"s1""#),
        "{search:?}"
    );

    for path in ["/ws/meta", "/ws/sessions/s1", "/ws/collaboration/s1"] {
        assert_eq!(
            server.ws(path, &[]).await.err(),
            Some(refused(Refusal::Missing)),
            "{path}"
        );
        assert_eq!(
            server
                .ws(path, &[("authorization", "Bearer nope")])
                .await
                .err(),
            Some(refused(Refusal::BadToken)),
            "{path}"
        );
        let (mut socket, protocol) = server
            .ws(path, &[("authorization", &bearer())])
            .await
            .unwrap();
        assert_eq!(protocol, None, "no subprotocol was offered");
        assert_greets(&mut socket).await;
        // The browser carrier: the marker comes back, the token never does.
        let (mut socket, protocol) = server
            .ws(path, &[("sec-websocket-protocol", &ws_bearer())])
            .await
            .unwrap();
        assert_eq!(protocol.as_deref(), Some("rosetta"), "{path}");
        assert_greets(&mut socket).await;
    }
}

#[tokio::test]
async fn an_allowed_origin_is_never_a_credential() {
    let server = serve(policy()).await;
    for origin in ["rosetta://app", "http://127.0.0.1:5173", SHELL] {
        // rosetta's own app origin, a dev server, the configured extra origin:
        // with no credential each is refused like any other caller...
        let reply = server.get("/api/sessions", &[("origin", origin)]).await;
        assert_eq!(reply.refusal(), (401, Some("missing")), "{origin}");
        // ...and CORS still lets the page read why.
        assert_eq!(reply.header("access-control-allow-origin"), Some(origin));
        let exposed = reply
            .header("access-control-expose-headers")
            .unwrap()
            .to_ascii_lowercase();
        assert!(
            exposed.contains("x-rosetta-auth") && exposed.contains("www-authenticate"),
            "{exposed}"
        );
        assert_eq!(
            server.ws("/ws/meta", &[("origin", origin)]).await.err(),
            Some(refused(Refusal::Missing)),
            "{origin}"
        );
        // With the bearer the same page works, on REST and on the upgrade.
        let ok = server
            .get(
                "/api/sessions",
                &[("origin", origin), ("authorization", &bearer())],
            )
            .await;
        assert_eq!(ok.status, 200, "{origin}");
        assert_eq!(ok.header("access-control-allow-origin"), Some(origin));
        let (mut socket, _) = server
            .ws(
                "/ws/meta",
                &[("origin", origin), ("sec-websocket-protocol", &ws_bearer())],
            )
            .await
            .unwrap();
        assert_greets(&mut socket).await;
        // A preflight must admit Authorization: `*` never covers it.
        let preflight = server
            .request(
                "OPTIONS",
                "/api/sessions",
                &[
                    ("origin", origin),
                    ("access-control-request-method", "GET"),
                    ("access-control-request-headers", "authorization"),
                ],
            )
            .await;
        assert_eq!(preflight.status, 200, "{preflight:?}");
        assert_eq!(
            preflight
                .header("access-control-allow-headers")
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("authorization")
        );
    }
    // A foreign page holding the real bearer is still refused where browsers
    // skip CORS: upgrades and writes.
    assert_eq!(
        server
            .ws(
                "/ws/meta",
                &[
                    ("origin", FOREIGN),
                    ("sec-websocket-protocol", &ws_bearer())
                ]
            )
            .await
            .err()
            .map(|(status, _)| status),
        Some(403)
    );
    let write = server
        .request(
            "POST",
            "/api/ingest/boost",
            &[("origin", FOREIGN), ("authorization", &bearer())],
        )
        .await;
    assert_eq!(write.status, 403);
    let preflight = server
        .request(
            "OPTIONS",
            "/api/sessions",
            &[
                ("origin", FOREIGN),
                ("access-control-request-method", "GET"),
                ("access-control-request-headers", "authorization"),
            ],
        )
        .await;
    assert_eq!(preflight.header("access-control-allow-origin"), None);
}

#[tokio::test]
async fn a_grant_opens_its_own_session_and_nothing_else() {
    let server = serve(policy()).await;
    let grant = grant_for("s1");
    let header = authorization(&grant);
    for path in [
        "/api/sessions/s1",
        "/api/sessions/s1/timeline",
        "/api/sessions/s1/collaboration",
    ] {
        let reply = server.get(path, &[("authorization", &header)]).await;
        assert_eq!(reply.status, 200, "{path}: {reply:?}");
    }
    let timeline = server
        .get("/api/sessions/s1/timeline", &[("authorization", &header)])
        .await;
    assert!(timeline.body.contains("hello from s1"), "{timeline:?}");

    // A native client (fieldd, the phone) sends the header...
    let (mut socket, _) = server
        .ws("/ws/sessions/s1", &[("authorization", &header)])
        .await
        .unwrap();
    assert_greets(&mut socket).await;
    let snapshot = next_frame(&mut socket).await;
    assert_eq!(snapshot["frame"], "timeline_snapshot");
    assert!(snapshot.to_string().contains("hello from s1"), "{snapshot}");
    // ...a renderer behind an egress route sends its origin and the pair.
    let (mut socket, protocol) = server
        .ws(
            "/ws/sessions/s1",
            &[
                ("origin", SHELL),
                ("sec-websocket-protocol", &ws_grant(&grant)),
            ],
        )
        .await
        .unwrap();
    assert_eq!(protocol.as_deref(), Some("rosetta"));
    assert_greets(&mut socket).await;
    let (mut socket, _) = server
        .ws("/ws/collaboration/s1", &[("authorization", &header)])
        .await
        .unwrap();
    assert_greets(&mut socket).await;

    // Another session.
    for path in ["/api/sessions/s2", "/api/sessions/s2/timeline"] {
        let reply = server.get(path, &[("authorization", &header)]).await;
        assert_eq!(reply.refusal(), (401, Some("wrong-session")), "{path}");
    }
    for path in ["/ws/sessions/s2", "/ws/collaboration/s2"] {
        assert_eq!(
            server.ws(path, &[("authorization", &header)]).await.err(),
            Some(refused(Refusal::WrongSession)),
            "{path}"
        );
    }
    // Routes a grant never opens: lists, search, meta, status, the live file
    // read — even for its own session — and writes.
    for path in [
        "/api/status",
        "/api/runtimes",
        "/api/projects",
        "/api/sessions",
        "/api/search?q=hello",
        "/api/search?q=hello&session=s1",
        "/api/sessions/s1/timeline/t1/read/0",
        "/api/projects/p1/heatmap",
    ] {
        let reply = server.get(path, &[("authorization", &header)]).await;
        assert_eq!(reply.refusal(), (401, Some("wrong-route")), "{path}");
    }
    assert_eq!(
        server
            .ws("/ws/meta", &[("authorization", &header)])
            .await
            .err(),
        Some(refused(Refusal::WrongRoute))
    );
    let write = server
        .request(
            "POST",
            "/api/ingest/boost",
            &[
                ("authorization", &header),
                ("content-type", "application/json"),
            ],
        )
        .await;
    assert_eq!(write.refusal(), (401, Some("wrong-route")));
    // A foreign page with a stolen grant: the Origin rule still refuses it.
    assert_eq!(
        server
            .ws(
                "/ws/sessions/s1",
                &[
                    ("origin", FOREIGN),
                    ("sec-websocket-protocol", &ws_grant(&grant))
                ]
            )
            .await
            .err()
            .map(|(status, _)| status),
        Some(403)
    );
}

#[tokio::test]
async fn expired_forged_malformed_and_narrow_grants_are_refused_by_name() {
    let server = serve(policy()).await;
    let expired = grant::mint(&KEY, &GrantClaims::read("s1", "fieldd/w1", now_ms() - 1));
    let forged = grant::mint(
        b"not the daemon's key, 32 bytes!!",
        &GrantClaims::read("s1", "fieldd/w1", now_ms() + 60_000),
    );
    let narrow = grant::mint(
        &KEY,
        &GrantClaims {
            scope: vec!["write".into()],
            ..GrantClaims::read("s1", "fieldd/w1", now_ms() + 60_000)
        },
    );
    for (grant, reason) in [
        (expired.as_str(), Refusal::Expired),
        (forged.as_str(), Refusal::BadSignature),
        (narrow.as_str(), Refusal::WrongScope),
        ("not-a-grant", Refusal::Malformed),
    ] {
        let reply = server
            .get(
                "/api/sessions/s1/timeline",
                &[("authorization", &authorization(grant))],
            )
            .await;
        assert_eq!(reply.refusal(), (401, Some(reason.as_str())), "{grant}");
        assert_eq!(
            server
                .ws(
                    "/ws/sessions/s1",
                    &[("sec-websocket-protocol", &ws_grant(grant))]
                )
                .await
                .err(),
            Some(refused(reason)),
            "{grant}"
        );
    }
    // A grant and a bearer together are two credentials.
    let doubled = server
        .get(
            "/api/sessions/s1",
            &[
                ("authorization", &bearer()),
                ("authorization", &authorization(&grant_for("s1"))),
            ],
        )
        .await;
    assert_eq!(doubled.refusal(), (401, Some("malformed")));
}

#[tokio::test]
async fn every_allow_listed_route_is_a_real_route_a_grant_opens() {
    let server = serve(policy()).await;
    let own = authorization(&grant_for("s1"));
    let other = authorization(&grant_for("s2"));
    for template in GRANT_SESSION_ROUTES.iter().chain([&GRANT_ASSET_ROUTE]) {
        let path = template
            .replace("{id}", "s1")
            .replace("{timeline_id}", "t1")
            .replace("{revision}", "1")
            .replace("{name}", "00.png");
        // Past the credential check: the handler answers (200, 404, or a
        // plain GET on an upgrade route), never 401.
        let reply = server.get(&path, &[("authorization", &own)]).await;
        assert_ne!(reply.status, 401, "{template}: {reply:?}");
        // The template really is judged as a session route (or the asset).
        let expected = if *template == GRANT_ASSET_ROUTE {
            reply.status
        } else {
            401
        };
        let foreign = server.get(&path, &[("authorization", &other)]).await;
        assert_eq!(foreign.status, expected, "{template}: {foreign:?}");
    }
}

#[tokio::test]
async fn without_a_key_grants_are_refused_and_without_a_token_nothing_changes() {
    let token_only = serve(AuthPolicy::new(Some(TOKEN), None, &[]).unwrap()).await;
    let reply = token_only
        .get(
            "/api/sessions/s1",
            &[("authorization", &authorization(&grant_for("s1")))],
        )
        .await;
    assert_eq!(reply.refusal(), (401, Some("grants-disabled")));
    assert_eq!(reply.all("www-authenticate"), [r#"Bearer realm="rosetta""#]);
    drop(token_only);

    let open = serve(AuthPolicy::open()).await;
    for headers in [
        vec![],
        vec![("authorization", "Rosetta-Grant ignored.here")],
        vec![("authorization", "Bearer ignored")],
    ] {
        assert_eq!(open.get("/api/sessions", &headers).await.status, 200);
    }
    let (mut socket, _) = open.ws("/ws/meta", &[]).await.unwrap();
    assert_greets(&mut socket).await;
    // CORS is byte-for-byte what it was.
    let app = open
        .get("/api/status", &[("origin", "rosetta://app")])
        .await;
    assert_eq!(app.status, 200);
    assert_eq!(
        app.header("access-control-expose-headers"),
        Some("x-rosetta-protocol-version")
    );
    // The Origin rule without extra origins: the VibeField shell is foreign.
    for origin in [SHELL, FOREIGN, "null"] {
        assert_eq!(
            open.ws("/ws/meta", &[("origin", origin)])
                .await
                .err()
                .map(|(status, _)| status),
            Some(403),
            "{origin}"
        );
    }
    drop(open);
    // An extra origin extends the rule on a token-less daemon too.
    let open_shell = serve(AuthPolicy::new(None, None, &[SHELL.into()]).unwrap()).await;
    let (mut socket, _) = open_shell
        .ws("/ws/meta", &[("origin", SHELL)])
        .await
        .unwrap();
    assert_greets(&mut socket).await;
}

#[tokio::test]
async fn the_debug_observer_names_the_client_never_the_grant() {
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = events.clone();
    let server = serve(
        policy().with_observer(Arc::new(move |event: GrantEvent<'_>| {
            sink.lock().unwrap().push(format!(
                "{:?} {} {:?}",
                event.cid, event.route, event.refusal
            ));
        })),
    )
    .await;
    let good = grant_for("s1");
    let expired = grant::mint(&KEY, &GrantClaims::read("s1", "fieldd/w1", now_ms() - 1));
    let forged = grant::mint(
        b"not the daemon's key, 32 bytes!!",
        &GrantClaims::read("s1", "attacker", now_ms() + 60_000),
    );
    for (path, grant) in [
        ("/api/sessions/s1/timeline", &good),
        ("/api/sessions/s1/timeline", &expired),
        ("/api/sessions/s1/timeline", &forged),
        ("/api/sessions/s2", &good),
        ("/api/sessions", &good),
    ] {
        server
            .get(path, &[("authorization", &authorization(grant))])
            .await;
    }
    let events = events.lock().unwrap().clone();
    assert_eq!(
        events,
        [
            r#"Some("fieldd/w1") /api/sessions/{id}/timeline None"#,
            r#"Some("fieldd/w1") /api/sessions/{id}/timeline Some(Expired)"#,
            "None /api/sessions/{id}/timeline Some(BadSignature)",
            r#"Some("fieldd/w1") /api/sessions/{id} Some(WrongSession)"#,
            "None /api/sessions Some(WrongRoute)",
        ]
    );
    for grant in [&good, &expired, &forged] {
        let (_, mac) = grant.split_once('.').unwrap();
        assert!(events.iter().all(|event| !event.contains(mac)));
    }
}
