//! Explicit local HTML delivery links. Callers decide which authored messages
//! are presentations; file writes, user prose and tool output never call this.

use std::collections::BTreeSet;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use rosetta_core::html_artifacts::{HtmlArtifactPublication, identity, is_html_path};
use rosetta_core::{IngestOp, rows::MessageRow, timeline::ResourceRef};

use crate::{NormalizeCtx, tool::lexical_resource};

/// Native recovery knowledge stays in the adapter. The capture layer owns IO;
/// hints are transient and never become canonical candidate tables.
#[derive(Debug, Default)]
pub struct CaptureHints {
    pub files: Vec<String>,
    pub roots: Vec<String>,
    pub urls: Vec<String>,
    pub record: Option<RecordedHtml>,
}

/// A successful source write in this same transcript. Only a coordinate and
/// checksum survive checkpoints; HTML payloads are never copied into them.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecordedHtml {
    pub offset: u64,
    pub digest: String,
}

pub fn publication(
    ctx: &NormalizeCtx,
    timeline_id: &str,
    path: &str,
    title: Option<&str>,
    cwd: Option<&str>,
    at: Option<i64>,
) -> Option<HtmlArtifactPublication> {
    if !is_html_path(path)
        || path.contains('\0')
        || path.starts_with("//")
        || path.starts_with("\\\\")
        || path.len() > 16384
    {
        return None;
    }
    let ResourceRef::File { path, .. } = lexical_resource(path, cwd, false).ok()? else {
        return None;
    };
    let filename = path.rsplit('/').next()?.to_owned();
    let title = title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(&filename)
        .to_owned();
    let row = HtmlArtifactPublication {
        id: identity(&[&ctx.session_id, timeline_id, &path]),
        artifact_id: identity(&[&ctx.session_id, &path]),
        session_id: ctx.session_id.clone(),
        timeline_id: timeline_id.to_owned(),
        title,
        filename,
        path: Some(path),
        url: None,
        native_id: None,
        version: None,
        published_at: at,
        source_offset: ctx.src_offset.and_then(|v| v.try_into().ok()).unwrap_or(0),
    };
    row.validate().ok()?;
    Some(row)
}

/// Local Markdown links are explicit delivery. References, emphasis in labels,
/// escaped paths and angle-delimited destinations follow CommonMark. Code,
/// quoted examples, images and remote URLs are not local artifact publication.
pub fn presented_links(ctx: &NormalizeCtx, row: &MessageRow, cwd: Option<&str>) -> Vec<IngestOp> {
    let Some(text) = row.content.as_deref() else {
        return Vec::new();
    };
    let mut excluded = 0_u32;
    let mut link: Option<(String, String)> = None;
    let mut paths = BTreeSet::new();
    let mut ops = Vec::new();
    for event in Parser::new(text) {
        match event {
            Event::Start(
                Tag::BlockQuote(_) | Tag::CodeBlock(_) | Tag::Image { .. } | Tag::HtmlBlock,
            ) => excluded += 1,
            Event::End(
                TagEnd::BlockQuote(_) | TagEnd::CodeBlock | TagEnd::Image | TagEnd::HtmlBlock,
            ) => excluded = excluded.saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) if excluded == 0 => {
                link = local_link(&dest_url).map(|path| (path, String::new()));
            }
            Event::Text(text) | Event::Code(text) if excluded == 0 => {
                if let Some((_, label)) = &mut link {
                    label.push_str(&text);
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some((path, label)) = link.take()
                    && paths.insert(path.clone())
                    && let Some(publication) =
                        publication(ctx, &row.timeline_id, &path, Some(&label), cwd, row.ts)
                {
                    ops.push(IngestOp::HtmlArtifact(publication));
                }
            }
            _ => {}
        }
        if ops.len() >= 32 {
            break;
        }
    }
    ops
}

fn local_link(destination: &str) -> Option<String> {
    // Strip only explicit local URI schemes. No network/UNC authorities and no
    // line-number anchors on source-code references masquerading as delivery.
    let path = if let Some(path) = destination.strip_prefix("file://") {
        if !path.starts_with('/') {
            return None;
        }
        path
    } else {
        destination.strip_prefix("sandbox:").unwrap_or(destination)
    };
    if path.contains(['?', '#'])
        || path.starts_with("//")
        || (path.contains(':') && path.as_bytes().get(1) != Some(&b':'))
    {
        return None;
    }
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    is_html_path(&decoded).then(|| decoded.into_owned())
}

/// Plain text ranges outside code/quotes/HTML, for an adapter's presentation
/// marker syntax. Offset ranges keep JSON whitespace and escaping intact.
pub fn authored_text_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut excluded = 0_u32;
    let mut ranges = Vec::new();
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(
                Tag::BlockQuote(_)
                | Tag::CodeBlock(_)
                | Tag::Image { .. }
                | Tag::HtmlBlock
                | Tag::Link { .. },
            ) => excluded += 1,
            Event::End(
                TagEnd::BlockQuote(_)
                | TagEnd::CodeBlock
                | TagEnd::Image
                | TagEnd::HtmlBlock
                | TagEnd::Link,
            ) => excluded = excluded.saturating_sub(1),
            Event::Text(_) if excluded == 0 => ranges.push(range),
            _ => {}
        }
    }
    ranges
}
