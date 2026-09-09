//! Structured WebSearch/WebFetch results from the pinned Claude tool-result
//! facets (observed-2026-09-03.json). Never mine arbitrary result prose for URLs.
use rosetta_adapter_support::tool::url;
use rosetta_core::timeline::{
    CORE_SCALAR_MAX_BYTES, FetchOutcome, ToolAction, WEB_SOURCE_LIMIT, WebSearchOutcome, WebSource,
};
use serde_json::Value;

pub(crate) fn project_result(request: &ToolAction, result: &Value) -> Option<ToolAction> {
    let mut action = request.clone();
    match &mut action {
        ToolAction::WebSearch { outcome, .. } => {
            let groups = result.get("results")?.as_array()?;
            let mut recognized = groups.is_empty();
            let mut sources = Vec::new();
            let mut total_count = 0;
            for group in groups {
                let Some(content) = group.get("content").and_then(Value::as_array) else {
                    continue;
                };
                recognized |= content.is_empty();
                for item in content {
                    let Some(target) = item.get("url").and_then(Value::as_str) else {
                        continue;
                    };
                    if target.trim().is_empty() {
                        continue;
                    }
                    recognized = true;
                    total_count += 1;
                    // A source beyond the canonical address bound still counts;
                    // shortening its address would change the destination.
                    if target.len() > CORE_SCALAR_MAX_BYTES || url(target).is_err() {
                        continue;
                    }
                    if sources.len() < WEB_SOURCE_LIMIT {
                        sources.push(WebSource {
                            title: item
                                .get("title")
                                .and_then(Value::as_str)
                                .map(bounded_title)
                                .filter(|title| !title.is_empty()),
                            url: target.to_owned(),
                        });
                    }
                }
            }
            if !recognized {
                return None;
            }
            *outcome = Some(WebSearchOutcome {
                sources,
                total_count,
            });
        }
        ToolAction::WebFetch { outcome, .. } => {
            let code = result.get("code")?.as_u64()?;
            if !(100..=599).contains(&code) {
                return None;
            }
            let target = result.get("url")?.as_str()?;
            if target.len() > CORE_SCALAR_MAX_BYTES || url(target).is_err() {
                return None;
            }
            *outcome = Some(FetchOutcome {
                final_url: Some(target.to_owned()),
                title: None, // `result` is extracted prose; `codeText` is the HTTP status phrase.
                http_status: Some(code as u16),
            });
        }
        _ => return None,
    }
    action.validate().ok()?;
    Some(action)
}

fn bounded_title(value: &str) -> String {
    let value = value.trim();
    let mut end = value.len().min(CORE_SCALAR_MAX_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bounds_keep_source_totals_and_unicode_titles_without_claiming_malformed_results_are_empty() {
        let request = ToolAction::WebSearch {
            query: Some("q".into()),
            allowed_domains: vec![],
            blocked_domains: vec![],
            outcome: None,
        };
        let result = json!({"results":[{"content":[
            {"url":format!("https://example.org/{}", "x".repeat(CORE_SCALAR_MAX_BYTES)), "title":"long address"},
            {"url":"https://example.org/ok", "title":"é".repeat(CORE_SCALAR_MAX_BYTES)}
        ]}]});
        let action = project_result(&request, &result).unwrap();
        action.validate().unwrap();
        let ToolAction::WebSearch {
            outcome: Some(outcome),
            ..
        } = action
        else {
            panic!("missing outcome")
        };
        assert_eq!(outcome.total_count, 2);
        assert_eq!(outcome.sources.len(), 1);
        let title = outcome.sources[0].title.as_deref().unwrap();
        assert!(title.len() <= CORE_SCALAR_MAX_BYTES);
        assert!(title.chars().all(|ch| ch == 'é'));
        assert!(
            project_result(
                &request,
                &json!({"results":[{"content":[42,{"title":"no address"}]}]})
            )
            .is_none()
        );
    }
}
