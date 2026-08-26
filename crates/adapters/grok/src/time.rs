//! Grok timestamp helpers.
//!
//! `updates.timestamp` is unix seconds. The nested native metadata carries the
//! more precise `agentTimestampMs`, which normalization prefers when present.

use rosetta_core::rows::Ms;
use serde_json::Value;

pub fn seconds_ms(seconds: i64) -> Option<Ms> {
    seconds.checked_mul(1_000)
}

pub fn value_ms(value: &Value) -> Option<Ms> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

/// Rust's serialized `SystemTime` shape used by background-task snapshots.
pub fn system_time_ms(value: &Value) -> Option<Ms> {
    let seconds = value["secs_since_epoch"]
        .as_i64()
        .or_else(|| {
            value["secs_since_epoch"]
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
        })?
        .checked_mul(1_000)?;
    let millis = value["nanos_since_epoch"]
        .as_u64()
        .and_then(|value| i64::try_from(value / 1_000_000).ok())?;
    seconds.checked_add(millis)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{seconds_ms, system_time_ms, value_ms};

    #[test]
    fn grok_seconds_and_native_milliseconds() {
        assert_eq!(seconds_ms(1_787_634_571), Some(1_787_634_571_000));
        assert_eq!(
            value_ms(&json!(1_787_634_571_426u64)),
            Some(1_787_634_571_426)
        );
        assert_eq!(seconds_ms(i64::MAX), None);
    }

    #[test]
    fn task_system_time_keeps_millisecond_precision() {
        assert_eq!(
            system_time_ms(&json!({
                "secs_since_epoch": 1_787_634_571,
                "nanos_since_epoch": 426_999_000
            })),
            Some(1_787_634_571_426)
        );
    }
}
