//! Files a `SendUserFile` result lists as sent to the user.
//!
//! The canonical declares `toolUseResult.attachments[]` on that tool's result
//! with `path` and `size` required and `media_type` optional (census
//! evidence only; the writer is closed). Those three fields are everything
//! read here: `isImage`, `pathValidated`, `file_uuid`, `caption` and
//! `display` stay ledger-only. Nothing is opened. The transcript names a
//! path, a size and a type, and the pipeline reads the bytes later.
//!
//! A narrowing failure never fails the record (`docs/groundtruth/claude-code.md`
//! §3): an attachment of any other shape is an empty slot, which keeps its
//! native position and projects nothing.

use rosetta_adapter_support::NormalizeCtx;
use rosetta_adapter_support::presented_media::{PresentedFile, push};
use rosetta_core::IngestOp;
use rosetta_core::presented_media::FILES_PER_RECORD;
use rosetta_core::rows::Ms;
use serde_json::Value;

/// A slot the shared producer always skips: no path, no size, no type.
const EMPTY: PresentedFile<'static> = PresentedFile {
    path: "",
    cwd: None,
    len: 0,
    media_type: "",
};

/// The files of one result, one slot per native attachment in native order,
/// so a file's index here is its index in the transcript. Only the leading
/// slots the shared producer examines are read.
///
/// The declared type is passed through as written: which types are presented
/// is the shared producer's decision, not this reader's. `cwd` is never set:
/// the canonical does not say what a relative `path` is relative to, so
/// resolving one against the record's working directory would name a file
/// the transcript did not, and the shared producer skips it instead.
pub(crate) fn sent_files(tool_use_result: Option<&Value>) -> Vec<PresentedFile<'_>> {
    tool_use_result
        .and_then(|result| result.get("attachments"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(FILES_PER_RECORD)
        .map(|attachment| sent_file(attachment).unwrap_or(EMPTY))
        .collect()
}

fn sent_file(attachment: &Value) -> Option<PresentedFile<'_>> {
    Some(PresentedFile {
        path: attachment.get("path")?.as_str()?,
        cwd: None,
        // A positive integer that fits u64: `as_u64` is `None` for a
        // negative, fractional or non-numeric size.
        len: attachment.get("size")?.as_u64().filter(|size| *size > 0)?,
        media_type: attachment.get("media_type")?.as_str()?,
    })
}

/// Attach the presentable files of a proven, successful `SendUserFile`
/// result to its tool row as path references, then state one capture fact
/// per attached file. The caller proves the opener and the result.
pub(crate) fn push_sent_files(
    ctx: &NormalizeCtx,
    tool_use_id: &str,
    tool_use_result: Option<&Value>,
    at: Option<Ms>,
    ops: &mut Vec<IngestOp>,
) {
    let files = sent_files(tool_use_result);
    if files.is_empty() {
        return;
    }
    let facts = push(ctx, tool_use_id, tool_use_id, &files, at, ops);
    ops.extend(facts.into_iter().map(IngestOp::PresentedMedia));
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn read(result: &Value) -> Vec<(&str, u64, &str)> {
        sent_files(Some(result))
            .into_iter()
            .map(|file| {
                assert_eq!(file.cwd, None);
                (file.path, file.len, file.media_type)
            })
            .collect()
    }

    #[test]
    fn each_attachment_keeps_its_native_slot() {
        let result = json!({"caption": "", "attachments": [
            {"path": "/w/a.png", "size": 10, "media_type": "image/png", "isImage": true},
            {"path": "/w/b.md", "size": 20, "media_type": "text/markdown", "isImage": false},
            {"path": "/w/c.bin", "size": 30, "isImage": false},
            {"path": "/w/d.mp4", "size": 40, "media_type": " Video/MP4 ", "isImage": false,
             "pathValidated": true, "file_uuid": "uuid"},
        ]});
        assert_eq!(
            read(&result),
            [
                ("/w/a.png", 10, "image/png"),
                // The declared type is passed through, whatever it says.
                ("/w/b.md", 20, "text/markdown"),
                ("", 0, ""),
                ("/w/d.mp4", 40, " Video/MP4 "),
            ]
        );
    }

    #[test]
    fn any_other_shape_is_an_empty_slot_or_nothing() {
        for attachment in [
            json!({"path": 7, "size": 10, "media_type": "image/png"}),
            json!({"path": null, "size": 10, "media_type": "image/png"}),
            json!({"size": 10, "media_type": "image/png"}),
            json!({"path": "/w/a.png", "media_type": "image/png"}),
            json!({"path": "/w/a.png", "size": 0, "media_type": "image/png"}),
            json!({"path": "/w/a.png", "size": -4, "media_type": "image/png"}),
            json!({"path": "/w/a.png", "size": 10.5, "media_type": "image/png"}),
            json!({"path": "/w/a.png", "size": 10.0, "media_type": "image/png"}),
            json!({"path": "/w/a.png", "size": "10", "media_type": "image/png"}),
            json!({"path": "/w/a.png", "size": 1e30, "media_type": "image/png"}),
            json!({"path": "/w/a.png", "size": 10, "media_type": 3}),
            json!({"path": "/w/a.png", "size": 10, "media_type": null}),
            json!("/w/a.png"),
            json!(["/w/a.png"]),
            json!(null),
        ] {
            let result = json!({"attachments": [attachment]});
            assert_eq!(read(&result), [("", 0, "")], "{result}");
        }
        assert_eq!(
            read(
                &json!({"attachments": [{"path": "/w/a.png", "size": u64::MAX,
                "media_type": "image/png"}]})
            ),
            [("/w/a.png", u64::MAX, "image/png")]
        );
        for result in [
            json!("sent 1 file"),
            json!(null),
            json!([]),
            json!({}),
            json!({"attachments": "none"}),
            json!({"attachments": {"path": "/w/a.png", "size": 1, "media_type": "image/png"}}),
            json!({"attachments": []}),
        ] {
            assert!(sent_files(Some(&result)).is_empty(), "{result}");
        }
        assert!(sent_files(None).is_empty());
    }

    #[test]
    fn only_the_examined_slots_are_read() {
        let attachments: Vec<Value> = (0..FILES_PER_RECORD + 9)
            .map(|index| json!({"path": format!("/w/{index}.png"), "size": 1, "media_type": "image/png"}))
            .collect();
        let result = json!({ "attachments": attachments });
        let files = sent_files(Some(&result));
        assert_eq!(files.len(), FILES_PER_RECORD);
        assert_eq!(files.last().unwrap().path, "/w/31.png");
    }
}
