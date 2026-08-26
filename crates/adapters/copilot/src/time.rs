//! RFC 3339 timestamps to unix milliseconds, kept adapter-local until the
//! shared canonical layer owns a timestamp parser.

use rosetta_core::rows::Ms;

pub fn rfc3339_ms(text: &str) -> Option<Ms> {
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let (year, month, day) = triple(date, '-')?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (clock, offset_ms) = split_zone(rest)?;
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let (hour, minute, second) = triple(clock, ':')?;
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

fn split_zone(rest: &str) -> Option<(&str, i64)> {
    if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        return Some((clock, 0));
    }
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

fn fraction_ms(fraction: &str) -> Option<i64> {
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let digits = fraction.as_bytes();
    Some((0..3).fold(0, |millis, index| {
        millis * 10 + digits.get(index).map_or(0, |byte| i64::from(byte - b'0'))
    }))
}

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
    fn parses_fixture_and_offset_stamps() {
        assert_eq!(
            rfc3339_ms("2026-08-25T05:27:58.248Z"),
            Some(1_787_635_678_248)
        );
        assert_eq!(
            rfc3339_ms("2026-08-25T07:27:58.248+02:00"),
            Some(1_787_635_678_248)
        );
        assert_eq!(rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn malformed_stamps_remain_unknown() {
        for value in [
            "",
            "2026-08-25",
            "2026-13-01T00:00:00Z",
            "2026-08-25T25:00:00Z",
            "2026-08-25T00:00:xxZ",
        ] {
            assert_eq!(rfc3339_ms(value), None, "{value} should not parse");
        }
    }
}
