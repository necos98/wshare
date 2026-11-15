//! A point in time with one-second resolution, stored as readable UTC text
//! (`2026-11-15T13:20:05Z`) in the trust files.

use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_MINUTE: u64 = 60;
/// `YYYY-MM-DDTHH:MM:SSZ`.
const TEXT_LEN: usize = 20;

/// Seconds since 1970-01-01 00:00:00 UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(u64);

impl Timestamp {
    pub fn now() -> Self {
        // A clock before 1970 is not worth an error: it reads as the epoch.
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(since_epoch.as_secs())
    }

    #[cfg(test)]
    pub const fn from_unix_seconds(seconds: u64) -> Self {
        Self(seconds)
    }

    /// Whole seconds from `self` to `later`; zero if `later` is earlier
    /// (e.g. the clock was moved back).
    pub fn seconds_until(self, later: Self) -> u64 {
        later.0.saturating_sub(self.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_from_days(self.0 / SECONDS_PER_DAY);
        let in_day = self.0 % SECONDS_PER_DAY;
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            in_day / SECONDS_PER_HOUR,
            in_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
            in_day % SECONDS_PER_MINUTE
        )
    }
}

impl FromStr for Timestamp {
    type Err = anyhow::Error;

    /// Accepts exactly the format [`fmt::Display`] writes.
    fn from_str(text: &str) -> Result<Self> {
        let bytes = text.as_bytes();
        let separators = [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'Z'),
        ];
        // ASCII first, so the byte ranges below are character boundaries.
        if !text.is_ascii()
            || bytes.len() != TEXT_LEN
            || separators.iter().any(|&(at, sep)| bytes[at] != sep)
        {
            bail!("timestamp must look like 2026-01-31T23:59:59Z, got {text:?}");
        }
        let field = |range: std::ops::Range<usize>| -> Result<u64> {
            let digits = &text[range];
            if !digits.bytes().all(|b| b.is_ascii_digit()) {
                bail!("invalid number {digits:?} in timestamp {text:?}");
            }
            digits.parse().context("timestamp field")
        };
        let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
        let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
        if !(1970..=9999).contains(&year)
            || !(1..=12).contains(&month)
            || !(1..=days_in_month(year, month)).contains(&day)
            || hour > 23
            || minute > 59
            || second > 59
        {
            bail!("timestamp {text:?} is out of range");
        }
        let days = days_from_civil(year, month, day);
        Ok(Self(
            days * SECONDS_PER_DAY + hour * SECONDS_PER_HOUR + minute * SECONDS_PER_MINUTE + second,
        ))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

fn is_leap(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a date from 1970 on (H. Hinnant's algorithm,
/// restricted to non-negative years).
fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year % 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    // 719_468: days from 0000-03-01 to 1970-01-01.
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`]: (year, month, day).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let days = days + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = (month_from_march + 2) % 12 + 1;
    let year = era * 400 + year_of_era + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_dates() {
        assert_eq!(Timestamp(0).to_string(), "1970-01-01T00:00:00Z");
        assert_eq!(Timestamp(1_763_212_805).to_string(), "2025-11-15T13:20:05Z");
        // Leap day.
        assert_eq!(Timestamp(951_782_400).to_string(), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn text_roundtrip_over_many_days() {
        for days in (0..30_000).step_by(37) {
            let stamp = Timestamp(days * SECONDS_PER_DAY + 45_296);
            assert_eq!(stamp.to_string().parse::<Timestamp>().unwrap(), stamp);
        }
    }

    #[test]
    fn rejects_malformed_text() {
        for text in [
            "",
            "2025-11-15 13:20:05Z",
            "2025-11-15T13:20:05",
            "2025-13-01T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2025-01-01T24:00:00Z",
            "1969-12-31T23:59:59Z",
            "2025-+1-01T00:00:00Z",
            "2025-01-01T00:00:00Zx",
            "２０２５-01-01T00:00:00Z",
            // 20 bytes, with a two-byte character inside the year.
            "2\u{e9}5-01-01T00:00:00Z",
        ] {
            assert!(text.parse::<Timestamp>().is_err(), "{text:?}");
        }
    }

    #[test]
    fn elapsed_seconds_never_go_negative() {
        let (earlier, later) = (Timestamp(100), Timestamp(160));
        assert_eq!(earlier.seconds_until(later), 60);
        assert_eq!(later.seconds_until(earlier), 0);
    }

    #[test]
    fn now_is_after_2020() {
        assert!(Timestamp::now() > "2020-01-01T00:00:00Z".parse().unwrap());
    }
}
