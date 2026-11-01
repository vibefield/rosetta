//! Confirmed native publications. Non-publishing Artifact actions never enter
//! the review library, even when a list/read response happens to contain paths.

use rosetta_adapter_support::{NormalizeCtx, html_artifacts::publication};
use rosetta_core::{
    IngestOp, ProjectionAsset,
    html_artifacts::{
        HTML_MAX_BYTES, HTML_MEDIA_TYPE, HtmlArtifactAvailability, HtmlArtifactContent,
        HtmlArtifactPublication, identity, is_html_path,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingPublication {
    publications: Vec<HtmlArtifactPublication>,
    native: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inline_html: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inline_availability: Option<HtmlArtifactAvailability>,
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
    let mut paths = Vec::new();
    let mut inline_html = None;
    let mut inline_availability = None;
    let mut publications = Vec::new();
    if let Some(path) = input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(Value::as_str)
    {
        paths.push(path);
    }
    if !native && let Some(files) = input.get("files").and_then(Value::as_array) {
        paths.extend(files.iter().filter_map(Value::as_str).take(32));
    }
    // In the files-map form only a typed `from` is a source path. String
    // values are content, never filenames to probe on the host filesystem.
    if native
        && paths.is_empty()
        && let Some(files) = input.get("files").and_then(Value::as_object)
    {
        let sources: Vec<_> = files
            .iter()
            .filter(|(name, value)| {
                is_html_path(name)
                    || value
                        .get("from")
                        .and_then(Value::as_str)
                        .is_some_and(is_html_path)
            })
            .take(2)
            .collect();
        // One self-contained page has one entrypoint. A multi-page bundle
        // cannot be reconstructed by guessing which HTML dependency is root.
        if sources.len() == 1 {
            let (member, value) = sources[0];
            if let Some(path) = value.get("from").and_then(Value::as_str) {
                paths.push(path);
            } else if let Some(html) = value.as_str() {
                let filename = member.rsplit('/').next()?.to_owned();
                publications.push(HtmlArtifactPublication {
                    id: identity(&[&ctx.session_id, timeline_id, "inline", member]),
                    artifact_id: identity(&[&ctx.session_id, "inline", member]),
                    session_id: ctx.session_id.clone(),
                    timeline_id: timeline_id.to_owned(),
                    title: title
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or(&filename)
                        .to_owned(),
                    filename,
                    path: None,
                    url: None,
                    native_id: None,
                    version: None,
                    published_at: at,
                    source_offset: ctx.src_offset.and_then(|v| v.try_into().ok()).unwrap_or(0),
                });
                let availability = if html.len() > HTML_MAX_BYTES {
                    HtmlArtifactAvailability::TooLarge
                } else if html.contains('\0') {
                    HtmlArtifactAvailability::Unsupported
                } else {
                    inline_html = Some(html.to_owned());
                    HtmlArtifactAvailability::Captured
                };
                inline_availability = Some(availability);
            }
        }
    }
    for path in paths {
        if let Some(row) = publication(ctx, timeline_id, path, title, Some(cwd), at)
            && !publications
                .iter()
                .any(|other: &HtmlArtifactPublication| other.id == row.id)
        {
            publications.push(row);
        }
    }
    (!publications.is_empty()).then_some(PendingPublication {
        publications,
        native,
        inline_html,
        inline_availability,
    })
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
    let mut ops: Vec<_> = pending
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
        .collect();
    if let Some(availability) = pending.inline_availability
        && let Some(IngestOp::HtmlArtifact(row)) = ops.first()
    {
        let asset = pending
            .inline_html
            .and_then(|html| ProjectionAsset::new(HTML_MEDIA_TYPE, html.into_bytes()));
        let content = HtmlArtifactContent {
            session_id: row.session_id.clone(),
            source_offset: row.source_offset,
            path: None,
            availability,
            captured_at: None,
            recovered_from: None,
            asset: asset.as_ref().map(ProjectionAsset::reference),
        };
        if let Some(asset) = asset {
            ops.push(IngestOp::ProjectionAsset { asset });
        }
        ops.push(IngestOp::HtmlArtifactContent(content));
    }
    ops
}
