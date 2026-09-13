//! Just enough calendar arithmetic to print a date.
//!
//! Records store epoch seconds, which is what comparisons and sorting want.
//! Humans reading "why is this on my machine" want a date, so this converts.
//! A whole date-time crate for one format string is not worth the dependency
//! in something that ships in an initramfs.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Epoch seconds to `YYYY-MM-DD HH:MM:SS` in UTC.
///
/// Uses Howard Hinnant's civil-from-days algorithm, which is exact for the
/// proleptic Gregorian calendar and needs no tables.
pub fn format_utc(epoch_seconds: u64) -> String {
    let days = (epoch_seconds / 86_400) as i64;
    let seconds_of_day = epoch_seconds % 86_400;

    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_itself() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00");
    }

    #[test]
    fn a_known_instant() {
        // 2026-09-13 22:36:51 UTC
        assert_eq!(format_utc(1_789_339_011), "2026-09-13 22:36:51");
    }

    #[test]
    fn leap_day_is_handled() {
        // 2024-02-29 00:00:00 UTC
        assert_eq!(format_utc(1_709_164_800), "2024-02-29 00:00:00");
    }

    #[test]
    fn a_century_boundary_is_handled() {
        // 2000-03-01: 2000 is a leap year, 1900 was not.
        assert_eq!(format_utc(951_868_800), "2000-03-01 00:00:00");
    }
}
