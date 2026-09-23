//! The one usage-record projection both rollup folds read — the live
//! per-bucket rebuild and the cold set-based fold — so they can never
//! disagree about what a record contributes (ARCHITECTURE §4.1.3).

use std::sync::LazyLock;

/// The deterministic SQLite scalar that prices one usage record in pico-USD
/// (NULL = the catalog cannot price it). Registered on every connection the
/// store opens; see [`crate::pricing`].
pub(crate) const PRICE_FN: &str = "rosetta_list_cost";

/// A record carries usage when any bucket or the web-search count was
/// reported. Rows without usage (most message rows) never reach the pricer
/// and never count as unpriced.
const HAS_USAGE: &str = "COALESCE(in_tokens, out_tokens, cache_r_tokens, cache_w_tokens, \
     cache_w1h_tokens, think_tokens, web_searches) IS NOT NULL";

/// Per-record value columns shared by message and `session_usage` rows.
/// `cost_pusd` is evaluated once per record: SQLite never flattens a compound
/// (UNION ALL) subquery beneath an aggregate, so each row prices once.
fn record_columns(scope: &str) -> String {
    format!(
        "in_tokens, out_tokens, cache_r_tokens, cache_w_tokens, cache_w1h_tokens,
         think_tokens, web_searches, billed_pusd,
         CASE WHEN {HAS_USAGE} THEN {PRICE_FN}(model, ts, {scope}, in_tokens, out_tokens,
              cache_r_tokens, cache_w_tokens, cache_w1h_tokens, think_tokens, web_searches,
              service_tier, route, requests) END AS cost_pusd,
         CASE WHEN {HAS_USAGE} THEN COALESCE(in_tokens, 0) + COALESCE(out_tokens, 0)
              + COALESCE(cache_r_tokens, 0) + COALESCE(cache_w_tokens, 0)
              + COALESCE(cache_w1h_tokens, 0) + COALESCE(think_tokens, 0) END AS usage_tokens"
    )
}

/// The bucket aggregate over [`record_columns`], in `usage_bucket` column
/// order after the key: SUM keeps never-reported components NULL.
const BUCKET_SUMS: &str = "SUM(in_tokens), SUM(out_tokens), SUM(cache_r_tokens),
    SUM(cache_w_tokens), SUM(think_tokens), SUM(cache_w1h_tokens), SUM(web_searches),
    SUM(cost_pusd), SUM(billed_pusd),
    COALESCE(SUM(CASE WHEN cost_pusd IS NULL THEN usage_tokens ELSE 0 END), 0),
    SUM(message_count), SUM(tool_call_count)";

/// `usage_bucket` value columns, matching [`BUCKET_SUMS`].
pub(crate) const BUCKET_COLUMNS: &str = "in_tokens, out_tokens, cache_r_tokens, cache_w_tokens,
    think_tokens, cache_w1h_tokens, web_searches, cost_pusd, billed_pusd, unpriced_tokens,
    message_count, tool_call_count";

/// Hour floor matching Rust's `div_euclid` for pre-epoch values; SQLite
/// integer division otherwise truncates toward zero. `?1` is `HOUR_MS`.
fn hour_floor(ts: &str) -> String {
    format!("CASE WHEN {ts} >= 0 THEN ({ts} / ?1) * ?1 ELSE - ((-{ts} + ?1 - 1) / ?1) * ?1 END")
}

/// Recompute one bucket (`?1` hour length, `?2` session, `?3` hour start,
/// `?4` model key) from its source rows.
pub(crate) static REBUILD_ONE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT {BUCKET_SUMS}, COUNT(*)
         FROM (
           SELECT {message}, 1 AS message_count,
                  CASE WHEN type = 'tool_use' THEN 1 ELSE 0 END AS tool_call_count
           FROM message
           WHERE session_id = ?2
             AND COALESCE(ts, 0) >= ?3 AND COALESCE(ts, 0) < ?3 + ?1
             AND COALESCE(model, 'unknown') = ?4
           UNION ALL
           SELECT {sample}, 0 AS message_count, 0 AS tool_call_count
           FROM session_usage
           WHERE session_id = ?2
             AND ts >= ?3 AND ts < ?3 + ?1
             AND COALESCE(model, 'unknown') = ?4
         )",
        message = record_columns("usage_scope"),
        sample = record_columns("scope"),
    )
});

/// Replace every bucket in one set-based pass (`?1` hour length).
pub(crate) static FOLD_ALL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "INSERT INTO usage_bucket (session_id, hour_ts, model, {BUCKET_COLUMNS})
         SELECT session_id, hour_ts, model_key, {BUCKET_SUMS}
         FROM (
           SELECT session_id, {message_hour} AS hour_ts,
                  COALESCE(model, 'unknown') AS model_key, {message}, 1 AS message_count,
                  CASE WHEN type = 'tool_use' THEN 1 ELSE 0 END AS tool_call_count
           FROM message
           UNION ALL
           SELECT session_id, {sample_hour} AS hour_ts,
                  COALESCE(model, 'unknown') AS model_key, {sample}, 0 AS message_count,
                  0 AS tool_call_count
           FROM session_usage
         )
         GROUP BY session_id, hour_ts, model_key",
        message_hour = hour_floor("COALESCE(ts, 0)"),
        sample_hour = hour_floor("ts"),
        message = record_columns("usage_scope"),
        sample = record_columns("scope"),
    )
});
