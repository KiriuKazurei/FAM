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
}
