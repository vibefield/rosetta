//! Authored media occurrences. An attachment's identity is not its blob's digest.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum AttachmentUnavailableReason {
    NotRecorded,
    Omitted,
    InvalidData,
    UnsupportedMedia,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum AttachmentSource {
    Asset { asset: AssetRef },
    Reference { reference: ResourceRef },
    Unavailable { reason: AttachmentUnavailableReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct AttachmentRef {
    /// Stable within the owning message, including when bytes arrive later.
    pub id: String,
    pub kind: AttachmentKind,
    pub label: Option<String>,
    pub name: Option<String>,
    pub media_type: Option<String>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub byte_len: Option<u64>,
    pub source: AttachmentSource,
    pub dimensions: Option<Dimensions>,
    /// Half-open Unicode-scalar spans in the final canonical message text.
    pub message_ranges: Vec<TextRange>,
}

impl AttachmentRef {
    pub fn new(id: impl Into<String>, kind: AttachmentKind, source: AttachmentSource) -> Self {
        Self {
            id: id.into(),
            kind,
            source,
            label: None,
            name: None,
            media_type: None,
            byte_len: None,
            dimensions: None,
            message_ranges: Vec::new(),
        }
    }

    pub fn unavailable(
        id: impl Into<String>,
        kind: AttachmentKind,
        reason: AttachmentUnavailableReason,
    ) -> Self {
        Self::new(id, kind, AttachmentSource::Unavailable { reason })
    }

    pub fn validate(&self) -> Result<(), CanonicalError> {
        required_bounded("attachment id", &self.id, ID_MAX_BYTES)?;
        bounded_option("attachment label", self.label.as_deref(), ID_MAX_BYTES)?;
        bounded_option(
            "attachment name",
            self.name.as_deref(),
            CORE_SCALAR_MAX_BYTES,
        )?;
        bounded_option(
            "attachment media type",
            self.media_type.as_deref(),
            ID_MAX_BYTES,
        )?;
        if self.byte_len == Some(0) {
            return Err(CanonicalError::Invalid(
                "attachment has zero declared bytes".into(),
            ));
        }
        if self
            .dimensions
            .is_some_and(|d| d.width == 0 || d.height == 0)
        {
            return Err(CanonicalError::Invalid(
                "attachment dimensions must be positive".into(),
            ));
        }
        match &self.source {
            AttachmentSource::Asset { asset } => {
                asset.validate()?;
                if self.byte_len.is_some_and(|len| len != asset.byte_len)
                    || self
                        .media_type
                        .as_deref()
                        .is_some_and(|mime| mime != asset.media_type)
                {
                    return Err(CanonicalError::Invalid(
                        "attachment metadata disagrees with asset".into(),
                    ));
                }
            }
            AttachmentSource::Reference { reference } => {
                reference.validate()?;
                match reference {
                    ResourceRef::File { .. } => {}
                    ResourceRef::Url { url }
                        if !url.trim_start().to_ascii_lowercase().starts_with("data:") => {}
                    _ => {
                        return Err(CanonicalError::Invalid(
                            "attachment reference must be a recorded file or non-data URL".into(),
                        ));
                    }
                }
            }
            AttachmentSource::Unavailable { .. } => {}
        }
        bounded_collection(
            "attachment message ranges",
            self.message_ranges.len(),
            CITATION_RANGE_LIMIT,
        )?;
        let mut end = 0;
        for range in &self.message_ranges {
            if range.start >= range.end || range.start < end {
                return Err(CanonicalError::Invalid(
                    "attachment ranges must be positive, ordered and non-overlapping".into(),
                ));
            }
            end = range.end;
        }
        Ok(())
    }
}

pub(super) fn validate_attachments(
    attachments: &[AttachmentRef],
    text: Option<&CanonicalText>,
) -> Result<(), CanonicalError> {
    bounded_collection(
        "message attachments",
        attachments.len(),
        CANONICAL_LIST_LIMIT,
    )?;
    let mut ids = std::collections::BTreeSet::new();
    let mut spans = Vec::new();
    // Full member text is checked before externalization; at the public boundary
    // its byte length remains a conservative upper bound without doing asset IO.
    let bound = match text {
        Some(CanonicalText::Inline { text }) => text.chars().count() as u64,
        Some(CanonicalText::Member { byte_len, .. }) => *byte_len,
        None => 0,
    };
    for attachment in attachments {
        attachment.validate()?;
        if !ids.insert(&attachment.id) {
            return Err(CanonicalError::Invalid(
                "attachment ids must be unique within a message".into(),
            ));
        }
        for range in &attachment.message_ranges {
            if u64::from(range.end) > bound {
                return Err(CanonicalError::Invalid(
                    "attachment range is outside the message".into(),
                ));
            }
            spans.push((range.start, range.end));
        }
    }
    spans.sort_unstable();
    if spans.windows(2).any(|pair| pair[1].0 < pair[0].1) {
        return Err(CanonicalError::Invalid(
            "attachment message ranges conflict".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(id: &str, ranges: Vec<TextRange>) -> AttachmentRef {
        let mut image = AttachmentRef::unavailable(
            id,
            AttachmentKind::Image,
            AttachmentUnavailableReason::NotRecorded,
        );
        image.message_ranges = ranges;
        image
    }
    #[test]
    fn ranges_use_unicode_scalars_and_attachment_occurrences_are_unique() {
        let text = CanonicalText::Inline {
            text: "😀 image".into(),
        };
        let first = image("first", vec![TextRange { start: 2, end: 7 }]);
        assert!(validate_attachments(std::slice::from_ref(&first), Some(&text)).is_ok());
        assert!(validate_attachments(&[first.clone(), first], Some(&text)).is_err());
        assert!(
            validate_attachments(
                &[image("too-far", vec![TextRange { start: 2, end: 8 }])],
                Some(&text)
            )
            .is_err()
        );
        assert!(
            validate_attachments(
                &[
                    image("a", vec![TextRange { start: 0, end: 3 }]),
                    image("b", vec![TextRange { start: 2, end: 4 }])
                ],
                Some(&text)
            )
            .is_err()
        );
        assert!(validate_attachments(&[image("none", vec![])], None).is_ok());
    }
    #[test]
    fn reference_sources_cannot_smuggle_inline_bytes_into_the_canonical_json() {
        let mut attachment = image("image", vec![]);
        attachment.source = AttachmentSource::Reference {
            reference: ResourceRef::Url {
                url: "data:image/png;base64,bytes".into(),
            },
        };
        assert!(attachment.validate().is_err());
        attachment.source = AttachmentSource::Reference {
            reference: ResourceRef::Url {
                url: "https://example.test/a.png".into(),
            },
        };
        assert!(attachment.validate().is_ok());
        attachment.id.clear();
        assert!(attachment.validate().is_err());
    }
}
