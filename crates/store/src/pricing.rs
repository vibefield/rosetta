//! The store's price function (docs/ARCHITECTURE.md §4.1.3). Every connection
//! the store opens registers `rosetta_list_cost`, so the live bucket rebuild
//! and the cold set-based fold price each usage record with the one compiled
//! catalog. A writer opening a complete projection priced with another catalog
//! re-folds the rollup from stored facts — never from the ledger.

use rosetta_core::{ServiceTier, TokenSnapshot, UsageCost, UsageScope};
use rosetta_pricing::{Request, list_cost, pico_to_micro, pricing_version, quote};
use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};

use crate::StoreError;
use crate::usage_sql::PRICE_FN;

/// Argument order: model, ts, scope, in, out, cache_r, cache_w, cache_w1h,
/// think, web_searches, service_tier, route, requests. NULL means unpriced.
const ARITY: i32 = 13;

pub(crate) fn register(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        PRICE_FN,
        ARITY,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(price(ctx)),
    )
}

fn text<'a>(ctx: &'a Context<'_>, index: usize) -> Option<&'a str> {
    match ctx.get_raw(index) {
        ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok(),
        _ => None,
    }
}

fn integer(ctx: &Context<'_>, index: usize) -> Result<i64, ()> {
    match ctx.get_raw(index) {
        ValueRef::Null => Ok(0),
        ValueRef::Integer(value) => Ok(value),
        _ => Err(()),
    }
}

/// A usage record's pricing instant. Usage ops carry `ts = 0` when the agent
/// recorded no time, and an undated row can inherit that 0: it means
/// unknown, never 1970-01-01, so a dated model stays `ambiguous_time`.
fn instant(at_ms: Option<i64>) -> Option<i64> {
    at_ms.filter(|at_ms| *at_ms > 0)
}

fn price(ctx: &Context<'_>) -> Option<i64> {
    let model = text(ctx, 0)?;
    let at_ms = instant(match ctx.get_raw(1) {
        ValueRef::Integer(value) => Some(value),
        _ => None,
    });
    let scope = match text(ctx, 2) {
        Some(value) => UsageScope::parse(value)?,
        None => UsageScope::Request,
    };
    let service_tier = match text(ctx, 10) {
        Some(value) => Some(ServiceTier::parse(value)?),
        None => None,
    };
    let request = Request {
        model,
        at_ms,
        scope,
        input: integer(ctx, 3).ok()?,
        output: integer(ctx, 4).ok()?,
        cache_read: integer(ctx, 5).ok()?,
        cache_write: integer(ctx, 6).ok()?,
        cache_write_1h: integer(ctx, 7).ok()?,
        thinking: integer(ctx, 8).ok()?,
        web_searches: integer(ctx, 9).ok()?,
        service_tier,
        route: text(ctx, 11),
        requests: integer(ctx, 12).ok()?,
    };
    list_cost(&request).ok()
}

/// The pricing version the projection's rollup costs were computed with.
pub fn stored_version(conn: &Connection) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT pricing_version FROM projection_state WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Re-price a complete projection whose rollup was computed with another
/// catalog or calculator. One transaction re-folds `usage_bucket` from the stored usage
/// facts and stamps the compiled version. Returns whether it re-priced.
pub(crate) fn ensure_current(conn: &mut Connection) -> Result<bool, StoreError> {
    if stored_version(conn)?.as_deref() == Some(pricing_version()) {
        return Ok(false);
    }
    let tx = conn.transaction()?;
    crate::bulk::refold_usage_buckets(&tx)?;
    tx.commit()?;
    Ok(true)
}

/// The read-time list-price cost of one row's usage record; `None` when the
/// row carries no usage. Rows are priced from the same stored facts the
/// rollup folds, so a transcript and its totals always agree.
pub fn usage_cost(
    model: Option<&str>,
    at_ms: Option<i64>,
    usage: &TokenSnapshot,
) -> Option<UsageCost> {
    if usage.total_tokens().is_none() && usage.web_searches.is_none() {
        return None;
    }
    let Some(model) = model else {
        return Some(UsageCost::Unpriced {
            reason: rosetta_pricing::Unpriced::UnknownModel.as_str().to_owned(),
        });
    };
    Some(
        match quote(&Request::from_usage(model, instant(at_ms), usage)) {
            Ok(quote) => UsageCost::Priced {
                micro_usd: pico_to_micro(quote.pico_usd),
                long_context: quote.long_context,
            },
            Err(unpriced) => UsageCost::Unpriced {
                reason: unpriced.as_str().to_owned(),
            },
        },
    )
}
