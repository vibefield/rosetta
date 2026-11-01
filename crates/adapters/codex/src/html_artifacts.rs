//! Codex's explicit visualize presentation syntax, inside assistant final prose.

use rosetta_adapter_support::{
    NormalizeCtx,
    html_artifacts::{authored_text_ranges, presented_links, publication},
};
use rosetta_core::{IngestOp, rows::MessageRow};
use serde::Deserialize;

#[derive(Deserialize)]
struct Presentation {
    path: String,
    title: Option<String>,
}

pub(crate) fn presented(ctx: &NormalizeCtx, row: &MessageRow, cwd: Option<&str>) -> Vec<IngestOp> {
    let mut ops = presented_links(ctx, row, cwd);
    let Some(text) = row.content.as_deref() else {
        return ops;
    };
    let ranges = authored_text_ranges(text);
    const PREFIX: &str = "visualize";
    for (offset, _) in text.match_indices(PREFIX) {
        // The marker must start in authored prose. Read its JSON from the raw
        // text: Markdown punctuation inside a title/path is still JSON data.
        if !ranges
            .get(ranges.partition_point(|range| range.end <= offset))
            .is_some_and(|range| range.contains(&offset))
        {
            continue;
        }
        let Some((json, _)) = text[offset + PREFIX.len()..].split_once('') else {
            continue;
        };
        if json.len() <= 20_000
            && let Ok(presentation) = serde_json::from_str::<Presentation>(json)
            && let Some(publication) = publication(
                ctx,
                &row.timeline_id,
                &presentation.path,
                presentation.title.as_deref(),
                cwd,
                row.ts,
            )
            && !ops
                .iter()
                .any(|op| matches!(op, IngestOp::HtmlArtifact(other) if other.id == publication.id))
        {
            ops.push(IngestOp::HtmlArtifact(publication));
        }
        if ops.len() >= 32 {
            return ops;
        }
    }
    ops
}
