//! Duration parsing and UTC timestamp formatting, with no dependency.
//!
//! `chrono`/`time` would be a new dependency for two small, exactly-specifiable
//! jobs: turning `8h` into a `Duration`, and turning a Unix timestamp into the
//! `20260713-141530` label that names a run directory. Both are pure functions
//! with unit tests, which is cheaper than a crate and (unlike a crate) cannot
//! surprise us at build time.

use std::time::Duration;

/// Parse a human duration: a number with an optional `s` / `m` / `h` / `d`
/// suffix. A bare number is seconds. `30m`, `8h`, `90s`, `1d`, `120` all parse.
///
/// # Errors
///
/// Returns a human-readable `String` for an empty input, a bad number, an
/// unknown suffix, or a non-positive duration.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".to_string());
    }
    let (value, unit_secs) = match s.chars().last() {
        Some('s' | 'S') => (&s[..s.len() - 1], 1.0),
        Some('m' | 'M') => (&s[..s.len() - 1], 60.0),
        Some('h' | 'H') => (&s[..s.len() - 1], 3600.0),
        Some('d' | 'D') => (&s[..s.len() - 1], 86400.0),
        _ => (s, 1.0),
    };
    let n: f64 = value.trim().parse().map_err(|_| {
        format!("bad duration {s:?} (expected e.g. 90s, 30m, 8h, 1d, or a number of seconds)")
    })?;
    if !n.is_finite() || n <= 0.0 {
        return Err(format!("duration {s:?} must be greater than zero"));
    }
    Ok(Duration::from_secs_f64(n * unit_secs))
}

/// Render a duration the way the report prints it: `8h 00m 00s`.
#[must_use]
pub fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    format!("{h}h {m:02}m {s:02}s")
}

/// Format a Unix timestamp (seconds) as a compact UTC label, `YYYYMMDD-HHMMSS`
/// — the default name of a run directory, sortable and unambiguous.
///
/// Civil-from-days per Howard Hinnant's `civil_from_days`, which is exact for
/// every date in the proleptic Gregorian calendar and needs no leap table.
#[must_use]
pub fn format_utc_label(unix_secs: u64) -> String {
    let days = i64::try_from(unix_secs / 86_400).unwrap_or(0);
    let secs_of_day = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

/// Seconds since the Unix epoch, or `0` if the system clock predates it.
#[must_use]
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Days since 1970-01-01 -> `(year, month, day)`. Hinnant's algorithm: shift
/// the epoch to March 1st of year 0 so the leap day lands at the end of the
/// era, which makes the whole conversion branch-free arithmetic.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "unwrap is appropriate in test code — every input is a literal"
)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_suffix() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_mins(30));
        assert_eq!(parse_duration("8h").unwrap(), Duration::from_hours(8));
        // `Duration::from_days` is still unstable, so the 1-day expectation is
        // spelled in hours.
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_hours(24));
    }

    #[test]
    fn a_bare_number_is_seconds() {
        assert_eq!(parse_duration("120").unwrap(), Duration::from_mins(2));
    }

    #[test]
    fn fractional_and_uppercase_units_parse() {
        assert_eq!(parse_duration("0.5h").unwrap(), Duration::from_mins(30));
        assert_eq!(parse_duration("2H").unwrap(), Duration::from_hours(2));
    }

    #[test]
    fn bad_durations_are_errors() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("0").is_err());
        assert!(parse_duration("-5m").is_err());
        assert!(parse_duration("8y").is_err());
    }

    #[test]
    fn formats_a_duration_for_the_report() {
        assert_eq!(format_duration(Duration::from_hours(8)), "8h 00m 00s");
        assert_eq!(format_duration(Duration::from_secs(125)), "0h 02m 05s");
    }

    #[test]
    fn formats_a_known_timestamp() {
        // 2026-07-20T14:15:30Z — verified against `date -u -r 1784556930`.
        assert_eq!(format_utc_label(1_784_556_930), "20260720-141530");
        // The epoch itself.
        assert_eq!(format_utc_label(0), "19700101-000000");
    }

    #[test]
    fn handles_a_leap_day() {
        // 2024-02-29T00:00:00Z.
        assert_eq!(format_utc_label(1_709_164_800), "20240229-000000");
    }
}
