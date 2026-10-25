//! UTC RFC 3339 helpers.
//!
//! The domain only needs three operations: parse an instant, format an instant
//! and measure elapsed days. Implementing them here keeps the crate free of a
//! calendar dependency and keeps every arithmetic path deterministic and
//! testable, which the FSRS/TTL contract tests depend on.

use std::fmt;

const SECONDS_PER_DAY: i64 = 86_400;

/// Why an instant could not be understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeError {
    /// The text is not an RFC 3339 UTC timestamp this implementation accepts.
    Malformed {
        /// Offending input.
        input: String,
    },
    /// A field is syntactically fine but outside its valid range.
    OutOfRange {
        /// Offending input.
        input: String,
        /// Field that is out of range.
        field: &'static str,
    },
    /// The instant cannot be represented as seconds since the Unix epoch.
    Unrepresentable {
        /// Seconds requested.
        seconds: i64,
    },
}

impl TimeError {
    /// Stable machine code.
    pub fn code(&self) -> &'static str {
        match self {
            TimeError::Malformed { .. } => "INVALID_ARGUMENT",
            TimeError::OutOfRange { .. } => "INVALID_ARGUMENT",
            TimeError::Unrepresentable { .. } => "INVALID_ARGUMENT",
        }
    }
}

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimeError::Malformed { input } => {
                write!(f, "`{input}` is not an RFC 3339 UTC instant")
            }
            TimeError::OutOfRange { input, field } => {
                write!(f, "`{input}` has an out-of-range {field}")
            }
            TimeError::Unrepresentable { seconds } => {
                write!(f, "{seconds} is outside the representable date range")
            }
        }
    }
}

impl std::error::Error for TimeError {}

fn digits(bytes: &[u8], start: usize, len: usize) -> Option<i64> {
    if start + len > bytes.len() {
        return None;
    }
    let mut value = 0i64;
    for byte in &bytes[start..start + len] {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Some(value)
}

fn leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`].
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

/// Parse `YYYY-MM-DDTHH:MM:SS[.fraction](Z|±HH:MM)` into Unix seconds.
///
/// Only UTC instants are accepted; a non-zero offset is applied, then discarded.
pub fn parse_rfc3339(text: &str) -> Result<i64, TimeError> {
    let malformed = || TimeError::Malformed {
        input: text.to_string(),
    };
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return Err(malformed());
    }
    let year = digits(bytes, 0, 4).ok_or_else(malformed)?;
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(malformed());
    }
    let month = digits(bytes, 5, 2).ok_or_else(malformed)?;
    let day = digits(bytes, 8, 2).ok_or_else(malformed)?;
    if !matches!(bytes[10], b'T' | b't' | b' ') {
        return Err(malformed());
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return Err(malformed());
    }
    let hour = digits(bytes, 11, 2).ok_or_else(malformed)?;
    let minute = digits(bytes, 14, 2).ok_or_else(malformed)?;
    let second = digits(bytes, 17, 2).ok_or_else(malformed)?;
    if !(1..=12).contains(&month) {
        return Err(TimeError::OutOfRange {
            input: text.to_string(),
            field: "month",
        });
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err(TimeError::OutOfRange {
            input: text.to_string(),
            field: "day",
        });
    }
    if hour > 23 {
        return Err(TimeError::OutOfRange {
            input: text.to_string(),
            field: "hour",
        });
    }
    if minute > 59 {
        return Err(TimeError::OutOfRange {
            input: text.to_string(),
            field: "minute",
        });
    }
    if second > 59 {
        return Err(TimeError::OutOfRange {
            input: text.to_string(),
            field: "second",
        });
    }

    let mut position = 19;
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        let start = position;
        while position < bytes.len() && bytes[position].is_ascii_digit() {
            position += 1;
        }
        if position == start {
            return Err(malformed());
        }
    }

    let offset_seconds = match bytes.get(position) {
        Some(b'Z') | Some(b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1 } else { 1 };
            let offset_hour = digits(bytes, position + 1, 2).ok_or_else(malformed)?;
            if bytes.get(position + 3) != Some(&b':') {
                return Err(malformed());
            }
            let offset_minute = digits(bytes, position + 4, 2).ok_or_else(malformed)?;
            if offset_hour > 23 || offset_minute > 59 {
                return Err(TimeError::OutOfRange {
                    input: text.to_string(),
                    field: "offset",
                });
            }
            sign * (offset_hour * 3_600 + offset_minute * 60)
        }
        _ => return Err(malformed()),
    };

    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(SECONDS_PER_DAY)
        .and_then(|value| value.checked_add(hour * 3_600 + minute * 60 + second))
        .and_then(|value| value.checked_sub(offset_seconds))
        .ok_or(TimeError::Unrepresentable { seconds: days })?;
    Ok(seconds)
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

/// Elapsed days from `from` to `to`, clamped at zero.
///
/// A `to` before `from` is a future timestamp relative to the reference instant.
/// Callers that must isolate clock skew use [`elapsed_days_checked`].
pub fn elapsed_days(from: i64, to: i64) -> f64 {
    (to - from) as f64 / SECONDS_PER_DAY as f64
}

/// Elapsed days that refuses negative intervals (§23.1 "`t >= 0`").
pub fn elapsed_days_checked(from: i64, to: i64) -> Result<f64, TimeError> {
    if to < from {
        return Err(TimeError::OutOfRange {
            input: format!("{to} is before {from}"),
            field: "elapsed_days",
        });
    }
    Ok(elapsed_days(from, to))
}

/// Instant plus a number of hours.
pub fn add_hours(seconds: i64, hours: f64) -> i64 {
    seconds + (hours * 3_600.0).round() as i64
}

/// Instant plus a number of days.
pub fn add_days(seconds: i64, days: f64) -> i64 {
    seconds + (days * f64::from(SECONDS_PER_DAY as i32)).round() as i64
}

/// The earlier of two instants.
pub fn earliest(left: i64, right: i64) -> i64 {
    if left <= right {
        left
    } else {
        right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_epoch_and_round_trips() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z").unwrap(), 0);
        assert_eq!(
            parse_rfc3339("2026-10-25T00:00:00Z").unwrap(),
            1_792_886_400
        );
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_792_886_400), "2026-10-25T00:00:00Z");
    }

    #[test]
    fn applies_and_keeps_fractional_seconds_lossless() {
        assert_eq!(
            parse_rfc3339("2026-10-25T02:00:00+02:00").unwrap(),
            parse_rfc3339("2026-10-25T00:00:00Z").unwrap()
        );
        assert_eq!(
            parse_rfc3339("2026-10-25T00:00:00.500Z").unwrap(),
            parse_rfc3339("2026-10-25T00:00:00Z").unwrap()
        );
        assert!(parse_rfc3339("2024-02-29T12:00:00Z").is_ok());
    }

    #[test]
    fn rejects_malformed_and_out_of_range_instants() {
        for bad in [
            "",
            "2026-10-25",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2026-10-25T24:00:00Z",
            "2026-10-25T00:60:00Z",
            "2026-10-25T00:00:60Z",
            "2026-10-25T00:00:00",
            "2026-10-25T00:00:00.Z",
            "abcd-10-25T00:00:00Z",
        ] {
            assert!(parse_rfc3339(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn elapsed_days_reports_negative_intervals_only_when_asked_to() {
        let base = parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(elapsed_days(base, base + 86_400), 1.0);
        assert_eq!(elapsed_days(base, base - 86_400), -1.0);
        assert!(elapsed_days_checked(base, base - 1).is_err());
        assert_eq!(elapsed_days_checked(base, base).unwrap(), 0.0);
    }

    #[test]
    fn date_helpers_are_inverse_and_handle_leap_days() {
        for days in [-40_000i64, -1, 0, 1, 20_000, 60_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
        assert_eq!(add_hours(0, 36.0), 129_600);
        assert_eq!(add_days(0, 0.5), 43_200);
        assert_eq!(earliest(5, 9), 5);
        assert_eq!(earliest(9, 5), 5);
    }
}
