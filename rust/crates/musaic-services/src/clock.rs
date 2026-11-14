//! `_utc_iso_now()` / `cache._now_iso()`: `2026-11-10T12:34:56Z`.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Source of the ISO-8601 timestamps the services stamp (missing_since,
/// scanned_at, started_at, ...). Injected so tests can pin it.
pub type Clock = Arc<dyn Fn() -> String + Send + Sync>;

/// The real clock.
pub fn system_clock() -> Clock {
    Arc::new(utc_iso_now)
}

/// Current UTC time, seconds precision, `Z` suffix.
pub fn utc_iso_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    format_utc(secs)
}

/// Seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::format_utc;

    #[test]
    fn formats_like_isoformat_seconds_z() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_794_363_720), "2026-11-11T02:22:00Z");
    }
}
