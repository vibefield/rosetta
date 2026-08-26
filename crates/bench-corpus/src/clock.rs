//! Timestamp synthesis from a fixed base epoch.
//!
//! The system clock is never read: a corpus generated today and one generated
//! next year from the same seed must be byte-identical (CLAUDE.md iron rule 3).
//! Everything is UTC — the generator has no timezone database and needs none.

/// 2026-01-01T00:00:00Z in milliseconds since the Unix epoch.
pub const BASE_EPOCH_MS: i64 = 1_767_225_600_000;

pub const MS_PER_DAY: i64 = 86_400_000;

/// Broken-down UTC time: year, month, day, hour, minute, second, millisecond.
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub milli: u32,
}

/// Howard Hinnant's `civil_from_days`, valid across the whole proleptic
/// Gregorian calendar — cheaper and more honest than pulling in a date crate
/// for the four formats below.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

pub fn civil(ms: i64) -> Civil {
    let (year, month, day) = civil_from_days(ms.div_euclid(MS_PER_DAY));
    let rest = ms.rem_euclid(MS_PER_DAY);
    Civil {
        year,
        month,
        day,
        hour: (rest / 3_600_000) as u32,
        minute: (rest / 60_000 % 60) as u32,
        second: (rest / 1_000 % 60) as u32,
        milli: (rest % 1_000) as u32,
    }
}

/// `2026-01-01T00:00:00.000Z` — the form both agents stamp on records.
pub fn iso8601(ms: i64) -> String {
    let c = civil(ms);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.milli
    )
}

/// `2026-01-01` — codex's `current_date`.
pub fn date(ms: i64) -> String {
    let c = civil(ms);
    format!("{:04}-{:02}-{:02}", c.year, c.month, c.day)
}

/// `("2026", "01", "01")` — the `sessions/YYYY/MM/DD` path segments.
pub fn date_parts(ms: i64) -> (String, String, String) {
    let c = civil(ms);
    (
        format!("{:04}", c.year),
        format!("{:02}", c.month),
        format!("{:02}", c.day),
    )
}

/// `2026-01-01T00-00-00` — the colon-free stamp in a rollout filename.
pub fn file_stamp(ms: i64) -> String {
    let c = civil(ms);
    format!(
        "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

pub fn epoch_secs(ms: i64) -> i64 {
    ms.div_euclid(1_000)
}

/// Epoch seconds with the millisecond fraction — codex writes `create_time`
/// as a float.
pub fn epoch_secs_f64(ms: i64) -> f64 {
    ms as f64 / 1_000.0
}

#[cfg(test)]
mod tests {
    use super::{BASE_EPOCH_MS, MS_PER_DAY, date, date_parts, file_stamp, iso8601};

    #[test]
    fn base_epoch_is_new_year_2026() {
        assert_eq!(iso8601(BASE_EPOCH_MS), "2026-01-01T00:00:00.000Z");
        assert_eq!(date(BASE_EPOCH_MS), "2026-01-01");
        assert_eq!(file_stamp(BASE_EPOCH_MS), "2026-01-01T00-00-00");
        assert_eq!(
            date_parts(BASE_EPOCH_MS),
            ("2026".into(), "01".into(), "01".into())
        );
    }

    #[test]
    fn walks_months_and_leap_years() {
        // 2026 is not a leap year: day 59 is 1 March.
        assert_eq!(date(BASE_EPOCH_MS + 59 * MS_PER_DAY), "2026-03-01");
        // 2028 is: 1 Jan 2028 + 59 days is 29 February.
        let y2028 = BASE_EPOCH_MS + 730 * MS_PER_DAY;
        assert_eq!(date(y2028), "2028-01-01");
        assert_eq!(date(y2028 + 59 * MS_PER_DAY), "2028-02-29");
    }

    #[test]
    fn renders_time_of_day() {
        let ms = BASE_EPOCH_MS + 13 * 3_600_000 + 7 * 60_000 + 42_000 + 375;
        assert_eq!(iso8601(ms), "2026-01-01T13:07:42.375Z");
        assert_eq!(file_stamp(ms), "2026-01-01T13-07-42");
    }
}
