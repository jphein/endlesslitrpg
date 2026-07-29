//! Minimal UTC date formatting.
//!
//! Hand-rolled rather than pulling `chrono`/`time` in for two format strings. The
//! conversion is Howard Hinnant's `civil_from_days`, which is exact for every date in
//! the proleptic Gregorian calendar; Unix time has no leap seconds, so a plain
//! divide-and-remainder is correct rather than merely close.

use std::time::{SystemTime, UNIX_EPOCH};

const DOW: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MON: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `(year, month, day, hour, minute, second, weekday)` — weekday 0 = Sunday.
pub fn civil_from_unix(secs: i64) -> (i64, i64, i64, i64, i64, i64, usize) {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // 1970-01-01 was a Thursday, hence the +4.
    let weekday = (days + 4).rem_euclid(7) as usize;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (y, m, d, hh, mm, ss, weekday)
}

pub fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `2026-07-29T00:12:13Z` — realm-sigil's `started` / `built` format.
pub fn rfc3339_utc(secs: i64) -> String {
    let (y, m, d, hh, mm, ss, _) = civil_from_unix(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// `Wed, 29 Jul 2026 00:12:13 GMT` — RSS 2.0 requires RFC 822 dates for `pubDate`.
pub fn rfc2822_utc(secs: i64) -> String {
    let (y, m, d, hh, mm, ss, wd) = civil_from_unix(secs);
    let mon = MON[(m as usize - 1).min(11)];
    let dow = DOW[wd.min(6)];
    format!("{dow}, {d:02} {mon} {y:04} {hh:02}:{mm:02}:{ss:02} GMT")
}
