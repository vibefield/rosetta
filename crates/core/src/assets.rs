//! Canonical materialization bytes for projection members.
//!
//! Adapters stay pure: they may decode native inline media or externalize
//! oversized canonical text and emit a [`ProjectionAsset`] alongside the
//! typed reference. The store materializes the bytes before committing that
//! URL. Bytes are skipped by serde because normalized staging is process-local;
//! the durable raw ledger always retains the native Base64 and can recreate them.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable HTTP path served by `rosetta-server` for projection members.
pub const IMAGE_ASSET_URL_PREFIX: &str = "/api/assets/";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionAsset {
    /// Lowercase BLAKE3 hex of the decoded original bytes.
    pub digest: String,
    /// Verified media type from the closed projection-member allowlist.
    pub media_type: String,
    /// Transient materialization bytes. The raw ledger is the durable replay
    /// substrate.
    #[serde(skip, default)]
    pub bytes: Vec<u8>,
}

impl ProjectionAsset {
    pub fn new(media_type: &str, bytes: Vec<u8>) -> Option<Self> {
        extension(media_type)?;
        Some(Self {
            digest: blake3::hash(&bytes).to_hex().to_string(),
            media_type: media_type.to_owned(),
            bytes,
        })
    }

    pub fn extension(&self) -> Option<&'static str> {
        extension(&self.media_type)
    }

    pub fn supported_extension(media_type: &str) -> Option<&'static str> {
        extension(media_type)
    }

    pub fn file_name(&self) -> Option<String> {
        Some(format!("{}.{}", self.digest, self.extension()?))
    }

    pub fn url(&self) -> Option<String> {
        Some(format!("{IMAGE_ASSET_URL_PREFIX}{}", self.file_name()?))
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self::new("text/plain; charset=utf-8", text.into().into_bytes())
            .expect("canonical UTF-8 text is a supported projection member")
    }
}

impl fmt::Debug for ProjectionAsset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectionAsset")
            .field("digest", &self.digest)
            .field("media_type", &self.media_type)
            .field("bytes", &format_args!("<{} bytes>", self.bytes.len()))
            .finish()
    }
}

fn extension(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "application/pdf" => Some("pdf"),
        "application/vnd.rosetta.canonical-document+json" => Some("json"),
        "text/plain; charset=utf-8" => Some("txt"),
        _ => None,
    }
}

/// Compatibility name for callers that specifically construct image
/// members. The value is the same generic, content-addressed projection
/// member and can no longer leak into an opaque tool payload.
pub type ImageAsset = ProjectionAsset;

#[cfg(test)]
mod tests {
    use super::ProjectionAsset;

    #[test]
    fn reference_is_content_addressed_and_does_not_serialize_bytes() {
        let asset = ProjectionAsset::new("image/png", b"png bytes".to_vec()).unwrap();
        assert_eq!(asset.file_name().unwrap().len(), 68);
        assert!(asset.url().unwrap().starts_with("/api/assets/"));

        let encoded = serde_json::to_value(&asset).unwrap();
        assert!(encoded.get("bytes").is_none());
        let restored: ProjectionAsset = serde_json::from_value(encoded).unwrap();
        assert!(restored.bytes.is_empty());
        assert_eq!(restored.digest, asset.digest);
    }

    #[test]
    fn text_members_are_utf8_and_content_addressed() {
        let asset = ProjectionAsset::text("hello");
        assert_eq!(asset.media_type, "text/plain; charset=utf-8");
        assert!(asset.url().unwrap().ends_with(".txt"));
        assert_eq!(asset.bytes, b"hello");
    }
}
