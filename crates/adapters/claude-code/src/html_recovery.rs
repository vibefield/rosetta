//! Recovery locators derived from the already-parsed native transcript.
//! No IO and no HTML bodies in checkpoints. Groundtruth declares the snapshot /
//! delta backup mappings and Write(file_path, content) tool inputs.

use std::collections::BTreeMap;

use rosetta_adapter_support::{
    NormalizeCtx,
    html_artifacts::{CaptureHints, RecordedHtml},
    tool::lexical_resource,
};
use rosetta_core::{
    html_artifacts::{HTML_MAX_BYTES, HtmlArtifactPublication, is_html_path},
    timeline::ResourceRef,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    normalize::ClaudeCodeState,
    raw::{
        KnownRecord, ParsedLine, parse_line,
        transcript::{
            AssistantContentBlock, KnownAssistantBlock, KnownUserBlock, UserContent,
            UserContentBlock,
        },
    },
};

const LIMIT: usize = 256;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct RecoveryState {
    cwd: Option<String>,
    backups: BTreeMap<String, String>,
    writes: BTreeMap<String, RecordedHtml>,
    pending: BTreeMap<String, (String, RecordedHtml)>,
}

fn local(path: &str, cwd: Option<&str>) -> Option<String> {
    if !is_html_path(path) || path.len() > 16384 {
        return None;
    }
    match lexical_resource(path, cwd, false).ok()? {
        ResourceRef::File { path, .. } if path.starts_with('/') && !path.starts_with("//") => {
            Some(path)
        }
        _ => None,
    }
}

fn bounded_insert<T>(map: &mut BTreeMap<String, T>, key: String, value: T) {
    if map.len() >= LIMIT && !map.contains_key(&key) {
        map.pop_first();
    }
    map.insert(key, value);
}

impl RecoveryState {
    pub(crate) fn history(&mut self, kind: &str, value: &Value) {
        match kind {
            "file-history-snapshot" => {
                if let Some(files) = value
                    .pointer("/snapshot/trackedFileBackups")
                    .and_then(Value::as_object)
                {
                    for (path, backup) in files {
                        self.backup(path, backup);
                    }
                }
            }
            "file-history-delta" => {
                if let (Some(path), Some(backup)) = (
                    value.get("trackingPath").and_then(Value::as_str),
                    value.get("backup"),
                ) {
                    self.backup(path, backup);
                }
            }
            _ => {}
        }
    }

    fn backup(&mut self, path: &str, backup: &Value) {
        let Some(path) = local(path, self.cwd.as_deref()) else {
            return;
        };
        // Null means the file did not yet exist. Never substitute another file
        // with the same basename, and never let a backup name escape its session.
        let Some(name) = backup
            .get("backupFileName")
            .and_then(Value::as_str)
            .filter(|s| {
                !s.is_empty()
                    && s.len() < 512
                    && !s.contains(['/', '\\', '\0'])
                    && *s != "."
                    && *s != ".."
            })
        else {
            self.backups.remove(&path);
            return;
        };
        bounded_insert(&mut self.backups, path, name.to_owned());
    }

    pub(crate) fn record(&mut self, record: &KnownRecord, offset: Option<i64>, raw: &[u8]) {
        match record {
            KnownRecord::Assistant(line) => {
                self.cwd = Some(line.cwd.clone());
                for block in &line.message.content {
                    if let AssistantContentBlock::Known(KnownAssistantBlock::ToolUse {
                        id,
                        name,
                        input,
                        ..
                    }) = block
                        && name == "Write"
                        && let Some(path) = input
                            .get("file_path")
                            .and_then(Value::as_str)
                            .and_then(|p| local(p, Some(&line.cwd)))
                        && input
                            .get("content")
                            .and_then(Value::as_str)
                            .is_some_and(|html| {
                                html.len() <= HTML_MAX_BYTES && !html.contains('\0')
                            })
                        && let Some(offset) = offset.and_then(|n| u64::try_from(n).ok())
                    {
                        bounded_insert(
                            &mut self.pending,
                            id.clone(),
                            (
                                path,
                                RecordedHtml {
                                    offset,
                                    digest: blake3::hash(raw).to_hex().to_string(),
                                },
                            ),
                        );
                    }
                }
            }
            KnownRecord::User(line) => {
                self.cwd = Some(line.cwd.clone());
                if let UserContent::Blocks(blocks) = &line.message.content {
                    for block in blocks {
                        if let UserContentBlock::Known(KnownUserBlock::ToolResult {
                            tool_use_id,
                            is_error,
                            ..
                        }) = block
                            && let Some((path, record)) = self.pending.remove(tool_use_id)
                            && *is_error != Some(true)
                            && line
                                .tool_use_result
                                .as_ref()
                                .and_then(|r| r.get("success"))
                                .and_then(Value::as_bool)
                                != Some(false)
                        {
                            bounded_insert(&mut self.writes, path, record);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn hints(
    ctx: &NormalizeCtx,
    publication: &HtmlArtifactPublication,
    data_root: &str,
) -> CaptureHints {
    let mut hints = CaptureHints {
        urls: publication.url.iter().cloned().collect(),
        ..Default::default()
    };
    if let Some(id) = publication
        .native_id
        .as_deref()
        .filter(|id| id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
    {
        // The native publication id survives Claude's transition to short URLs.
        hints
            .urls
            .push(format!("https://claude.ai/code/artifact/{id}"));
    }
    let Some(state) = ctx
        .state
        .as_deref()
        .and_then(|s| s.downcast_ref::<ClaudeCodeState>())
    else {
        return hints;
    };
    hints.roots.extend(state.html_recovery.cwd.iter().cloned());
    if let Some(path) = &publication.path {
        if let Some(name) = state.html_recovery.backups.get(path)
            && let Some(session) = ctx.native_session_id.as_deref().filter(|s| {
                !s.is_empty() && !s.contains(['/', '\\', '\0']) && *s != "." && *s != ".."
            })
        {
            hints
                .files
                .push(format!("{data_root}/file-history/{session}/{name}"));
        }
        hints.record = state.html_recovery.writes.get(path).cloned();
    }
    hints
}

pub(crate) fn content(raw: &[u8], target: &str) -> Option<Vec<u8>> {
    let ParsedLine::Known(record) = parse_line(std::str::from_utf8(raw).ok()?).ok()? else {
        return None;
    };
    let KnownRecord::Assistant(line) = *record else {
        return None;
    };
    for block in line.message.content {
        if let AssistantContentBlock::Known(KnownAssistantBlock::ToolUse { name, input, .. }) =
            block
            && name == "Write"
            && input
                .get("file_path")
                .and_then(Value::as_str)
                .and_then(|p| local(p, Some(&line.cwd)))
                .as_deref()
                == Some(target)
            && let Some(html) = input.get("content").and_then(Value::as_str)
            && html.len() <= HTML_MAX_BYTES
            && !html.contains('\0')
        {
            return Some(html.as_bytes().to_vec());
        }
    }
    None
}
