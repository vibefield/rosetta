use rosetta_adapter_codex::CodexAdapter;
use rosetta_adapter_support::{AgentAdapter, NormalizeCtx};
use rosetta_core::{IngestOp, html_artifacts::HtmlArtifactPublication};
use serde_json::json;

fn publications(role: &str, phase: Option<&str>, text: &str) -> Vec<HtmlArtifactPublication> {
    let mut ctx = NormalizeCtx::new("runtime", "session");
    ctx.state = CodexAdapter.init_state();
    ctx.src_offset = Some(123);
    let mut payload = json!({"type":"message","id":"reply","role":role,"content":[{"type":"output_text","text":text}]});
    if let Some(phase) = phase {
        payload["phase"] = json!(phase);
    }
    let outcome = CodexAdapter.ingest_line(
        &mut ctx,
        "rollout",
        &serde_json::to_vec(
            &json!({"type":"response_item","timestamp":"2026-10-01T12:00:00Z","payload":payload}),
        )
        .unwrap(),
    );
    outcome
        .ops
        .into_iter()
        .filter_map(|op| {
            if let IngestOp::HtmlArtifact(row) = op {
                Some(row)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn explicit_presentation_and_markdown_delivery_keep_title_filename_and_anchor() {
    let text = "visualize{\"path\":\"/tmp/review_design.html\",\"title\":\"Review options\",\"mode\":\"wide\"}\n\n[Second **design**](</tmp/second design.HTM>)";
    let rows = publications("assistant", Some("final_answer"), text);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows.iter()
            .any(|r| r.title == "Review options" && r.filename == "review_design.html")
    );
    assert!(
        rows.iter()
            .any(|r| r.title == "Second design"
                && r.path.as_deref() == Some("/tmp/second design.HTM"))
    );
    assert!(
        rows.iter()
            .all(|r| r.source_offset == 123 && !r.timeline_id.is_empty())
    );
    assert_eq!(rows, publications("assistant", Some("final_answer"), text));
}

#[test]
fn code_quotes_user_text_commentary_and_remote_pages_are_not_publications() {
    let marker = "visualize{\"path\":\"/tmp/design.html\"}";
    assert!(publications("user", None, marker).is_empty());
    assert!(publications("assistant", Some("commentary"), marker).is_empty());
    for text in [
        format!("`{marker}`"),
        format!("> {marker}"),
        format!("```json\n{marker}\n```"),
        "    [example](/tmp/example.html)".into(),
        "[Web](https://example.com/index.html)".into(),
        "![image](/tmp/screenshot.html)".into(),
        "[Code](/tmp/index.html:12)".into(),
        "I edited /tmp/index.html".into(),
        "[Remote](//remote/share.html)".into(),
        "[Remote](%2F%2Fremote/share.html)".into(),
        "visualize{\"path\":\"//remote/share.html\"}".into(),
    ] {
        assert!(publications("assistant", None, &text).is_empty(), "{text}");
    }
}

#[test]
fn repeated_links_deduplicate_and_local_uris_decode() {
    let rows = publications(
        "assistant",
        None,
        "[A](file:///tmp/my%20design.html) [Again](/tmp/my%20design.html) [No](file://remote/share.html)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].filename, "my design.html");
}

#[test]
fn presentation_json_preserves_markdown_punctuation_in_paths_and_titles() {
    let rows = publications(
        "assistant",
        Some("final_answer"),
        "visualize{\"path\":\"/tmp/design [v2].html\",\"title\":\"Review `code` and **bold**\"}",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].filename, "design [v2].html");
    assert_eq!(rows[0].title, "Review `code` and **bold**");
}
