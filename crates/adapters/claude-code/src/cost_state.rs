use rosetta_core::{ModelUsageTotal, SessionUsageTotals};

use crate::raw::cost_state::CostStateLine;

/// No pricing here: these amounts come from the CLI's local cost ledger.
/// Cache TTL and request timing have already been lost in that ledger, so it
/// must not masquerade as one more request or as provider-billed money.
pub(crate) fn totals(
    line: &CostStateLine,
    session_id: &str,
    ts: i64,
) -> Option<SessionUsageTotals> {
    let models = line
        .model_usage
        .iter()
        .map(|(model, usage)| {
            let input = i64::try_from(usage.input_tokens).ok()?;
            let output = i64::try_from(usage.output_tokens).ok()?;
            let cache_read = i64::try_from(usage.cache_read_input_tokens).ok()?;
            let cache_write = i64::try_from(usage.cache_creation_input_tokens).ok()?;
            input
                .checked_add(output)?
                .checked_add(cache_read)?
                .checked_add(cache_write)?;
            let thinking = usage.thinking_tokens.map(i64::try_from).transpose().ok()?;
            if thinking.is_some_and(|thinking| thinking > output) {
                return None;
            }
            Some(ModelUsageTotal {
                model: model.clone(),
                input,
                output,
                thinking,
                cache_read,
                cache_write,
                web_searches: i64::try_from(usage.web_search_requests).ok()?,
                estimated_pico_usd: if line.has_unknown_model_cost && usage.cost_usd == 0.0 {
                    None
                } else {
                    Some(pico_usd(usage.cost_usd)?)
                },
            })
        })
        .collect::<Option<Vec<_>>>()?;
    // Reject an invalid amount even when the model map is empty.
    pico_usd(line.total_cost_usd)?;
    Some(SessionUsageTotals {
        session_id: session_id.to_owned(),
        ts,
        models,
    })
}

fn pico_usd(usd: f64) -> Option<i64> {
    let pico = (usd * 1_000_000_000_000.0).round();
    (usd >= 0.0 && pico.is_finite() && pico < i64::MAX as f64).then_some(pico as i64)
}
