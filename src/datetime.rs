//! The rata datetime format, used by default for every timestamp the rata
//! suite writes.
//!
//! ```text
//! YYYY-MM-DD HH:MM:SS.nnnnnnnnn ±HHMMf        2026-11-26 14:30:00.000000000 -0400s
//! ```
//!
//! * Local wall-clock time, with nanoseconds.
//! * The UTC offset in effect at that instant, as `±HHMM`.
//! * A final flag: `s` when daylight saving time is in effect, a space when
//!   it is standard time.
//!
//! The offset makes every value convertible to any other time zone and
//! unique even when clocks go back; the flag records whether the offset was
//! a daylight saving one. Offsets are written in whole minutes; the
//! historical offsets that have seconds are truncated.
//!
//! File names do not use this format (it contains spaces and colons); they
//! keep `YYYYMMDD_HHMMSS`.

use std::fmt;
use std::str::FromStr;

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::{Offset, TimeZone};

/// Length of a formatted value, including the flag.
pub const LEN: usize = 36;

/// An instant, with the local offset and daylight saving flag in effect at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RataTime {
    /// The instant.
    pub timestamp: Timestamp,
    /// The UTC offset of the local time.
    pub offset: Offset,
    /// Whether daylight saving time was in effect.
    pub dst: bool,
}

impl RataTime {
    /// The current time in the system time zone.
    pub fn now() -> Self {
        Self::in_zone(Timestamp::now(), &TimeZone::system())
    }

    /// `timestamp` in the system time zone.
    pub fn from_timestamp(timestamp: Timestamp) -> Self {
        Self::in_zone(timestamp, &TimeZone::system())
    }

    /// `timestamp` in the time zone `tz`.
    pub fn in_zone(timestamp: Timestamp, tz: &TimeZone) -> Self {
        let info = tz.to_offset_info(timestamp);
        // Whole minutes only, so that what is written parses back exactly.
        let secs = info.offset().seconds();
        let offset = Offset::from_seconds(secs - secs % 60).unwrap_or(Offset::UTC);
        RataTime {
            timestamp,
            offset,
            dst: info.dst().is_dst(),
        }
    }

    /// The local wall-clock date and time.
    pub fn datetime(&self) -> DateTime {
        self.offset.to_datetime(self.timestamp)
    }
}

impl fmt::Display for RataTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dt = self.datetime();
        let secs = self.offset.seconds();
        let sign = if secs < 0 { '-' } else { '+' };
        let mins = secs.unsigned_abs() / 60;
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:09} {sign}{:02}{:02}{}",
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second(),
            dt.subsec_nanosecond(),
            mins / 60,
            mins % 60,
            if self.dst { 's' } else { ' ' }
        )
    }
}

impl serde::Serialize for RataTime {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// A string is not in the rata datetime format.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid rata datetime \"{0}\" (expected YYYY-MM-DD HH:MM:SS.nnnnnnnnn ±HHMM followed by s or a space)"
)]
pub struct ParseError(pub String);

impl FromStr for RataTime {
    type Err = ParseError;

    /// Parse the rata datetime format. A missing final flag (for example,
    /// when a tool trimmed the trailing space) is read as standard time.
    fn from_str(s: &str) -> Result<Self, ParseError> {
        parse(s).ok_or_else(|| ParseError(s.to_string()))
    }
}

/// Format `timestamp` in the system time zone.
pub fn format(timestamp: Timestamp) -> String {
    RataTime::from_timestamp(timestamp).to_string()
}

fn parse(s: &str) -> Option<RataTime> {
    let b = s.as_bytes();
    let dst = match b.len() {
        LEN => match b[LEN - 1] {
            b's' => true,
            b' ' => false,
            _ => return None,
        },
        n if n == LEN - 1 => false,
        _ => return None,
    };
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let punct = [
        (4, b'-'),
        (7, b'-'),
        (10, b' '),
        (13, b':'),
        (16, b':'),
        (19, b'.'),
        (29, b' '),
    ];
    if punct.iter().any(|&(i, c)| b[i] != c) {
        return None;
    }
    let sign = match b[30] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec, ns) = (num(11..13)?, num(14..16)?, num(17..19)?, num(20..29)?);
    let (oh, om) = (num(31..33)?, num(33..35)?);
    if om > 59 {
        return None;
    }
    let offset = Offset::from_seconds((sign * (oh * 3600 + om * 60)) as i32).ok()?;
    // DateTime::new validates every field (including the day of the month).
    let civil = DateTime::new(
        y as i16, mo as i8, d as i8, h as i8, mi as i8, sec as i8, ns as i32,
    )
    .ok()?;
    let timestamp = offset.to_timestamp(civil).ok()?;
    Some(RataTime {
        timestamp,
        offset,
        dst,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ny() -> TimeZone {
        TimeZone::get("America/New_York").unwrap()
    }

    #[test]
    fn formats_standard_and_daylight_time() {
        let winter: Timestamp = "2026-01-15T19:30:00.000000001Z".parse().unwrap();
        assert_eq!(
            RataTime::in_zone(winter, &ny()).to_string(),
            "2026-01-15 14:30:00.000000001 -0500 "
        );
        let summer: Timestamp = "2026-07-15T18:30:00.5Z".parse().unwrap();
        assert_eq!(
            RataTime::in_zone(summer, &ny()).to_string(),
            "2026-07-15 14:30:00.500000000 -0400s"
        );
        let india = TimeZone::get("Asia/Kolkata").unwrap();
        assert_eq!(
            RataTime::in_zone(winter, &india).to_string(),
            "2026-01-16 01:00:00.000000001 +0530 "
        );
    }

    #[test]
    fn round_trips() {
        for ts in [
            "2026-01-15T19:30:00.000000001Z",
            "2026-07-15T18:30:00.123456789Z",
        ] {
            let ts: Timestamp = ts.parse().unwrap();
            let t = RataTime::in_zone(ts, &ny());
            assert_eq!(t.to_string().parse::<RataTime>().unwrap(), t);
            assert_eq!(t.to_string().len(), LEN);
        }
    }

    #[test]
    fn unique_when_clocks_go_back() {
        // 2026-11-01 01:30 happens twice in New York.
        let first: Timestamp = "2026-11-01T05:30:00Z".parse().unwrap();
        let second: Timestamp = "2026-11-01T06:30:00Z".parse().unwrap();
        let a = RataTime::in_zone(first, &ny()).to_string();
        let b = RataTime::in_zone(second, &ny()).to_string();
        assert_eq!(a, "2026-11-01 01:30:00.000000000 -0400s");
        assert_eq!(b, "2026-11-01 01:30:00.000000000 -0500 ");
        assert_eq!(a.parse::<RataTime>().unwrap().timestamp, first);
        assert_eq!(b.parse::<RataTime>().unwrap().timestamp, second);
    }

    #[test]
    fn missing_flag_is_standard_time() {
        let t: RataTime = "2026-01-15 14:30:00.000000000 -0500".parse().unwrap();
        assert!(!t.dst);
        assert_eq!(t.timestamp.to_string(), "2026-01-15T19:30:00Z");
    }

    #[test]
    fn rejects_other_formats() {
        for bad in [
            "",
            "2026-01-15T14:30:00.000000000 -0500 ",
            "2026-01-15 14:30:00 -0500 ",
            "2026-01-15 14:30:00.000000000 -05:00",
            "2026-13-15 14:30:00.000000000 -0500 ",
            "2026-02-30 14:30:00.000000000 -0500 ",
            "2026-01-15 24:30:00.000000000 -0500 ",
            "2026-01-15 14:30:00.000000000 -0500x",
            "2026-01-15 14:30:00.00000000a -0500 ",
        ] {
            assert!(bad.parse::<RataTime>().is_err(), "{bad:?}");
        }
    }
}
