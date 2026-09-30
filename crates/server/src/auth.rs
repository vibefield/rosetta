//! Daemon access control: the bearer (RO-1), per-session grants (RO-10) and
//! the browser Origin rule. docs/AUTH.md is the rule; this is its code.
//!
//! Two guards, and neither stands in for the other:
//! - **The Origin rule guards against web pages.** A page cannot forge its
//!   own Origin, so a foreign origin is refused on every WebSocket upgrade and
//!   write (browsers apply no CORS to WebSockets, and a form POST needs no
//!   preflight), and CORS withholds every other response from foreign pages.
//!   Any process that is not a browser can send any Origin it likes, so an
//!   allowed Origin says nothing about who is calling.
//! - **The credential guards against processes.** With a bearer configured,
//!   every route needs `Authorization: Bearer <token>`, or a grant on the
//!   routes a grant opens. A request that passes the Origin rule still needs
//!   a credential; that is what makes the bearer hold against a same-uid
//!   process, or a request relayed from another device.
//!
//! With no bearer configured the daemon behaves as it always has: open on its
//! listener, with the Origin rule for browsers.

use std::borrow::Cow;
use std::sync::Arc;

use axum::extract::rejection::RawPathParamsRejection;
use axum::extract::{MatchedPath, RawPathParams, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::grant::{self, GrantError, SCOPE_READ};

/// The WebSocket subprotocol a credentialed WebSocket client offers beside
/// its credential entry. The daemon selects it, so a browser's handshake
/// completes; the credential entry itself is never echoed.
pub const WS_PROTOCOL: &str = "rosetta";
/// `rosetta.bearer.<base64url(token)>`: the bearer, for WebSocket clients
/// that cannot set `Authorization` (every browser).
pub const WS_BEARER_PREFIX: &str = "rosetta.bearer.";
/// `rosetta.grant.<grant>`: a grant, likewise. A grant is already base64url.
pub const WS_GRANT_PREFIX: &str = "rosetta.grant.";
/// Every 401 names its reason here ([`Refusal::as_str`]).
pub const REASON_HEADER: &str = "x-rosetta-auth";
/// A configured bearer has at least this many characters.
pub const MIN_TOKEN_CHARS: usize = 32;
/// A configured bearer has at most this many characters, so it fits either
/// carrier under [`MAX_CREDENTIAL_BYTES`] once base64url-encoded.
pub const MAX_TOKEN_CHARS: usize = 1024;
/// The largest credential a request may present, on either carrier.
pub const MAX_CREDENTIAL_BYTES: usize = 4096;

/// The session routes a grant can open, by router template. An allow-list:
/// a route added later needs the bearer until someone decides otherwise.
/// Deliberately absent: the lists, search, `/ws/meta`, status, the writes,
/// and the recorded-file route, which reads the file on disk NOW rather than
/// recorded history.
pub const GRANT_SESSION_ROUTES: [&str; 10] = [
    "/ws/sessions/{id}",
    "/ws/collaboration/{id}",
    "/api/sessions/{id}",
    "/api/sessions/{id}/collaboration",
    "/api/sessions/{id}/timeline",
    "/api/sessions/{id}/timeline/{timeline_id}",
    "/api/sessions/{id}/timeline/{timeline_id}/command",
    "/api/sessions/{id}/timeline/{timeline_id}/work",
    "/api/sessions/{id}/timeline/{timeline_id}/plan",
    "/api/sessions/{id}/plans/revisions/{revision}",
];
/// Content-addressed transcript images. Any live read grant opens them: the
/// URL is learned from a timeline, and its digest names bytes, not a session.
pub const GRANT_ASSET_ROUTE: &str = "/api/assets/{name}";

/// Why a request was refused a 401. The spelling is the wire vocabulary of
/// the [`REASON_HEADER`] header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No credential, and this daemon requires one.
    Missing,
    /// A bearer that is not this daemon's.
    BadToken,
    /// A credential that cannot be read: an unknown scheme, two credentials,
    /// bad base64, or a grant that is not a canonical version-1 grant.
    Malformed,
    /// A grant whose MAC does not verify under this daemon's key.
    BadSignature,
    /// An authentic grant whose `exp` has passed.
    Expired,
    /// An authentic grant for another session.
    WrongSession,
    /// An authentic grant without the scope the route needs.
    WrongScope,
    /// A grant on a route grants never open.
    WrongRoute,
    /// A grant presented to a daemon that was given no grant key.
    GrantsDisabled,
}

impl Refusal {
    pub const ALL: [Refusal; 9] = [
        Refusal::Missing,
        Refusal::BadToken,
        Refusal::Malformed,
        Refusal::BadSignature,
        Refusal::Expired,
        Refusal::WrongSession,
        Refusal::WrongScope,
        Refusal::WrongRoute,
        Refusal::GrantsDisabled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Missing => "missing",
            Refusal::BadToken => "bad-token",
            Refusal::Malformed => "malformed",
            Refusal::BadSignature => "bad-signature",
            Refusal::Expired => "expired",
            Refusal::WrongSession => "wrong-session",
            Refusal::WrongScope => "wrong-scope",
            Refusal::WrongRoute => "wrong-route",
            Refusal::GrantsDisabled => "grants-disabled",
        }
    }
}

/// One grant decision, for a debug log. It never carries the grant or its
/// MAC; `cid` is present only once the MAC has verified, so it is always the
/// minter's own words and never text an attacker chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantEvent<'a> {
    pub cid: Option<&'a str>,
    /// The router template (`/ws/sessions/{id}`), never the concrete path.
    pub route: &'a str,
    /// `None` when the grant was accepted.
    pub refusal: Option<Refusal>,
}

pub type GrantObserver = Arc<dyn Fn(GrantEvent<'_>) + Send + Sync>;

/// The daemon's access policy, fixed at startup.
pub struct AuthPolicy {
    /// BLAKE3 of the bearer: comparing digests is constant-time
    /// (`blake3::Hash`'s equality) and leaks nothing about the token's length.
    bearer: Option<blake3::Hash>,
    grant_key: Option<Vec<u8>>,
    /// Extra browser origins, lowercased `scheme://host[:port]`.
    origins: Vec<String>,
    observer: Option<GrantObserver>,
}

impl Default for AuthPolicy {
    fn default() -> Self {
        Self::open()
    }
}

impl AuthPolicy {
    /// No bearer, no grants, the built-in origins: the daemon as it was.
    pub fn open() -> Self {
        Self {
            bearer: None,
            grant_key: None,
            origins: Vec::new(),
            observer: None,
        }
    }

    /// `token` is the bearer: surrounding ASCII whitespace is trimmed, and
    /// what remains must be [`MIN_TOKEN_CHARS`]..=[`MAX_TOKEN_CHARS`] visible
    /// ASCII characters. `grant_key` must be at least
    /// [`grant::MIN_GRANT_KEY_BYTES`] and needs a bearer beside it: a grant
    /// stands in for the bearer on one session's routes, so without a bearer
    /// there is nothing for it to stand in for. `origins` are extra browser
    /// origins, `scheme://host[:port]`, compared case-insensitively.
    pub fn new(
        token: Option<&str>,
        grant_key: Option<Vec<u8>>,
        origins: &[String],
    ) -> Result<Self, String> {
        let bearer = token.map(bearer_digest).transpose()?;
        if let Some(key) = &grant_key {
            if bearer.is_none() {
                return Err(
                    "a grant key needs a bearer token: a grant stands in for the bearer on one \
                     session's routes"
                        .into(),
                );
            }
            if key.len() < grant::MIN_GRANT_KEY_BYTES {
                return Err(format!(
                    "the grant key is {} bytes; at least {} are required",
                    key.len(),
                    grant::MIN_GRANT_KEY_BYTES
                ));
            }
        }
        let origins = origins
            .iter()
            .map(|origin| normalize_origin(origin))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            bearer,
            grant_key,
            origins,
            observer: None,
        })
    }

    /// Receive one [`GrantEvent`] per grant decision.
    pub fn with_observer(mut self, observer: GrantObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Every route needs a credential.
    pub fn bearer_required(&self) -> bool {
        self.bearer.is_some()
    }

    /// Grants are verified (a key was given).
    pub fn grants_enabled(&self) -> bool {
        self.grant_key.is_some()
    }

    /// The configured extra origins, normalized.
    pub fn extra_origins(&self) -> &[String] {
        &self.origins
    }

    /// The Origin rule: any port on a loopback host, the Tauri and Electron
    /// shells, plus the configured extra origins. A `null` origin (sandboxed
    /// iframes, some file:// contexts) is never allowed: any site can
    /// manufacture one.
    pub fn origin_allowed(&self, origin: &HeaderValue) -> bool {
        if local_origin(origin) {
            return true;
        }
        let Ok(text) = origin.to_str() else {
            return false;
        };
        self.origins
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(text))
    }

    fn observe(&self, cid: Option<&str>, route: &str, refusal: Option<Refusal>) {
        if let Some(observer) = &self.observer {
            observer(GrantEvent {
                cid,
                route,
                refusal,
            });
        }
    }

    /// Admit a grant for this request, or name why not. The route is judged
    /// before the grant is read; the MAC before its claims.
    fn admit_grant(
        &self,
        grant: &str,
        method: &Method,
        route: Option<&str>,
        params: Option<&RawPathParams>,
        now_ms: i64,
    ) -> Result<(), Refusal> {
        let Some(key) = &self.grant_key else {
            return Err(Refusal::GrantsDisabled);
        };
        let route_name = route.unwrap_or("(unrouted)");
        let Some(reach) = grant_reach(method, route, params) else {
            self.observe(None, route_name, Some(Refusal::WrongRoute));
            return Err(Refusal::WrongRoute);
        };
        let claims = match grant::open(key, grant) {
            Ok(claims) => claims,
            Err(error) => {
                let refusal = match error {
                    GrantError::BadSignature => Refusal::BadSignature,
                    GrantError::Malformed | GrantError::Expired => Refusal::Malformed,
                };
                self.observe(None, route_name, Some(refusal));
                return Err(refusal);
            }
        };
        let verdict = if claims.exp <= now_ms {
            Err(Refusal::Expired)
        } else if let Reach::Session(id) = reach
            && claims.sid != id
        {
            Err(Refusal::WrongSession)
        } else if !claims.allows(SCOPE_READ) {
            Err(Refusal::WrongScope)
        } else {
            Ok(())
        };
        self.observe(Some(&claims.cid), route_name, verdict.err());
        verdict
    }

    fn refuse(&self, reason: Refusal) -> Response {
        let mut response = (
            StatusCode::UNAUTHORIZED,
            format!("rosetta: unauthorized ({})\n", reason.as_str()),
        )
            .into_response();
        let headers = response.headers_mut();
        headers.append(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Bearer realm="rosetta""#),
        );
        if self.grants_enabled() {
            headers.append(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static(r#"Rosetta-Grant realm="rosetta""#),
            );
        }
        headers.insert(REASON_HEADER, HeaderValue::from_static(reason.as_str()));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

fn bearer_digest(token: &str) -> Result<blake3::Hash, String> {
    let token = token.trim_ascii();
    if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("the bearer token must be visible ASCII characters with no spaces".into());
    }
    if !(MIN_TOKEN_CHARS..=MAX_TOKEN_CHARS).contains(&token.len()) {
        return Err(format!(
            "the bearer token is {} characters; {MIN_TOKEN_CHARS} to {MAX_TOKEN_CHARS} are required",
            token.len()
        ));
    }
    Ok(blake3::hash(token.as_bytes()))
}

/// `scheme://host[:port]`, lowercased. `null`, wildcards, paths, queries
/// and credentials are refused: an origin is exactly what a browser sends.
fn normalize_origin(text: &str) -> Result<String, String> {
    let origin = text.trim().to_ascii_lowercase();
    let well_formed = origin.split_once("://").is_some_and(|(scheme, authority)| {
        scheme.starts_with(|c: char| c.is_ascii_lowercase())
            && scheme.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.')
            })
            && !authority.is_empty()
            && authority
                .chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '/' | '?' | '#' | '@' | '*' | '\\'))
    });
    if well_formed {
        Ok(origin)
    } else {
        Err(format!(
            "allowed origin {text:?} is not scheme://host[:port] (no path, query, credentials, \
             wildcard or null)"
        ))
    }
}

/// The built-in Origin rule: any port on a loopback host, plus the Tauri and
/// Electron shells.
fn local_origin(origin: &HeaderValue) -> bool {
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

enum Reach<'a> {
    Session(&'a str),
    Asset,
}

/// What a grant could open on this request, if anything: a read (GET/HEAD)
/// of an allow-listed route.
fn grant_reach<'a>(
    method: &Method,
    route: Option<&str>,
    params: Option<&'a RawPathParams>,
) -> Option<Reach<'a>> {
    if method != Method::GET && method != Method::HEAD {
        return None;
    }
    let route = route?;
    if route == GRANT_ASSET_ROUTE {
        return Some(Reach::Asset);
    }
    if !GRANT_SESSION_ROUTES.contains(&route) {
        return None;
    }
    params?
        .iter()
        .find(|(name, _)| *name == "id")
        .map(|(_, id)| Reach::Session(id))
}

enum Credential<'a> {
    Bearer(Cow<'a, str>),
    Grant(&'a str),
}

fn keep<'a>(slot: &mut Option<Credential<'a>>, found: Credential<'a>) -> Result<(), Refusal> {
    match slot.replace(found) {
        Some(_) => Err(Refusal::Malformed),
        None => Ok(()),
    }
}

/// The one credential a request carries: `Authorization` when present,
/// otherwise a WebSocket subprotocol entry. Two credentials are malformed.
fn presented(headers: &HeaderMap) -> Result<Option<Credential<'_>>, Refusal> {
    let mut found = None;
    for value in headers.get_all(header::AUTHORIZATION) {
        let text = value.to_str().map_err(|_| Refusal::Malformed)?;
        if text.len() > MAX_CREDENTIAL_BYTES {
            return Err(Refusal::Malformed);
        }
        let (scheme, rest) = text.trim().split_once(' ').ok_or(Refusal::Malformed)?;
        let rest = rest.trim_start();
        if scheme.eq_ignore_ascii_case("bearer") {
            keep(&mut found, Credential::Bearer(Cow::Borrowed(rest)))?;
        } else if scheme.eq_ignore_ascii_case("rosetta-grant") {
            keep(&mut found, Credential::Grant(rest))?;
        } else {
            return Err(Refusal::Malformed);
        }
    }
    if found.is_some() {
        return Ok(found);
    }
    for value in headers.get_all(header::SEC_WEBSOCKET_PROTOCOL) {
        let Ok(text) = value.to_str() else {
            continue;
        };
        for protocol in text.split(',').map(str::trim) {
            if protocol.len() > MAX_CREDENTIAL_BYTES {
                return Err(Refusal::Malformed);
            }
            if let Some(encoded) = protocol.strip_prefix(WS_BEARER_PREFIX) {
                let token = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .ok_or(Refusal::Malformed)?;
                keep(&mut found, Credential::Bearer(Cow::Owned(token)))?;
            } else if let Some(grant) = protocol.strip_prefix(WS_GRANT_PREFIX) {
                keep(&mut found, Credential::Grant(grant))?;
            }
        }
    }
    Ok(found)
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

/// The credential check: one layer over every route. It runs after routing,
/// so the matched template and its session id decide what a grant may open.
/// CORS answers preflights outside it; a refusal still passes back through
/// CORS, so an allowed page can read its 401.
pub(crate) async fn authenticate(
    State(policy): State<Arc<AuthPolicy>>,
    params: Result<RawPathParams, RawPathParamsRejection>,
    request: Request,
    next: Next,
) -> Response {
    let Some(bearer) = policy.bearer else {
        return next.run(request).await;
    };
    let decision = match presented(request.headers()) {
        Err(refusal) => Err(refusal),
        Ok(None) => Err(Refusal::Missing),
        Ok(Some(Credential::Bearer(token))) => {
            if blake3::hash(token.as_bytes()) == bearer {
                Ok(())
            } else {
                Err(Refusal::BadToken)
            }
        }
        Ok(Some(Credential::Grant(grant))) => policy.admit_grant(
            grant,
            request.method(),
            request
                .extensions()
                .get::<MatchedPath>()
                .map(MatchedPath::as_str),
            params.as_ref().ok(),
            now_ms(),
        ),
    };
    match decision {
        Ok(()) => next.run(request).await,
        Err(refusal) => policy.refuse(refusal),
    }
}

/// The Origin rule, enforced: WebSocket upgrades (browsers apply no CORS to
/// them) and writes (a form POST needs no preflight) refuse a foreign browser
/// origin with 403. A request without an Origin — every client that is not a
/// browser — passes here and answers to the credential check instead.
pub(crate) async fn require_allowed_origin(
    State(policy): State<Arc<AuthPolicy>>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && !policy.origin_allowed(origin)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn describe(headers: &HeaderMap) -> Result<Option<String>, Refusal> {
        presented(headers).map(|found| {
            found.map(|credential| match credential {
                Credential::Bearer(token) => format!("bearer:{token}"),
                Credential::Grant(grant) => format!("grant:{grant}"),
            })
        })
    }

    #[test]
    fn credentials_come_from_authorization_or_one_subprotocol_entry() {
        assert_eq!(describe(&headers(&[])), Ok(None));
        assert_eq!(
            describe(&headers(&[("authorization", "Bearer abc")])),
            Ok(Some("bearer:abc".into()))
        );
        assert_eq!(
            describe(&headers(&[("authorization", "bearer   abc")])),
            Ok(Some("bearer:abc".into()))
        );
        assert_eq!(
            describe(&headers(&[("authorization", "Rosetta-Grant p.m")])),
            Ok(Some("grant:p.m".into()))
        );
        let encoded = URL_SAFE_NO_PAD.encode("tok/en=");
        assert_eq!(
            describe(&headers(&[(
                "sec-websocket-protocol",
                &format!("rosetta, rosetta.bearer.{encoded}")
            )])),
            Ok(Some("bearer:tok/en=".into()))
        );
        assert_eq!(
            describe(&headers(&[
                ("sec-websocket-protocol", "rosetta"),
                ("sec-websocket-protocol", "rosetta.grant.p.m"),
            ])),
            Ok(Some("grant:p.m".into()))
        );
        // Authorization wins; the subprotocol list is not consulted.
        assert_eq!(
            describe(&headers(&[
                ("authorization", "Bearer abc"),
                ("sec-websocket-protocol", "rosetta, rosetta.grant.p.m"),
            ])),
            Ok(Some("bearer:abc".into()))
        );
        // Offering only the marker, or unrelated protocols, is no credential.
        assert_eq!(
            describe(&headers(&[("sec-websocket-protocol", "rosetta, chat")])),
            Ok(None)
        );
    }

    #[test]
    fn unreadable_or_doubled_credentials_are_malformed() {
        for pairs in [
            vec![("authorization", "Basic dXNlcjpwYXNz")],
            vec![("authorization", "Bearer")],
            vec![("authorization", "Bearer a"), ("authorization", "Bearer b")],
            vec![(
                "sec-websocket-protocol",
                "rosetta, rosetta.grant.a.b, rosetta.grant.c.d",
            )],
            vec![("sec-websocket-protocol", "rosetta, rosetta.bearer.@@@")],
        ] {
            assert_eq!(
                describe(&headers(&pairs)),
                Err(Refusal::Malformed),
                "{pairs:?}"
            );
        }
        let long = format!("Bearer {}", "a".repeat(MAX_CREDENTIAL_BYTES));
        assert_eq!(
            describe(&headers(&[("authorization", &long)])),
            Err(Refusal::Malformed)
        );
    }

    #[test]
    fn a_bearer_is_long_visible_ascii_and_a_grant_key_needs_one() {
        assert!(AuthPolicy::new(Some(&format!("  {TOKEN}\n")), None, &[]).is_ok());
        assert!(AuthPolicy::new(Some("short"), None, &[]).is_err());
        assert!(AuthPolicy::new(Some(&format!("{TOKEN} {TOKEN}")), None, &[]).is_err());
        assert!(AuthPolicy::new(Some(&"x".repeat(MAX_TOKEN_CHARS + 1)), None, &[]).is_err());
        let key = vec![7u8; grant::MIN_GRANT_KEY_BYTES];
        assert!(AuthPolicy::new(None, Some(key.clone()), &[]).is_err());
        assert!(AuthPolicy::new(Some(TOKEN), Some(key[1..].to_vec()), &[]).is_err());
        let policy = AuthPolicy::new(Some(TOKEN), Some(key), &[]).unwrap();
        assert!(policy.bearer_required() && policy.grants_enabled());
        assert!(!AuthPolicy::open().bearer_required());
    }

    #[test]
    fn extra_origins_are_exact_origins_compared_without_case() {
        let policy = AuthPolicy::new(
            None,
            None,
            &[
                "VibeField-App://Shell".into(),
                "https://example.test:8443".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            policy.extra_origins(),
            ["vibefield-app://shell", "https://example.test:8443"]
        );
        for (origin, allowed) in [
            ("vibefield-app://shell", true),
            ("VIBEFIELD-APP://SHELL", true),
            ("https://example.test:8443", true),
            ("https://example.test", false),
            ("vibefield-app://shell.evil", false),
            ("rosetta://app", true),
            ("http://127.0.0.1:5173", true),
            ("null", false),
        ] {
            assert_eq!(
                policy.origin_allowed(&HeaderValue::from_static(origin)),
                allowed,
                "{origin}"
            );
        }
        assert!(
            !AuthPolicy::open().origin_allowed(&HeaderValue::from_static("vibefield-app://shell"))
        );
        for bad in [
            "null",
            "*",
            "vibefield-app",
            "https://*.example.test",
            "http://localhost:3000/",
            "https://user@example.test",
            "https://example.test?x",
            "1http://x",
            "https://",
        ] {
            assert!(AuthPolicy::new(None, None, &[bad.into()]).is_err(), "{bad}");
        }
    }

    #[test]
    fn refusal_spellings_are_distinct() {
        let mut spellings: Vec<_> = Refusal::ALL.iter().map(|r| r.as_str()).collect();
        spellings.sort_unstable();
        spellings.dedup();
        assert_eq!(spellings.len(), Refusal::ALL.len());
    }
}
