use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, negative for timestamps before it.
pub fn to_epoch_ms(at: SystemTime) -> Option<i64> {
    match at.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).ok(),
        Err(error) => i64::try_from(error.duration().as_millis())
            .map(|value| -value)
            .ok(),
    }
}

/// Milliseconds since the Unix epoch, or `None` for a clock before it.
pub fn epoch_ms(at: SystemTime) -> Option<u64> {
    at.duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// The calendar date in UTC, from milliseconds since the epoch.
///
/// Days-from-civil after Howard Hinnant's algorithm: no month table, no leap-year special case to
/// get wrong, and correct for the whole range a real clock will produce.
pub fn utc_date(at_ms: u64) -> (i64, u32, u32) {
    civil_from_days((at_ms / 86_400_000) as i64)
}

/// `2026-09-23T04:11:07.415Z`
pub fn format_utc(at_ms: u64) -> String {
    let (year, month, day) = utc_date(at_ms);
    let seconds_of_day = (at_ms % 86_400_000) / 1000;
    let (hour, minute, second) = (
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    );
    let millis = at_ms % 1000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
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
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    // The year this counts from starts in March, so January and February belong to the year above.
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn negative_epoch_offsets_survive_conversion() {
        let before_epoch = UNIX_EPOCH - Duration::from_secs(1_000);
        assert_eq!(to_epoch_ms(before_epoch), Some(-1_000_000));
    }

    #[test]
    fn positive_offsets_round_trip() {
        let after = UNIX_EPOCH + Duration::from_millis(1_234_567);
        assert_eq!(to_epoch_ms(after), Some(1_234_567));
    }

    #[test]
    fn a_day_is_a_day_in_utc() {
        assert_eq!(utc_date(0), (1970, 1, 1));
        assert_eq!(utc_date(86_399_999), (1970, 1, 1));
        assert_eq!(utc_date(86_400_000), (1970, 1, 2));
        // A leap day, and the century rule that does not leap.
        assert_eq!(utc_date(951_782_400_000), (2000, 2, 29));
        assert_eq!(utc_date(1_577_836_800_000), (2020, 1, 1));
        // A winter evening, from `date -u -d @1766528059`.
        assert_eq!(utc_date(1_766_528_059_000), (2025, 12, 23));
    }

    #[test]
    fn a_timestamp_reads_as_a_clock() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00.000Z");
        // Both from `date -u -d @<seconds>` on this machine.
        assert_eq!(format_utc(1_727_050_267_415), "2024-09-23T00:11:07.415Z");
        assert_eq!(format_utc(1_766_528_059_000), "2025-12-23T22:14:19.000Z");
    }
}
