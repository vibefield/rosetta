//! A file an agent sent to the user by path: the pure fact an adapter emits
//! from the transcript alone. The bytes are not in the transcript, so the
//! capture layer observes the file and ledgers it; replay joins that frame
//! back to this fact by `(source_offset, path)` and never reads a filesystem.
//!
//! The fact is transient: the store applies nothing for it. What a reader
//! sees is the tool row's attachment, a path reference first and an asset
//! once a captured frame projects.

use serde::{Deserialize, Serialize};

use crate::rows::Ms;
use crate::timeline::{
    AttachmentKind, AttachmentRef, AttachmentSource, EvidenceOrder, ResourceRef,
};

/// Largest video the capture layer copies into the ledger.
pub const VIDEO_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Captured bytes one raw record may contribute across its files.
pub const RECORD_MAX_BYTES: u64 = 128 * 1024 * 1024;
/// Files examined on one raw record; later ones stay ledger-only.
pub const FILES_PER_RECORD: usize = 32;
/// The closed set of types a sent file may claim.
pub const MEDIA_TYPES: [&str; 8] = [
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
    "image/x-icon",
    "video/mp4",
    "video/quicktime",
];
/// Mutation index of the record's one path-reference tool update. The
/// per-file upgrades sit directly above it, so a captured asset always
/// outranks the reference it replaces and both stay below the two indexes
/// reserved at the top of the range.
pub const REFERENCE_MUTATION: u32 = u32::MAX - 2 - FILES_PER_RECORD as u32;

/// Mutation index of the upgrade for the file at native index `ordinal`.
pub const fn upgrade_mutation(ordinal: u32) -> u32 {
    REFERENCE_MUTATION + 1 + ordinal
}

const PATH_MAX_BYTES: usize = 16384;

/// Whether an absolute path is the device or process tree, or lies under
/// one. Opening such a path can act on the reader itself: `/dev/fd/N` is a
/// duplicate of its own descriptor N. Letter case is ignored because the
/// default macOS volume resolves `/DEV` to `/dev`.
fn is_device_path(path: &str) -> bool {
    let root = path
        .strip_prefix('/')
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default();
    root.eq_ignore_ascii_case("dev") || root.eq_ignore_ascii_case("proc")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentedMedia {
    pub session_id: String,
    /// Raw-record coordinate: the `Document` frame's `start_offset`.
    pub source_offset: u64,
    /// The absolute path as the transcript recorded it, or a recorded
    /// relative path joined to its working directory. Never rewritten:
    /// capture opens exactly this path.
    pub path: String,
    /// The size the transcript recorded.
    pub expected_len: u64,
    /// One of [`MEDIA_TYPES`].
    pub media_type: String,
    pub correlation_key: String,
    /// The reference slot exactly as it was attached to the tool row.
    pub attachment: AttachmentRef,
    /// Evidence of the upgrade that swaps the slot's source to an asset.
    pub evidence: EvidenceOrder,
    pub at: Option<Ms>,
}

impl PresentedMedia {
    /// Whether this fact can be captured and its upgrade applied. The one
    /// producer drops a file whose fact fails, so nothing emitted can fail a
    /// store batch.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.session_id.is_empty() {
            return Err("presented media has no session");
        }
        if self.correlation_key.is_empty() {
            return Err("presented media has no correlation key");
        }
        if !self.path.starts_with('/')
            || self.path.starts_with("//")
            || self.path.contains('\0')
            || self.path.len() > PATH_MAX_BYTES
        {
            return Err("presented media path is not a bounded absolute path");
        }
        if is_device_path(&self.path) {
            return Err("presented media path is a device or process path");
        }
        if self.expected_len == 0 {
            return Err("presented media has no recorded size");
        }
        if !MEDIA_TYPES.contains(&self.media_type.as_str()) {
            return Err("presented media type is not a presented type");
        }
        if self.attachment.validate().is_err() {
            return Err("presented media attachment is invalid");
        }
        match &self.attachment.source {
            AttachmentSource::Reference {
                reference: ResourceRef::File { path, .. },
            } if *path == self.path => {}
            _ => return Err("presented media attachment is not a reference to its path"),
        }
        let kind = if self.media_type.starts_with("image/") {
            AttachmentKind::Image
        } else {
            AttachmentKind::Video
        };
        if self.attachment.kind != kind {
            return Err("presented media attachment kind disagrees with its type");
        }
        if self.evidence.record_offset != self.source_offset {
            return Err("presented media evidence names another record");
        }
        let band = upgrade_mutation(0)..=upgrade_mutation(FILES_PER_RECORD as u32 - 1);
        if !band.contains(&self.evidence.mutation_index) {
            return Err("presented media evidence is outside the upgrade band");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IngestOp;
    use crate::timeline::SourceIdentity;

    fn fact(media_type: &str) -> PresentedMedia {
        let path = "/work/out/clip.mp4".to_owned();
        let kind = if media_type.starts_with("image/") {
            AttachmentKind::Image
        } else {
            AttachmentKind::Video
        };
        let mut attachment = AttachmentRef::new(
            "occurrence:sent-0",
            kind,
            AttachmentSource::Reference {
                reference: ResourceRef::File {
                    path: path.clone(),
                    resolved: true,
                },
            },
        );
        attachment.name = Some("clip.mp4".into());
        attachment.media_type = Some(media_type.into());
        attachment.byte_len = Some(42);
        PresentedMedia {
            session_id: "s1".into(),
            source_offset: 128,
            path,
            expected_len: 42,
            media_type: media_type.into(),
            correlation_key: "tool-1".into(),
            attachment,
            evidence: EvidenceOrder {
                source_identity: SourceIdentity("source".into()),
                generation: 1,
                record_offset: 128,
                mutation_index: upgrade_mutation(0),
            },
            at: Some(7),
        }
    }

    #[test]
    fn a_well_formed_fact_validates_for_every_presented_type() {
        for media_type in MEDIA_TYPES {
            fact(media_type).validate().unwrap();
        }
        let mut last = fact("video/quicktime");
        last.evidence.mutation_index = upgrade_mutation(FILES_PER_RECORD as u32 - 1);
        last.validate().unwrap();
    }

    #[test]
    fn each_broken_field_is_rejected() {
        type Break = fn(&mut PresentedMedia);
        let cases: [(&str, Break); 20] = [
            ("empty session", |f| f.session_id.clear()),
            ("empty correlation key", |f| f.correlation_key.clear()),
            ("relative path", |f| f.path = "out/clip.mp4".into()),
            ("empty path", |f| f.path.clear()),
            ("network path", |f| f.path = "//host/clip.mp4".into()),
            ("nul in path", |f| f.path = "/work/\0clip.mp4".into()),
            ("over-long path", |f| {
                f.path = format!("/{}", "a".repeat(PATH_MAX_BYTES))
            }),
            ("zero size", |f| f.expected_len = 0),
            ("unknown type", |f| f.media_type = "video/webm".into()),
            ("html type", |f| f.media_type = "text/html".into()),
            ("invalid attachment", |f| f.attachment.id.clear()),
            ("attachment names another path", |f| {
                f.path = "/work/out/other.mp4".into()
            }),
            ("attachment is unavailable", |f| {
                f.attachment.source = AttachmentSource::Unavailable {
                    reason: crate::timeline::AttachmentUnavailableReason::NotRecorded,
                }
            }),
            ("attachment is a url", |f| {
                f.attachment.source = AttachmentSource::Reference {
                    reference: ResourceRef::Url {
                        url: "https://example.test/clip.mp4".into(),
                    },
                }
            }),
            ("attachment is a directory", |f| {
                f.attachment.source = AttachmentSource::Reference {
                    reference: ResourceRef::Directory {
                        path: "/work/out/clip.mp4".into(),
                        resolved: true,
                    },
                }
            }),
            ("kind disagrees", |f| {
                f.attachment.kind = AttachmentKind::Image
            }),
            ("evidence names another record", |f| {
                f.evidence.record_offset += 1
            }),
            ("reference mutation", |f| {
                f.evidence.mutation_index = REFERENCE_MUTATION
            }),
            ("below the band", |f| f.evidence.mutation_index = 0),
            ("above the band", |f| {
                f.evidence.mutation_index = upgrade_mutation(FILES_PER_RECORD as u32)
            }),
        ];
        for (name, break_it) in cases {
            let mut broken = fact("video/mp4");
            break_it(&mut broken);
            assert!(broken.validate().is_err(), "{name} must be rejected");
        }
        // The kind rule cuts both ways.
        let mut image = fact("image/png");
        image.attachment.kind = AttachmentKind::Video;
        assert!(image.validate().is_err());
        // The longest accepted path is exactly the bound.
        let mut longest = fact("video/mp4");
        longest.path = format!("/{}", "a".repeat(PATH_MAX_BYTES - 1));
        longest.attachment.source = AttachmentSource::Reference {
            reference: ResourceRef::File {
                path: longest.path.clone(),
                resolved: true,
            },
        };
        longest.validate().unwrap();
    }

    /// The fact as it stands when its path and its slot both name `path`.
    fn at_path(path: &str) -> PresentedMedia {
        let mut fact = fact("video/mp4");
        fact.path = path.to_owned();
        fact.attachment.source = AttachmentSource::Reference {
            reference: ResourceRef::File {
                path: path.to_owned(),
                resolved: true,
            },
        };
        fact
    }

    #[test]
    fn a_device_or_process_path_is_rejected() {
        for path in [
            "/dev",
            "/dev/",
            "/dev/null",
            "/dev/fd/3",
            "/dev/cu.usbmodem1",
            "/proc",
            "/proc/self/fd/3",
            "/proc/1/mem",
            // The default macOS volume folds case: `/DEV/fd/3` is `/dev/fd/3`.
            "/DEV/fd/3",
            "/Dev/null",
            "/Proc/self/mem",
        ] {
            assert_eq!(
                at_path(path).validate(),
                Err("presented media path is a device or process path"),
                "{path}"
            );
        }
        // Only the first component decides, and only as a whole name.
        for path in [
            "/devices/clip.mp4",
            "/dev.mp4",
            "/developer/out/clip.mp4",
            "/process/clip.mp4",
            "/proc.mov",
            "/work/dev/clip.mp4",
            "/work/proc/clip.mp4",
            "/Users/dev/proc/clip.mp4",
        ] {
            at_path(path)
                .validate()
                .unwrap_or_else(|error| panic!("{path}: {error}"));
        }
    }

    #[test]
    fn the_upgrade_band_sits_above_the_reference_and_below_the_reserved_top() {
        assert_eq!(upgrade_mutation(0), REFERENCE_MUTATION + 1);
        assert!(upgrade_mutation(31) < u32::MAX - 1);
        assert_eq!(upgrade_mutation(31), u32::MAX - 2);
        assert_eq!(FILES_PER_RECORD, 32);
    }

    #[test]
    fn the_fact_round_trips_as_a_tagged_ingest_op() {
        let op = IngestOp::PresentedMedia(fact("video/mp4"));
        let value = serde_json::to_value(&op).unwrap();
        assert_eq!(value["op"], "presented_media");
        assert_eq!(value["path"], "/work/out/clip.mp4");
        assert_eq!(value["expected_len"], 42);
        let restored: IngestOp = serde_json::from_value(value).unwrap();
        assert_eq!(restored, op);
        assert_eq!(restored.named_session(), Some("s1"));
        assert!(restored.approx_bytes() > 64);
    }
}
