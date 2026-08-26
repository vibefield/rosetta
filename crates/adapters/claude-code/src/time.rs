//! RFC 3339 → unix milliseconds.
//!
//! Claude Code stamps every timeline record with an ISO-8601 UTC string
//! (`2026-08-25T02:35:19.072Z`), and the canonical model wants `Ms`. This lives
//! in the adapter because the workspace pulls in no date crate and `rosetta-core`
//! exposes no parser yet — see the crate docs for the contract gap. Pure by
//! construction: no clock, no locale, no IO.

use rosetta_core::rows::Ms;

/// Parse an RFC 3339 timestamp. Returns `None` rather than guessing — an
/// unparseable stamp leaves `ts` unknown, which the projection preserves.
/// A stamp with no zone marker is read as UTC (Claude Code always writes `Z`).
pub fn rfc3339_ms(text: &str) -> Option<Ms> {
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let mut parts = date.splitn(3, '-');
    let year: i64 = parse_int(parts.next()?)?;
    let month: i64 = parse_int(parts.next()?)?;
    let day: i64 = parse_int(parts.next()?)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (time, offset_ms) = split_offset(rest)?;
    let mut parts = time.splitn(3, ':');
    let hour: i64 = parse_int(parts.next()?)?;
    let minute: i64 = parse_int(parts.next()?)?;
    let seconds = parts.next()?;
    let (second, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let second: i64 = parse_int(second)?;
    let millis = fraction_millis(fraction)?;
    // Second 60 is a leap second; it lands on the same millisecond as 59.999.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000 + millis - offset_ms)
}

fn parse_int(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Split the zone suffix off a time-of-day, returning the offset in ms.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(time) = rest.strip_suffix(['Z', 'z']) {
        return Some((time, 0));
    }
    // Skip byte 0: a sign there would mean an empty time-of-day.
    let Some(sign_at) = rest
        .bytes()
        .position(|b| b == b'+' || b == b'-')
        .filter(|at| *at > 0)
    else {
        return Some((rest, 0));
    };
    let (time, zone) = rest.split_at(sign_at);
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let (hours, minutes) = match zone[1..].split_once(':') {
        Some(split) => split,
        None if zone.len() == 5 => zone[1..].split_at(2),
        None => return None,
    };
    let hours = parse_int(hours)?;
    let minutes = parse_int(minutes)?;
    Some((time, sign * (hours * 3_600 + minutes * 60) * 1_000))
}

/// Sub-second digits → milliseconds, truncating beyond the third digit.
fn fraction_millis(fraction: &str) -> Option<i64> {
    if !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut millis = 0i64;
    for index in 0..3 {
        let digit = fraction
            .as_bytes()
            .get(index)
            .map_or(0, |b| i64::from(b - b'0'));
        millis = millis * 10 + digit;
    }
    Some(millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's civil
/// algorithm). Correct for the whole range, no lookup tables.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_shifted = (month + 9) % 12;
    let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
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
        // The stamp on the committed tool-use fixture.
        assert_eq!(rfc3339_ms("2026-08-25T02:35:19.072Z"), Some(1787625319072));
    }

    #[test]
    fn leap_years_and_fraction_widths() {
        assert_eq!(rfc3339_ms("2024-02-29T00:00:00Z"), Some(1709164800000));
        // 2100 is not a leap year; 2000 is.
        assert_eq!(rfc3339_ms("2100-03-01T00:00:00Z"), Some(4107542400000));
        assert_eq!(rfc3339_ms("2026-01-01T00:00:00.5Z"), Some(1767225600500));
        // Beyond millisecond precision truncates, never rounds.
        assert_eq!(
            rfc3339_ms("2026-01-01T00:00:00.123999Z"),
            Some(1767225600123)
        );
    }

    #[test]
    fn zone_offsets_shift_to_utc() {
        let utc = rfc3339_ms("2026-08-25T02:35:19Z");
        assert_eq!(rfc3339_ms("2026-08-25T04:35:19+02:00"), utc);
        assert_eq!(rfc3339_ms("2026-08-24T21:35:19-05:00"), utc);
        assert_eq!(rfc3339_ms("2026-08-25T04:35:19+0200"), utc);
        // No zone marker is read as UTC.
        assert_eq!(rfc3339_ms("2026-08-25T02:35:19"), utc);
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
