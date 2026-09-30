//! Agent-reported cumulative usage, independent of request-level accounting.

use serde::{Deserialize, Serialize};

use crate::Ms;

/// The latest reported totals for a session and its descendants. Replacement,
/// never an additive sample: the store compares these with recorded requests
/// and contributes only a positive difference to the aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUsageTotals {
    pub session_id: String,
    /// Attribution time, not a claim that every request happened at this time.
    pub ts: Ms,
    pub models: Vec<ModelUsageTotal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsageTotal {
    pub model: String,
    pub input: i64,
    /// Includes thinking, unlike TokenSnapshot::output.
    pub output: i64,
    pub thinking: Option<i64>,
    pub cache_read: i64,
    /// All cache writes; the cumulative source may not retain their TTLs.
    pub cache_write: i64,
    pub web_searches: i64,
    /// The agent's estimate, never provider-billed money.
    pub estimated_pico_usd: Option<i64>,
}
