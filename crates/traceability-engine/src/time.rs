//! RFC 3339 UTC formatting.
//!
//! Trace objects only need to *emit* instants (the clock supplies them and the
//! snapshots compare them as opaque revisions), so this module is format-only.
//! `memory-domain` carries the matching parser; hoisting both into `ukma-types`
//! is recorded as an ADR-053 gap rather than duplicating a parser here.

const SECONDS_PER_DAY: i64 = 86_400;

/// Inverse of the civil-date algorithm: days since the epoch to `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_rfc3339(seconds: i64) -> String {
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let remainder = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    let minute = (remainder % 3_600) / 60;
    let second = remainder % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_792_886_400), "2026-10-25T00:00:00Z");
        assert_eq!(format_rfc3339(-86_400), "1969-12-31T00:00:00Z");
        assert_eq!(format_rfc3339(3_600), "1970-01-01T01:00:00Z");
    }
}
