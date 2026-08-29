//! Timestamp helpers shared by native adapters.

use rosetta_core::rows::Ms;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Parse a standards-compliant RFC 3339 timestamp to Unix milliseconds.
/// Invalid or out-of-range native values remain unknown instead of guessed.
pub fn rfc3339_ms(text: &str) -> Option<Ms> {
    let nanos = OffsetDateTime::parse(text, &Rfc3339)
        .ok()?
        .unix_timestamp_nanos();
    i64::try_from(nanos.div_euclid(1_000_000)).ok()
}

#[cfg(test)]
mod tests {
    use super::rfc3339_ms;

    #[test]
    fn keeps_fractional_milliseconds_and_offsets() {
        assert_eq!(
            rfc3339_ms("2026-08-25T05:09:30.810488Z"),
            Some(1_787_634_570_810)
        );
        assert_eq!(
            rfc3339_ms("2026-08-24T22:09:30.810488-07:00"),
            Some(1_787_634_570_810)
        );
        assert_eq!(rfc3339_ms("not-a-time"), None);
    }
}
