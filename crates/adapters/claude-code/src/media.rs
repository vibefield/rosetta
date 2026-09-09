//! Claude authored source variants; generic decoding lives in adapter-support.

use rosetta_adapter_support::media::{self, MaterializedAttachment};
use rosetta_core::timeline::{AttachmentKind, AttachmentUnavailableReason};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AuthoredSource {
    Base64 {
        media_type: String,
        data: String,
    },
    Url {
        url: String,
    },
    Text {
        data: String,
        media_type: Option<String>,
    },
}

pub(crate) fn authored_attachment(
    id: &str,
    kind: AttachmentKind,
    source: &Value,
) -> MaterializedAttachment {
    match serde_json::from_value::<AuthoredSource>(source.clone()) {
        Ok(AuthoredSource::Url { url }) => media::from_url(id, kind, &url, None),
        Ok(AuthoredSource::Base64 { media_type, data }) => {
            media::from_base64(id, kind, &media_type, &data)
        }
        Ok(AuthoredSource::Text { data, media_type })
            if kind == AttachmentKind::Document
                && media_type.as_deref().is_none_or(|mime| {
                    matches!(mime, "text/plain" | "text/plain; charset=utf-8")
                }) =>
        {
            media::from_bytes(
                id,
                kind,
                media_type.as_deref().unwrap_or("text/plain; charset=utf-8"),
                data.into_bytes(),
            )
        }
        _ => media::unavailable(id, kind, AttachmentUnavailableReason::InvalidData),
    }
}
