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
    /// Lowercase BLAKE3 hex of the materialized (possibly transcoded) bytes.
    pub digest: String,
    /// Verified media type from the closed projection-member allowlist.
    pub media_type: String,
    /// Transient materialization bytes. The raw ledger is the durable replay
    /// substrate.
    #[serde(skip, default)]
    pub bytes: Vec<u8>,
    /// Disposable display variants, generated from the same source pixels.
    #[serde(skip, default)]
    pub image_variants: Vec<ImageVariantBytes>,
}

impl ProjectionAsset {
    pub fn new(media_type: &str, bytes: Vec<u8>) -> Option<Self> {
        extension(media_type)?;
        Some(Self {
            digest: blake3::hash(&bytes).to_hex().to_string(),
            media_type: media_type.to_owned(),
            bytes,
            image_variants: Vec::new(),
        })
    }

    pub fn reference(&self) -> crate::timeline::AssetRef {
        let image_variants = if self.image_variants.is_empty() {
            None
        } else {
            Some(
                self.image_variants
                    .iter()
                    .map(|variant| {
                        let digest = blake3::hash(&variant.bytes).to_hex().to_string();
                        (
                            variant.variant,
                            crate::timeline::AssetRef {
                                url: format!("{IMAGE_ASSET_URL_PREFIX}{digest}.webp"),
                                digest,
                                media_type: "image/webp".into(),
                                byte_len: variant.bytes.len() as u64,
                                image_variants: None,
                            },
                        )
                    })
                    .collect(),
            )
        };
        crate::timeline::AssetRef {
            url: self.url().expect("validated projection member"),
            digest: self.digest.clone(),
            media_type: self.media_type.clone(),
            byte_len: self.bytes.len() as u64,
            image_variants,
        }
    }

    pub fn materialized_byte_len(&self) -> usize {
        self.bytes.len()
            + self
                .image_variants
                .iter()
                .map(|v| v.bytes.len())
                .sum::<usize>()
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
            .field("image_variants", &self.image_variants.len())
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
    fn rendition_maps_are_complete_leaf_references_and_skip_transient_bytes() {
        let mut image = ProjectionAsset::new("image/webp", b"full".to_vec()).unwrap();
        image.image_variants = super::ImageVariant::DERIVED
            .into_iter()
            .map(|variant| super::ImageVariantBytes {
                variant,
                bytes: variant.name().as_bytes().to_vec(),
            })
            .collect();
        let reference = image.reference();
        reference.validate().unwrap();
        let value = serde_json::to_value(&image).unwrap();
        assert!(value.get("image_variants").is_none());
        let mut invalid = reference.clone();
        invalid
            .image_variants
            .as_mut()
            .unwrap()
            .remove(&super::ImageVariant::Rail);
        assert!(invalid.validate().is_err());
        let mut invalid = reference.clone();
        invalid
            .image_variants
            .as_mut()
            .unwrap()
            .get_mut(&super::ImageVariant::Rail)
            .unwrap()
            .image_variants = reference.image_variants;
        assert!(invalid.validate().is_err());
        image.media_type = "image/png".into();
        assert!(image.reference().validate().is_err());
    }

    #[test]
    fn text_members_are_utf8_and_content_addressed() {
        let asset = ProjectionAsset::text("hello");
        assert_eq!(asset.media_type, "text/plain; charset=utf-8");
        assert!(asset.url().unwrap().ends_with(".txt"));
        assert_eq!(asset.bytes, b"hello");
    }
}

/// Display recipes are versioned in their URL and cache filename. None of these
/// change attachment occurrence identity or add canonical reconciliation state.
pub const IMAGE_RECIPE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum ImageVariant {
    #[default]
    Full,
    Rail,
    Tile,
    Tile2x,
    Preview,
}

impl ImageVariant {
    pub const DERIVED: [Self; 4] = [Self::Rail, Self::Tile, Self::Tile2x, Self::Preview];

    pub fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Rail => "rail",
            Self::Tile => "tile",
            Self::Tile2x => "tile2x",
            Self::Preview => "preview",
        }
    }

    pub fn edge(self) -> u32 {
        match self {
            Self::Full => 16383,
            Self::Rail => 160,
            Self::Tile => 320,
            Self::Tile2x => 640,
            Self::Preview => 1600,
        }
    }

    pub fn is_cover(self) -> bool {
        matches!(self, Self::Tile | Self::Tile2x)
    }

    pub fn file_name(self, digest: &str) -> String {
        format!(
            "{digest}.image-v{IMAGE_RECIPE_VERSION}-{}.webp",
            self.name()
        )
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ImageVariantBytes {
    pub variant: ImageVariant,
    pub bytes: Vec<u8>,
}
