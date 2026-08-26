//! RFC 3339 → unix milliseconds.
//!
//! Every rollout envelope stamps `timestamp` as an ISO-8601 UTC string
//! (`2026-08-25T02:35:03.577Z`; groundtruth.json, `rollout.timestamp`) and the
//! canonical model wants [`Ms`]. The workspace pulls in no date crate and
//! `rosetta-core` exposes no parser, so the helper lives in the adapter — see
//! the crate docs for the contract gap. Pure: no clock, no locale, no IO.

use rosetta_core::rows::Ms;

/// Parse an RFC 3339 timestamp to unix milliseconds.
///
/// Returns `None` rather than guessing: an unparseable stamp leaves `ts`
/// unknown, and the projection preserves unknown. A stamp with no zone marker
/// reads as UTC — codex always writes `Z`.
pub fn rfc3339_ms(text: &str) -> Option<Ms> {
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let (year, month, day) = triple(date, '-')?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (clock, offset_ms) = split_zone(rest)?;
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let (hour, minute, second) = triple(clock, ':')?;
    // Second 60 is a leap second: it lands in the same millisecond as 59.999.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(seconds * 1_000 + fraction_ms(fraction)? - offset_ms)
}

fn triple(text: &str, separator: char) -> Option<(i64, i64, i64)> {
    let mut parts = text.splitn(3, separator);
    Some((
        int(parts.next()?)?,
        int(parts.next()?)?,
        int(parts.next()?)?,
    ))
}

fn int(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Split the zone suffix off a time-of-day, returning its offset in ms.
fn split_zone(rest: &str) -> Option<(&str, i64)> {
    if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        return Some((clock, 0));
    }
    // Byte 0 is skipped: a sign there would leave an empty time-of-day.
    let sign_at = rest
        .bytes()
        .position(|byte| byte == b'+' || byte == b'-')
        .filter(|at| *at > 0);
    let Some(sign_at) = sign_at else {
        return Some((rest, 0));
    };
    let (clock, zone) = rest.split_at(sign_at);
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let (hours, minutes) = match zone[1..].split_once(':') {
        Some(parts) => parts,
        None if zone.len() == 5 => zone[1..].split_at(2),
        None => return None,
    };
    Some((
        clock,
        sign * (int(hours)? * 3_600 + int(minutes)? * 60) * 1_000,
    ))
}

/// Sub-second digits → milliseconds, truncating past the third digit.
fn fraction_ms(fraction: &str) -> Option<i64> {
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let digits = fraction.as_bytes();
    Some((0..3).fold(0, |millis, index| {
        millis * 10 + digits.get(index).map_or(0, |byte| i64::from(byte - b'0'))
    }))
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's civil
/// algorithm) — correct across the whole range, no lookup tables.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted = (month + 9) % 12;
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::rfc3339_ms;

    #[test]
    fn epoch_and_fixture_stamps() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(rfc3339_ms("1969-12-31T23:59:59.999Z"), Some(-1));
        // The first envelope of the committed basic-chat rollout.
        assert_eq!(rfc3339_ms("2026-08-25T02:35:03.577Z"), Some(1787625303577));
    }

    #[test]
    fn leap_years_and_fraction_widths() {
        assert_eq!(rfc3339_ms("2024-02-29T00:00:00Z"), Some(1709164800000));
        // 2000 is a leap year, 2100 is not.
        assert_eq!(rfc3339_ms("2100-03-01T00:00:00Z"), Some(4107542400000));
        assert_eq!(rfc3339_ms("2026-01-01T00:00:00.5Z"), Some(1767225600500));
        // Past millisecond precision truncates, never rounds.
        assert_eq!(
            rfc3339_ms("2026-01-01T00:00:00.123999Z"),
            Some(1767225600123)
        );
    }

    #[test]
    fn zone_offsets_shift_to_utc() {
        let utc = rfc3339_ms("2026-08-25T02:35:03Z");
        assert_eq!(rfc3339_ms("2026-08-25T04:35:03+02:00"), utc);
        assert_eq!(rfc3339_ms("2026-08-24T21:35:03-05:00"), utc);
        assert_eq!(rfc3339_ms("2026-08-25T04:35:03+0200"), utc);
        // A missing zone marker reads as UTC.
        assert_eq!(rfc3339_ms("2026-08-25T02:35:03"), utc);
    }

    #[test]
    fn malformed_stamps_stay_unknown() {
        for bad in [
            "",
            "2026-08-25",
            "2026-13-01T00:00:00Z",
            "2026-08-32T00:00:00Z",
            "2026-08-25T25:00:00Z",
            "2026-08-25T00:99:00Z",
            "20x6-08-25T00:00:00Z",
            "2026-08-25T00:00:0aZ",
            "2026-08-25T00:00:00.0a0Z",
        ] {
            assert_eq!(rfc3339_ms(bad), None, "{bad} should not parse");
        }
    }
}
