//! Session ids and timestamps.

use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;
use rand::rngs::OsRng;

/// Current UTC time as RFC 3339 with second precision, e.g.
/// `2026-10-30T09:15:00Z`.
pub fn utc_now_rfc3339() -> String {
    format_utc(unix_now())
}

/// A new session id: UTC timestamp plus 32 random bits, e.g.
/// `20261030T091500Z-1a2b3c4d`. Valid as a file name.
pub fn new_session_id() -> String {
    let stamp: String = format_utc(unix_now())
        .chars()
        .filter(|c| !matches!(c, '-' | ':'))
        .collect();
    format!("{stamp}-{:08x}", OsRng.next_u32())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Formats seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Proleptic Gregorian date of a day count since 1970-01-01
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_793_355_300), "2026-10-30T10:15:00Z");
    }

    #[test]
    fn session_ids_are_unique_and_file_safe() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), "20261030T091500Z-1a2b3c4d".len());
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }
}
