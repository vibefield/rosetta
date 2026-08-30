//! Canonical references to large binary projection assets.
//!
//! Adapters stay pure: they may decode a native inline image and emit an
//! [`ImageAsset`] alongside a typed attachment containing
//! [`ImageAsset::url`]. The store materializes the bytes before committing
//! that URL. Bytes are skipped
//! by serde because `source_op` is only temporary replacement staging; the
//! durable raw ledger always retains the native Base64 and can recreate them.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Stable HTTP path served by `rosetta-server` for projection image assets.
pub const IMAGE_ASSET_URL_PREFIX: &str = "/api/assets/";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAsset {
    /// Lowercase BLAKE3 hex of the decoded original bytes.
    pub digest: String,
    /// Verified media type; currently one of PNG, JPEG, GIF, or WebP.
    pub media_type: String,
    /// Transient materialization bytes. The raw ledger, not `source_op`, is
    /// the durable replay substrate.
    #[serde(skip, default)]
    pub bytes: Vec<u8>,
}

impl ImageAsset {
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

    pub fn file_name(&self) -> Option<String> {
        Some(format!("{}.{}", self.digest, self.extension()?))
    }

    pub fn url(&self) -> Option<String> {
        Some(format!("{IMAGE_ASSET_URL_PREFIX}{}", self.file_name()?))
    }
}

impl fmt::Debug for ImageAsset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageAsset")
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
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::ImageAsset;

    #[test]
    fn reference_is_content_addressed_and_does_not_serialize_bytes() {
        let asset = ImageAsset::new("image/png", b"png bytes".to_vec()).unwrap();
        assert_eq!(asset.file_name().unwrap().len(), 68);
        assert!(asset.url().unwrap().starts_with("/api/assets/"));

        let encoded = serde_json::to_value(&asset).unwrap();
        assert!(encoded.get("bytes").is_none());
        let restored: ImageAsset = serde_json::from_value(encoded).unwrap();
        assert!(restored.bytes.is_empty());
        assert_eq!(restored.digest, asset.digest);
    }
}
