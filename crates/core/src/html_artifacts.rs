//! Explicitly presented HTML deliverables and their captured review content.
//! Publications are user-facing history; capture provenance never claims that
//! a file observed during a later import is the original published version.

use serde::{Deserialize, Serialize};

use crate::timeline::AssetRef;

pub const HTML_MEDIA_TYPE: &str = "text/html; charset=utf-8";
pub const HTML_MAX_BYTES: usize = 16 * 1024 * 1024;
/// The longest publish description kept on a publication (RO-33, schema 30).
/// An adapter clips at a character boundary; [`HtmlArtifactPublication::validate`]
/// refuses anything longer.
pub const HTML_DESCRIPTION_MAX_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct HtmlArtifactPublication {
    /// One presentation/publication, stable across normalization and replay.
    pub id: String,
    /// Native artifact identity within a runtime, otherwise session + locator.
    pub artifact_id: String,
    pub session_id: String,
    pub timeline_id: String,
    pub title: String,
    pub filename: String,
    pub path: Option<String>,
    pub url: Option<String>,
    pub native_id: Option<String>,
    pub version: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub published_at: Option<i64>,
    /// Raw-record coordinate for the separately ledgered file observation.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub source_offset: u64,
    /// The publish request's own description, as the agent wrote it, bounded
    /// by [`HTML_DESCRIPTION_MAX_BYTES`] (RO-33, schema 30). Null when the
    /// request carried none, or when the source records no request at all.
    #[serde(default)]
    pub description: Option<String>,
    /// How many members of the publish request's `files` map the library did
    /// not keep beside this page (RO-34a, schema 30). Zero when the request
    /// named none; null when the source records no request (a delivery link,
    /// another agent's presentation), which means unknown.
    #[serde(default)]
    pub dropped_files: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum HtmlArtifactAvailability {
    /// The ledger contains the content, either inline in the publication or
    /// in a stable file observation taken at `captured_at`.
    Captured,
    /// The publication was recorded without a file observation (old ledgers,
    /// remote-only publications, or a capture-less normalization caller).
    NotCaptured,
    Missing,
    TooLarge,
    Unsupported,
    ChangedDuringRead,
}

/// A captured file enriches every presentation of this exact locator at this
/// raw-record coordinate. It is applied under the owning source generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HtmlArtifactContent {
    pub session_id: String,
    pub source_offset: u64,
    /// None for HTML embedded in the native publication request.
    pub path: Option<String>,
    pub availability: HtmlArtifactAvailability,
    /// File-observation time; inline content is already part of the raw record.
    pub captured_at: Option<i64>,
    /// Selected recovery source, when these bytes came from a backup, archive,
    /// or recorded write. This is a recovered copy, not a verified publication revision.
    #[serde(default)]
    pub recovered_from: Option<String>,
    pub asset: Option<AssetRef>,
}

pub fn is_html_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.ends_with(".html") || path.ends_with(".htm")
}

/// Length-delimited identities cannot collide when native values contain the
/// separator used by a display path or title.
pub fn identity(parts: &[&str]) -> String {
    let mut hash = blake3::Hasher::new();
    for part in parts {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}

impl HtmlArtifactPublication {
    pub fn validate(&self) -> Result<(), &'static str> {
        if [&self.id, &self.artifact_id].iter().any(|id| {
            id.len() != 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        }) {
            return Err("invalid HTML artifact identity");
        }
        if self.session_id.is_empty() || self.timeline_id.is_empty() {
            return Err("HTML artifact has no session or transcript anchor");
        }
        if self.title.is_empty() || self.title.len() > 4096 || self.filename.len() > 4096 {
            return Err("invalid HTML artifact display metadata");
        }
        if !is_html_path(&self.filename) {
            return Err("HTML artifact filename has no HTML extension");
        }
        if self
            .path
            .as_ref()
            .is_some_and(|path| path.len() > 16384 || path.contains('\0') || !is_html_path(path))
        {
            return Err("invalid HTML artifact path");
        }
        if self.url.as_ref().is_some_and(|url| {
            url.len() > 16384 || !url.starts_with("https://") || url.chars().any(char::is_control)
        }) {
            return Err("invalid HTML artifact publication URL");
        }
        if self.path.is_none() && self.url.is_none() && self.native_id.is_none() {
            return Err("HTML artifact has no recorded locator");
        }
        if self.native_id.as_ref().is_some_and(|v| v.len() > 4096)
            || self.version.as_ref().is_some_and(|v| v.len() > 4096)
        {
            return Err("HTML artifact native identity is too long");
        }
        if self.description.as_ref().is_some_and(|description| {
            description.is_empty()
                || description.len() > HTML_DESCRIPTION_MAX_BYTES
                || description.contains('\0')
        }) {
            return Err("invalid HTML artifact description");
        }
        Ok(())
    }
}

/// The served shell's version (RO-32, 2026-11-05). It changes whenever
/// [`runnable_document`]'s output for the same captured page would change. An
/// export that asks for the shell (`shell: "room"`) answers it as
/// `shell.version`; the export never writes the shell unasked (2026-11-06:
/// it places a page's own bytes by digest).
pub const SHELL_VERSION: &str = "1";

/// The public hosts the room admits for a captured page, per directive, as
/// data. Every export answers them as its `environment.hosts`, with or
/// without the shell, so a host that serves an exported page (a supervisor's
/// request guard and policy headers, a phone's content rules) admits exactly
/// these beside the page's own origin, and never re-derives the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellHosts {
    pub script: &'static [&'static str],
    pub style: &'static [&'static str],
    pub font: &'static [&'static str],
}

impl ShellHosts {
    /// The hosts per Content-Security-Policy directive, in the shell
    /// policy's order: an export's `environment.hosts`, keyed by the
    /// directive [`shell_policy`] names them under.
    pub fn directives(&self) -> [(&'static str, &'static [&'static str]); 3] {
        [
            ("script-src", self.script),
            ("style-src", self.style),
            ("font-src", self.font),
        ]
    }

    /// Every host once, in directive order (script, style, font): the
    /// `shell.hosts` of an export that wrote the shell.
    pub fn all(&self) -> Vec<&'static str> {
        let mut hosts: Vec<&'static str> = Vec::new();
        for &host in self.script.iter().chain(self.style).chain(self.font) {
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
        hosts
    }
}

/// The CDNs rosetta's room admits for a captured page
/// (`clients/room/src/components/artifacts/preview.ts`). The room's parity
/// test reads them back from `crates/core/tests/golden/served-shell.json`.
pub const SHELL_HOSTS: ShellHosts = ShellHosts {
    script: &["cdn.jsdelivr.net", "cdnjs.cloudflare.com", "unpkg.com"],
    style: &["fonts.googleapis.com", "cdn.jsdelivr.net"],
    font: &["fonts.gstatic.com"],
};

/// The host variables the room defines for a captured page (the names the
/// page's author wrote against), with their light values. Every export
/// answers them as its `environment.variables`, so a serving host can theme
/// a page without wrapping it. The served shell is light only: it sets
/// `color-scheme: light` and defines no dark values.
pub const SHELL_VARIABLES: &[(&str, &str)] = &[
    ("--color-background-primary", "#fff"),
    ("--color-background-secondary", "#f5f5f3"),
    ("--color-background-tertiary", "#eeede9"),
    ("--color-text-primary", "#252521"),
    ("--color-text-secondary", "#62625c"),
    ("--color-text-tertiary", "#85857e"),
    ("--color-border-primary", "#d5d5ce"),
    ("--color-border-secondary", "#e5e5df"),
    ("--color-border-tertiary", "#efefea"),
    ("--color-background-info", "#eaf1ff"),
    ("--color-text-info", "#2e5b99"),
    ("--color-background-success", "#edf6e8"),
    ("--color-text-success", "#3c6530"),
    ("--color-background-warning", "#fff3d6"),
    ("--color-text-warning", "#855c12"),
    ("--color-background-danger", "#fdecea"),
    ("--color-text-danger", "#9a3830"),
    ("--border-radius-md", "8px"),
    ("--border-radius-lg", "12px"),
    ("--font-sans", "system-ui,sans-serif"),
    ("--font-mono", "ui-monospace,monospace"),
];

/// The room's modest base rules, after the variables. Authored styles win.
pub const SHELL_BASE_STYLE: &str = "html { background: #fff; color: #252521; font-family: var(--font-sans); } body { margin: 24px; } * { box-sizing: border-box; } button,input,select,textarea { font: inherit; } a { color: #315e9c; }";

/// The served shell's policy, one directive per entry. It is the room's
/// preview policy with three deliberate differences, because an exported page
/// is served from an origin of its own instead of an opaque `srcdoc` frame:
/// `'self'` joins the script, style, font, image and media sources (a page's
/// own members load), `connect-src` is `'self'` (it may fetch its own data,
/// never another host), and `base-uri` is `'self'` (the room pins
/// `https://artifact.invalid`, which would send every relative URL nowhere).
pub fn shell_policy() -> Vec<String> {
    let sources = |hosts: &[&str]| {
        hosts
            .iter()
            .map(|host| format!("https://{host}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    vec![
        "default-src 'none'".to_owned(),
        format!(
            "script-src 'self' 'unsafe-inline' 'unsafe-eval' {}",
            sources(SHELL_HOSTS.script)
        ),
        format!(
            "style-src 'self' 'unsafe-inline' {}",
            sources(SHELL_HOSTS.style)
        ),
        format!("font-src 'self' data: {}", sources(SHELL_HOSTS.font)),
        "img-src 'self' data: blob:".to_owned(),
        "media-src 'self' data: blob:".to_owned(),
        "connect-src 'self'".to_owned(),
        "frame-src 'none'".to_owned(),
        "object-src 'none'".to_owned(),
        "worker-src 'none'".to_owned(),
        "base-uri 'self'".to_owned(),
        "form-action 'none'".to_owned(),
    ]
}

/// One captured page as a runnable document for its own origin, written by
/// RO-32's `shell: "room"` export option (the export's default places the
/// page's own bytes by digest): the room's preview shell rebuilt as data,
/// with no `<base>`, the policy of
/// [`shell_policy`], a viewport meta (a phone lays the page out at its own
/// width), the room's variables and base rules, light only. The captured
/// markup goes inside `<body>` exactly as the room places it, so a full
/// document and a fragment both parse. A meta policy cannot carry `sandbox`,
/// `frame-ancestors` or reporting; the serving host adds those.
pub fn runnable_document(html: &str) -> String {
    let policy = shell_policy().join("; ");
    let variables: String = SHELL_VARIABLES
        .iter()
        .map(|(name, value)| format!(" {name}: {value};"))
        .collect();
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\">\
         <meta name=\"referrer\" content=\"no-referrer\"><style>\n    \
         :root {{ color-scheme: light;{variables} }}\n    {SHELL_BASE_STYLE}\n  \
         </style></head><body>{html}</body></html>"
    )
}
