//! Explicitly presented HTML deliverables and their captured review content.
//! Publications are user-facing history; capture provenance never claims that
//! a file observed during a later import is the original published version.

use serde::{Deserialize, Serialize};

use crate::timeline::AssetRef;

pub const HTML_MEDIA_TYPE: &str = "text/html; charset=utf-8";
pub const HTML_MAX_BYTES: usize = 16 * 1024 * 1024;

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
        Ok(())
    }
}
