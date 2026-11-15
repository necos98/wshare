//! "How long ago", in Italian, for the last access of an authorized device.
//! Relative, so it needs no time zone. Pure.

use crate::trust::Timestamp;

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// E.g. "adesso", "5 minuti fa", "1 ora fa", "ieri", "12 giorni fa".
pub fn ago(then: Timestamp, now: Timestamp) -> String {
    let seconds = then.seconds_until(now);
    match seconds {
        s if s < MINUTE => "adesso".to_string(),
        s if s < HOUR => count(s / MINUTE, "minuto", "minuti"),
        s if s < DAY => count(s / HOUR, "ora", "ore"),
        s if s < 2 * DAY => "ieri".to_string(),
        s => count(s / DAY, "giorno", "giorni"),
    }
}

fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {} fa", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> Timestamp {
        Timestamp::from_unix_seconds(seconds)
    }

    #[test]
    fn wording_per_range() {
        let now = 10 * DAY;
        for (elapsed, text) in [
            (0, "adesso"),
            (59, "adesso"),
            (MINUTE, "1 minuto fa"),
            (5 * MINUTE + 30, "5 minuti fa"),
            (HOUR, "1 ora fa"),
            (23 * HOUR, "23 ore fa"),
            (DAY + HOUR, "ieri"),
            (2 * DAY, "2 giorni fa"),
        ] {
            assert_eq!(ago(at(now - elapsed), at(now)), text, "{elapsed}");
        }
    }

    #[test]
    fn a_future_time_reads_as_now() {
        assert_eq!(ago(at(500), at(100)), "adesso");
    }
}
