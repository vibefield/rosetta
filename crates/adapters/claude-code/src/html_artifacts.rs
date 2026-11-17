//! Confirmed native publications. Non-publishing Artifact actions never enter
//! the review library, even when a list/read response happens to contain paths.

use rosetta_adapter_support::{NormalizeCtx, html_artifacts::publication};
use rosetta_core::{
    IngestOp,
    html_artifacts::{HTML_DESCRIPTION_MAX_BYTES, HtmlArtifactPublication, identity, is_html_path},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A publication request awaiting its tool result. A checkpoint written by
/// normalizer 38 or earlier may also hold `inline_html`/`inline_availability`
/// for a files-map string; that reading was wrong (a string names a source
/// path, see `member_source`) and those fields are ignored on restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingPublication {
    publications: Vec<HtmlArtifactPublication>,
    native: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PublishedIdentity {
    artifact_id: String,
    native_id: Option<String>,
    url: Option<String>,
}

impl PublishedIdentity {
    pub(crate) fn of(row: &HtmlArtifactPublication) -> Self {
        Self {
            artifact_id: row.artifact_id.clone(),
            native_id: row.native_id.clone(),
            url: row.url.clone(),
        }
    }

    pub(crate) fn apply(&self, row: &mut HtmlArtifactPublication) {
        row.artifact_id.clone_from(&self.artifact_id);
        row.native_id.clone_from(&self.native_id);
        row.url.clone_from(&self.url);
        // A later file link identifies the known artifact, but does not prove
        // that its current file bytes are the native published revision.
    }
}

pub(crate) fn request(
    ctx: &NormalizeCtx,
    timeline_id: &str,
    name: &str,
    input: &Value,
    cwd: &str,
    at: Option<i64>,
) -> Option<PendingPublication> {
    let native = name == "Artifact";
    if !native && name != "SendUserFile" {
        return None;
    }
    if native
        && input
            .get("action")
            .and_then(Value::as_str)
            .is_some_and(|action| action != "publish")
    {
        return None;
    }
    let title = input
        .get("title")
        .or_else(|| input.get("label"))
        .and_then(Value::as_str);
    let mut paths: Vec<String> = Vec::new();
    if let Some(path) = input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(Value::as_str)
    {
        paths.push(path.to_owned());
    }
    if !native && let Some(files) = input.get("files").and_then(Value::as_array) {
        paths.extend(
            files
                .iter()
                .filter_map(Value::as_str)
                .take(32)
                .map(str::to_owned),
        );
    }
    // A native publish's own words (RO-33) and what the library did not keep
    // of it (RO-34a): the library keeps a publish's page, so every other
    // member its files map names is counted as dropped.
    let description = native.then(|| request_description(input)).flatten();
    let mut dropped_files = native.then_some(0_u32);
    if native && let Some(files) = input.get("files").and_then(Value::as_object) {
        let root = input
            .get("root")
            .and_then(Value::as_str)
            .filter(|root| !root.trim().is_empty());
        // A `null` value removes a member on an update; it names nothing.
        let members: Vec<(&String, &Value)> = files.iter().filter(|(_, v)| !v.is_null()).collect();
        let named = u32::try_from(members.len()).unwrap_or(u32::MAX);
        if paths.is_empty() {
            // One self-contained page has one entrypoint. A multi-page bundle
            // cannot be reconstructed by guessing which HTML member is root.
            let pages: Vec<_> = members
                .iter()
                .filter(|(member, value)| {
                    is_html_path(member) || member_source(value).is_some_and(is_html_path)
                })
                .take(2)
                .collect();
            if let [(_, page)] = pages.as_slice()
                && let Some(source) = member_source(page)
            {
                paths.push(rooted(root, source));
                dropped_files = Some(named.saturating_sub(1));
            }
        } else {
            // `file_path` is the page; the map holds its supporting files.
            dropped_files = Some(named);
        }
    }
    let mut publications: Vec<HtmlArtifactPublication> = Vec::new();
    for path in &paths {
        if let Some(mut row) = publication(ctx, timeline_id, path, title, Some(cwd), at)
            && !publications.iter().any(|other| other.id == row.id)
        {
            row.description.clone_from(&description);
            row.dropped_files = dropped_files;
            publications.push(row);
        }
    }
    (!publications.is_empty()).then_some(PendingPublication {
        publications,
        native,
    })
}

/// A files-map member's local source. The Artifact tool documents the map as
/// `{"published/path": "source/path" | {from, contentType} | {artifact, path} |
/// null}`: a string value is a source path on disk, exactly as a typed `from`
/// is, never inline content (corrected 2026-11-05; normalizer 38 read a string
/// as the page's HTML). A `{artifact, path}` copy names another artifact's
/// file, never a local one.
fn member_source(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("from").and_then(Value::as_str))
}

/// Resolve a member's source against the request's `root`, the base its
/// relative sources resolve against; whatever stays relative then resolves
/// against the working directory, as `file_path` does.
fn rooted(root: Option<&str>, source: &str) -> String {
    let portable = source.replace('\\', "/");
    let absolute = portable.starts_with('/') || portable.as_bytes().get(1) == Some(&b':');
    match root {
        Some(root) if !absolute => format!("{}/{source}", root.trim_end_matches(['/', '\\'])),
        _ => source.to_owned(),
    }
}

/// The request's description, trimmed and clipped at a character boundary to
/// [`HTML_DESCRIPTION_MAX_BYTES`]. A NUL makes it unusable, so it is dropped.
fn request_description(input: &Value) -> Option<String> {
    let text = input.get("description")?.as_str()?.trim();
    if text.is_empty() || text.contains('\0') {
        return None;
    }
    let mut end = text.len().min(HTML_DESCRIPTION_MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let clipped = text[..end].trim_end();
    (!clipped.is_empty()).then(|| clipped.to_owned())
}

pub(crate) fn confirmed(
    ctx: &NormalizeCtx,
    pending: PendingPublication,
    result: Option<&Value>,
    failed: bool,
    at: Option<i64>,
) -> Vec<IngestOp> {
    if failed {
        return Vec::new();
    }
    let native_id = result
        .and_then(|r| r.get("artifact_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let url = result
        .and_then(|r| r.get("url"))
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://"));
    // A generic terminal result is not evidence that Artifact published.
    if pending.native && native_id.is_none() && url.is_none() {
        return Vec::new();
    }
    pending
        .publications
        .into_iter()
        .filter_map(|mut row| {
            row.source_offset = ctx
                .src_offset
                .and_then(|offset| offset.try_into().ok())
                .unwrap_or(0);
            row.published_at = at.or(row.published_at);
            row.native_id = native_id.map(str::to_owned);
            row.url = url.map(str::to_owned);
            row.version = result.and_then(|r| r.get("version")).and_then(|v| match v {
                Value::String(v) => Some(v.clone()),
                Value::Number(v) => Some(v.to_string()),
                _ => None,
            });
            if let Some(id) = native_id.or(url) {
                row.artifact_id = identity(&[&ctx.runtime_id, id]);
            }
            if let Some(title) = result
                .and_then(|r| r.get("title"))
                .and_then(Value::as_str)
                .filter(|title| !title.is_empty())
            {
                row.title = title.to_owned();
            }
            row.validate().ok()?;
            Some(IngestOp::HtmlArtifact(row))
        })
        .collect()
}
