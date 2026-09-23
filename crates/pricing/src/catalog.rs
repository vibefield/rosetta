//! The static price catalog's shape. `generated.rs` (written by
//! `tools/pricing`, never by hand) instantiates these types; every rate is an
//! exact integer, so pricing never rounds until a service-tier multiplier.

use rosetta_core::ServiceTier;

/// Pico-USD (10⁻¹² USD). Catalog rates are pico-USD per token, exact for any
/// price with at most six decimals per million tokens.
pub type PicoUsd = i64;

/// One model's dated price history under its creator's first-party API.
#[derive(Debug)]
pub struct Model {
    /// genai-prices provider id (`anthropic`, `openai`, `x-ai`, …).
    pub provider: &'static str,
    /// Canonical catalog id (genai-prices model id).
    pub id: &'static str,
    pub name: &'static str,
    /// genai-prices semantics: tried last to first, the first whose constraint
    /// holds wins, and the first period applies when none holds.
    pub periods: &'static [Period],
}

#[derive(Debug)]
pub struct Period {
    pub constraint: Constraint,
    pub prices: Prices,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Constraint {
    Always,
    /// Active from this UTC day (days since 1970-01-01) onward.
    FromDay(i32),
    /// Active during a daily UTC window `[start, end)`, in seconds of the
    /// day; `end < start` wraps past midnight (off-peak pricing).
    TimeOfDay {
        start_s: u32,
        end_s: u32,
    },
}

/// Rates for the units Rosetta records. A missing child rate falls back to its
/// nearest priced ancestor, exactly as genai-prices decomposes usage:
/// 1-hour writes → cache writes → input; cache reads → input; reasoning →
/// output.
#[derive(Debug, Default)]
pub struct Prices {
    pub input: Option<Rate>,
    pub output: Option<Rate>,
    pub cache_read: Option<Rate>,
    pub cache_write: Option<Rate>,
    pub cache_write_1h: Option<Rate>,
    pub output_reasoning: Option<Rate>,
    /// Per server-side web search call.
    pub web_search_call: Option<PicoUsd>,
    /// Per request.
    pub request: Option<PicoUsd>,
}

/// A per-token rate with long-context tiers. A tier applies to the whole
/// request when its prompt size is strictly greater than `start`.
#[derive(Debug, Clone, Copy)]
pub struct Rate {
    pub base: PicoUsd,
    pub tiers: &'static [TierStep],
}

#[derive(Debug, Clone, Copy)]
pub struct TierStep {
    pub start: i64,
    pub rate: PicoUsd,
}

/// A cited service-tier price rule: the tier's cost is the standard cost times
/// `numerator / denominator`. `model = None` applies to every model of the
/// provider that has no model-specific rule.
#[derive(Debug)]
pub struct TierRule {
    pub provider: &'static str,
    pub model: Option<&'static str>,
    pub tier: ServiceTier,
    pub numerator: i64,
    pub denominator: i64,
    pub source: &'static str,
}

/// A cited residency/regional premium for a qualified route such as
/// `anthropic/us`, applied as a multiplier from `from_day` onward.
#[derive(Debug)]
pub struct RouteRule {
    pub route: &'static str,
    pub provider: &'static str,
    pub models: &'static [&'static str],
    pub numerator: i64,
    pub denominator: i64,
    pub source: &'static str,
}

/// Where the catalog's data came from, for `/api/pricing` and audits.
#[derive(Debug)]
pub struct Source {
    pub name: &'static str,
    pub url: &'static str,
    pub revision: &'static str,
    pub sha256: &'static str,
    pub license: &'static str,
}
