//! Rollout storage naming, pinned to rust-v0.167.2 in ground truth.
//! Pure descriptions only: filesystem and decoder work belong to capture.

use rosetta_adapter_support::adapter::{
    InputCandidate, InputEncoding, InputSource, TranscriptLocation,
};

pub fn location(path: &str) -> Option<TranscriptLocation> {
    let plain = path.strip_suffix(".zst").unwrap_or(path);
    if !plain.ends_with(".jsonl") {
        return None;
    }
    let name = plain.rsplit('/').next()?;
    let rest = name.strip_prefix("rollout-")?;
    let timestamp = rest.get(..19)?;
    if !timestamp.is_ascii() {
        return None;
    }
    if timestamp.as_bytes().get(4) != Some(&b'-')
        || timestamp.as_bytes().get(7) != Some(&b'-')
        || rest.as_bytes().get(19) != Some(&b'-')
    {
        return None;
    }
    let active = format!(
        "sessions/{}/{}/{}/{}",
        &timestamp[..4],
        &timestamp[5..7],
        &timestamp[8..10],
        name
    );
    let archived = format!("archived_sessions/{name}");
    if plain != active && plain != archived {
        return None;
    }
    let candidates = [
        (&active, InputEncoding::Plain),
        (&archived, InputEncoding::Plain),
        (&active, InputEncoding::Zstd),
        (&archived, InputEncoding::Zstd),
    ]
    .into_iter()
    .map(|(path, encoding)| InputCandidate {
        path: if encoding == InputEncoding::Zstd {
            format!("{path}.zst")
        } else {
            path.clone()
        },
        encoding,
    })
    .collect();
    Some(TranscriptLocation {
        logical_path: active,
        source: InputSource::single(candidates),
    })
}

pub fn header(
    path: &str,
    first: &[u8],
) -> Result<Option<rosetta_adapter_support::transcripts::TranscriptHeader>, String> {
    use rosetta_adapter_support::transcripts::{TranscriptHeader, TranscriptReference};
    let Some(location) = location(path) else {
        return Ok(None);
    };
    let text = std::str::from_utf8(first).map_err(|e| e.to_string())?;
    let crate::raw::ParsedLine::Known(record) =
        crate::raw::parse_line(text).map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let crate::raw::rollout::RolloutPayload::SessionMeta(meta) = record.payload else {
        return Ok(None);
    };
    let filename = location
        .logical_path
        .rsplit('/')
        .next()
        .ok_or("missing rollout filename")?;
    let ids = filename
        .strip_prefix("rollout-")
        .and_then(|s| s.get(20..))
        .and_then(|s| s.strip_suffix(".jsonl"))
        .ok_or("invalid rollout filename")?;
    let (thread, source_id) = ids.split_once('_').unwrap_or((ids, ids));
    if thread != meta.id {
        return Err("rollout filename and header name different threads".into());
    }
    let base = meta
        .history_base
        .as_ref()
        .filter(|base| !base.is_null())
        .map(|base| {
            Ok::<_, String>(TranscriptReference {
                source_id: base
                    .get("thread_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("missing history base rollout id")?
                    .to_owned(),
                end_offset: base
                    .get("end_byte_offset")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("missing history base offset")?,
                end_ordinal: base
                    .get("end_ordinal_exclusive")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or("missing history base ordinal")?,
            })
        })
        .transpose()?;
    Ok(Some(TranscriptHeader {
        source_id: source_id.to_owned(),
        session_id: meta.id,
        base,
        start_ordinal: record.ordinal.unwrap_or(0),
        paginated: meta.history_mode.as_deref() == Some("paginated"),
    }))
}

/// A copied/relocated CODEX_HOME can retain old absolute SQLite paths. Bind
/// only a recognized managed suffix inside the enrolled root; never read the
/// old absolute location or substitute a different rollout for this thread.
pub fn selection_path(path: &str) -> String {
    if location(path).is_some() {
        return path.to_owned();
    }
    for marker in ["/sessions/", "/archived_sessions/"] {
        if let Some(at) = path.rfind(marker) {
            let relative = &path[at + 1..];
            if location(relative).is_some() {
                return relative.to_owned();
            }
        }
    }
    path.to_owned()
}
