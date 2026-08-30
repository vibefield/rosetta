//! Lossless codec between the closed timeline contract and SQLite's bounded
//! message columns. `detail` is produced only from [`MessageDetail`]; native
//! objects and arbitrary JSON cannot enter this module.

use std::error::Error;

use rosetta_core::timeline::{AssetRef, AttachmentRef, CitationRef};
use rosetta_core::{
    CanonicalText, GroupDerivation, GroupKind, MessageDetail, Provenance, ResponseGroup,
    TimelineBody, TimelineEntry, TimelineGroup, TokenSnapshot,
};
use rusqlite::Row;
use rusqlite::types::Type;

use crate::{StoreError, tool_codec};

#[derive(Debug)]
pub(crate) struct EncodedMessage {
    pub kind: &'static str,
    pub content: Option<String>,
    pub detail: Option<String>,
}

pub(crate) fn encode(entry: &TimelineEntry) -> Result<EncodedMessage, StoreError> {
    entry
        .validate()
        .map_err(|error| StoreError::ProjectionInvalid {
            message: format!("invalid timeline entry {}: {error}", entry.timeline_id),
        })?;
    let (kind, content, detail) = match &entry.body {
        TimelineBody::User { text, attachments } => {
            let (content, text_member) = optional_text(text.as_ref());
            let detail =
                (!attachments.is_empty() || text_member.is_some()).then(|| MessageDetail::Prose {
                    text_member,
                    attachments: attachments.clone(),
                    citations: Vec::new(),
                });
            ("user", content, detail)
        }
        TimelineBody::Agent {
            text,
            attachments,
            citations,
        } => {
            let (content, text_member) = optional_text(text.as_ref());
            let detail = (!attachments.is_empty()
                || !citations.is_empty()
                || text_member.is_some())
            .then(|| MessageDetail::Prose {
                text_member,
                attachments: attachments.clone(),
                citations: citations.clone(),
            });
            ("agent", content, detail)
        }
        TimelineBody::Thinking { text } => {
            let (content, text_member) = required_text(text);
            let detail = text_member.map(|text_member| MessageDetail::Thinking {
                text_member: Some(text_member),
            });
            ("thinking", Some(content), detail)
        }
        TimelineBody::ToolUse { .. } => ("tool_use", None, None),
        TimelineBody::Notice { notice } => (
            "notice",
            None,
            Some(MessageDetail::Notice {
                notice: notice.clone(),
            }),
        ),
        TimelineBody::Summary { purpose, text } => {
            let (content, text_member) = required_text(text);
            (
                "summary",
                Some(content),
                Some(MessageDetail::Summary {
                    purpose: *purpose,
                    text_member,
                }),
            )
        }
    };
    Ok(EncodedMessage {
        kind,
        content,
        detail: detail
            .map(|detail| serde_json::to_string(&detail))
            .transpose()?,
    })
}

fn optional_text(text: Option<&CanonicalText>) -> (Option<String>, Option<AssetRef>) {
    text.map_or((None, None), |text| {
        let (content, member) = required_text(text);
        (Some(content), member)
    })
}

fn required_text(text: &CanonicalText) -> (String, Option<AssetRef>) {
    match text {
        CanonicalText::Inline { text } => (text.clone(), None),
        CanonicalText::Member { asset, preview, .. } => (preview.clone(), Some(asset.clone())),
    }
}

fn decode_error(index: usize, error: impl Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
}

fn invalid(message: impl Into<String>) -> rusqlite::Error {
    decode_error(
        0,
        std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()),
    )
}

fn parse_detail(row: &Row<'_>) -> rusqlite::Result<Option<MessageDetail>> {
    row.get::<_, Option<String>>("detail")?
        .map(|json| serde_json::from_str(&json).map_err(|error| decode_error(0, error)))
        .transpose()
}

fn text(content: Option<String>, member: Option<AssetRef>) -> Option<CanonicalText> {
    match (content, member) {
        (Some(preview), Some(asset)) => Some(CanonicalText::Member {
            byte_len: asset.byte_len,
            asset,
            preview,
        }),
        (Some(text), None) => Some(CanonicalText::Inline { text }),
        (None, _) => None,
    }
}

fn prose_detail(
    detail: Option<MessageDetail>,
) -> rusqlite::Result<(Option<AssetRef>, Vec<AttachmentRef>, Vec<CitationRef>)> {
    match detail {
        None => Ok((None, Vec::new(), Vec::new())),
        Some(MessageDetail::Prose {
            text_member,
            attachments,
            citations,
        }) => Ok((text_member, attachments, citations)),
        Some(_) => Err(invalid("message detail does not match prose body")),
    }
}

pub(crate) fn decode(row: &Row<'_>) -> rusqlite::Result<TimelineEntry> {
    let kind: String = row.get("type")?;
    let content: Option<String> = row.get("content")?;
    let detail = parse_detail(row)?;
    let (tool, _) = tool_codec::decode(row)?;
    let body = match kind.as_str() {
        "user" => {
            let (member, attachments, citations) = prose_detail(detail)?;
            if !citations.is_empty() {
                return Err(invalid("user prose cannot carry citations"));
            }
            TimelineBody::User {
                text: text(content, member),
                attachments,
            }
        }
        "agent" => {
            let (member, attachments, citations) = prose_detail(detail)?;
            TimelineBody::Agent {
                text: text(content, member),
                attachments,
                citations,
            }
        }
        "thinking" => {
            let member = match detail {
                None => None,
                Some(MessageDetail::Thinking { text_member }) => text_member,
                Some(_) => return Err(invalid("message detail does not match thinking body")),
            };
            TimelineBody::Thinking {
                text: text(content, member)
                    .ok_or_else(|| invalid("thinking row has no canonical text"))?,
            }
        }
        "tool_use" => {
            if detail.is_some() || content.is_some() {
                return Err(invalid("tool row contains prose payload"));
            }
            TimelineBody::ToolUse {
                tool: Box::new(tool.ok_or_else(|| invalid("tool row has no typed interaction"))?),
            }
        }
        "notice" => match detail {
            Some(MessageDetail::Notice { notice }) if content.is_none() => {
                TimelineBody::Notice { notice }
            }
            _ => return Err(invalid("notice row has invalid detail or content")),
        },
        "summary" => match detail {
            Some(MessageDetail::Summary {
                purpose,
                text_member,
            }) => TimelineBody::Summary {
                purpose,
                text: text(content, text_member)
                    .ok_or_else(|| invalid("summary row has no canonical text"))?,
            },
            _ => return Err(invalid("summary row has invalid detail")),
        },
        _ => return Err(invalid(format!("unknown canonical message type {kind:?}"))),
    };
    let response_id: Option<String> = row.get("response_id")?;
    let response_derivation: Option<String> = row.get("response_derivation")?;
    let response = match (response_id, response_derivation) {
        (Some(id), Some(derivation)) => Some(ResponseGroup {
            id,
            derivation: parse_derivation(&derivation)?,
        }),
        (None, None) => None,
        _ => return Err(invalid("response id and derivation disagree")),
    };
    let block_index = row
        .get::<_, Option<i64>>("block_index")?
        .map(u32::try_from)
        .transpose()
        .map_err(|error| decode_error(0, error))?;
    let entry = TimelineEntry {
        timeline_id: row.get("timeline_id")?,
        session_id: row.get("session_id")?,
        ordinal: row.get("ordinal")?,
        group: TimelineGroup {
            id: row.get("group_id")?,
            kind: parse_group_kind(&row.get::<_, String>("group_kind")?)?,
            derivation: parse_derivation(&row.get::<_, String>("group_derivation")?)?,
        },
        response,
        block_index,
        native_id: row.get("uuid")?,
        parent_native_id: row.get("parent_uuid")?,
        at: row.get("ts")?,
        model: row.get("model")?,
        effort: row.get("effort")?,
        body,
        parent_tool_native_id: row.get("parent_tool_native_id")?,
        usage: TokenSnapshot {
            input: row.get("in_tokens")?,
            output: row.get("out_tokens")?,
            cache_read: row.get("cache_r_tokens")?,
            cache_write: row.get("cache_w_tokens")?,
            thinking: row.get("think_tokens")?,
        },
        provenance: Provenance {
            agent_version: row.get("agent_version")?,
            src_cursor: row.get("src_cursor")?,
            src_offset: row.get("src_offset")?,
        },
    };
    entry
        .validate()
        .map_err(|error| invalid(format!("invalid stored timeline entry: {error}")))?;
    Ok(entry)
}

pub(crate) const fn group_kind_name(value: GroupKind) -> &'static str {
    match value {
        GroupKind::HumanTurn => "human_turn",
        GroupKind::AgentCycle => "agent_cycle",
        GroupKind::ParentInvocation => "parent_invocation",
        GroupKind::SystemCycle => "system_cycle",
        GroupKind::Synthetic => "synthetic",
    }
}

pub(crate) const fn derivation_name(value: GroupDerivation) -> &'static str {
    match value {
        GroupDerivation::Native => "native",
        GroupDerivation::ParentLinked => "parent_linked",
        GroupDerivation::Ordered => "ordered",
        GroupDerivation::Synthetic => "synthetic",
    }
}

fn parse_group_kind(value: &str) -> rusqlite::Result<GroupKind> {
    match value {
        "human_turn" => Ok(GroupKind::HumanTurn),
        "agent_cycle" => Ok(GroupKind::AgentCycle),
        "parent_invocation" => Ok(GroupKind::ParentInvocation),
        "system_cycle" => Ok(GroupKind::SystemCycle),
        "synthetic" => Ok(GroupKind::Synthetic),
        _ => Err(invalid(format!("invalid group kind {value:?}"))),
    }
}

fn parse_derivation(value: &str) -> rusqlite::Result<GroupDerivation> {
    match value {
        "native" => Ok(GroupDerivation::Native),
        "parent_linked" => Ok(GroupDerivation::ParentLinked),
        "ordered" => Ok(GroupDerivation::Ordered),
        "synthetic" => Ok(GroupDerivation::Synthetic),
        _ => Err(invalid(format!("invalid group derivation {value:?}"))),
    }
}
