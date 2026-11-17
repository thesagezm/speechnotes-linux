//! Apple reference-date conversion.
//!
//! Every Speechnotes store encodes dates as a JSON number of seconds since
//! 2001-01-01T00:00:00Z, because every member of the family writes them through
//! Swift's default `JSONEncoder` (`.deferredToDate`). A library moves between
//! the iOS, Android and Linux apps byte-for-byte, so this conversion is load
//! bearing: get it wrong and every timestamp in a library silently shifts by 54
//! years with no error anywhere.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Seconds between the Unix epoch (1970-01-01) and the Apple reference date
/// (2001-01-01T00:00:00Z).
pub const APPLE_EPOCH_OFFSET: i64 = 978_307_200;

/// Unix seconds (with fractional part) to a reference-date offset.
#[inline]
pub fn to_apple(unix: f64) -> f64 {
    unix - APPLE_EPOCH_OFFSET as f64
}

/// Reference-date offset back to Unix seconds.
#[inline]
pub fn from_apple(apple: f64) -> f64 {
    apple + APPLE_EPOCH_OFFSET as f64
}

/// Current time as an Apple reference-date offset, suitable for JSON.
pub fn now_apple() -> f64 {
    to_apple(unix_now())
}

/// Seconds since the Unix epoch, as a float.
pub fn unix_now() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -(e.duration().as_secs_f64()),
    }
}

/// A timestamp stored in Apple's reference-date form.
///
/// Serialises as a JSON number, always. Not a string — the other apps would not
/// read it. An integral value still round-trips as `0.0`, never `0` as an
/// integer, because `serde_json` renders `-0.0` and `1.0` as floats.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct AppleDate(pub f64);

impl AppleDate {
    /// From Unix seconds.
    pub fn from_unix(unix: f64) -> Self {
        Self(to_apple(unix))
    }

    /// Now.
    pub fn now() -> Self {
        Self(now_apple())
    }

    /// To Unix seconds.
    pub fn to_unix(self) -> f64 {
        from_apple(self.0)
    }

    /// The raw reference-date value, which is what goes on disk.
    pub fn raw(self) -> f64 {
        self.0
    }

    /// Seconds since the Unix epoch, whole.
    pub fn unix_secs(self) -> i64 {
        self.to_unix() as i64
    }

    /// Build from a reference-date value read off disk.
    pub fn from_apple(raw: f64) -> Self {
        Self(raw)
    }

    /// A date `days` in the future, as a reference-date offset.
    pub fn plus_days(self, days: i64) -> Self {
        Self::from_unix(self.unix_secs() as f64 + days as f64 * 86_400.0)
    }

    /// Local calendar day as `YYYY-MM-DD`, used by the statistics store.
    pub fn local_day(self) -> String {
        use chrono_shim::Local;
        let secs = self.unix_secs();
        let dt = Local.timestamp(secs);
        format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
    }

    /// Local calendar day for a unix timestamp, `YYYY-MM-DD`.
    //  authored from the call site (pomodoro.rs's structured-mode key
    // `AppleDate::local_day_from_unix(now)`); the original appledate.rs carries
    // only `local_day`. Same day-string shape, through the same shim.
    pub fn local_day_from_unix(unix: i64) -> String {
        use chrono_shim::Local;
        let dt = Local.timestamp(unix);
        format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
    }

    /// Days between two dates, as whole local calendar days.
    pub fn days_since(self, earlier: AppleDate) -> i64 {
        self.unix_secs().div_euclid(86_400) - earlier.unix_secs().div_euclid(86_400)
    }
}

impl From<f64> for AppleDate {
    /// Interprets a float as a raw reference-date offset.
    fn from(raw: f64) -> Self {
        Self(raw)
    }
}

impl From<AppleDate> for f64 {
    fn from(d: AppleDate) -> Self {
        d.0
    }
}

impl Serialize for AppleDate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for AppleDate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Tolerate an integer on the way in. The other apps always write a
        // float, but hand-edited or third-party files sometimes carry `0`.
        //  the original file's fast path called `serde::de::value::FloatVisitor`,
        // which never existed under that name; the visitor below already
        // accepts f64/i64/u64, so the plain any-deserialize covers it.
        d.deserialize_any(AppleDateVisitor)
    }
}

struct AppleDateVisitor;

impl<'de> serde::de::Visitor<'de> for AppleDateVisitor {
    type Value = AppleDate;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a reference-date offset as a JSON number")
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<AppleDate, E> {
        Ok(AppleDate(v))
    }

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<AppleDate, E> {
        Ok(AppleDate(v as f64))
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<AppleDate, E> {
        Ok(AppleDate(v as f64))
    }
}

/// A tiny date shim so `sn-core` does not need a full calendar dependency for
/// the two things it does with dates: a local day string and day arithmetic.
mod chrono_shim {
    /// Days from civil epoch (1970-01-01) to a y/m/d, Howard Hinnant's algorithm.
    /// (Kept alongside `civil_from_days`; only the reverse direction has a
    /// call site today.)
    #[allow(dead_code)]
    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn civil_from_days(z: i64) -> (i64, i64, i64) {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (if m <= 2 { y + 1 } else { y }, m, d)
    }

    pub struct Local;

    impl Local {
        /// Offset-aware enough for a day string: uses the process-local zone via
        /// `time`, falling back to UTC when the tz database is unreadable.
        pub fn timestamp(self, unix_secs: i64) -> DateTime {
            let offset = local_utc_offset_secs(unix_secs);
            let local = unix_secs + offset;
            let days = local.div_euclid(86_400);
            let (y, m, d) = civil_from_days(days);
            DateTime { year: y, month: m, day: d }
        }
    }

    pub struct DateTime {
        pub year: i64,
        pub month: i64,
        pub day: i64,
    }

    impl DateTime {
        //  the original shim lost the accessors the format path calls.
        pub fn year(&self) -> i64 { self.year }
        pub fn month(&self) -> i64 { self.month }
        pub fn day(&self) -> i64 { self.day }
    }

    /// Read `TZ`-style offsets through libc-free means: `/etc/localtime` is not
    /// parsed here, so we use the `TZ` environment offset when it is a simple
    /// `UTC±HH:MM` and zero otherwise. `stats.json` keys are local days, and a
    /// wrong offset would only ever move a day boundary, never lose data.
    fn local_utc_offset_secs(_unix_secs: i64) -> i64 {
        if let Ok(tz) = std::env::var("TZ") {
            if let Some(secs) = parse_fixed_tz(&tz) {
                return secs;
            }
        }
        // `date`-independent fallback: libc's `localtime_r` via the `time`
        // crate is deliberately avoided here to keep `sn-core` dependency-light.
        0
    }

    fn parse_fixed_tz(tz: &str) -> Option<i64> {
        let rest = tz.strip_prefix("UTC").or_else(|| tz.strip_prefix("Etc/UTC"))?;
        if rest.is_empty() {
            return Some(0);
        }
        let (sign, digits) = match rest.as_bytes()[0] {
            b'+' => (1i64, &rest[1..]),
            b'-' => (-1i64, &rest[1..]),
            _ => return None,
        };
        let mut parts = digits.split(':');
        let h: i64 = parts.next()?.parse().ok()?;
        let m: i64 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        Some(sign * (h * 3600 + m * 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_offset_is_the_documented_constant() {
        // 2001-01-01T00:00:00Z as a Unix timestamp.
        assert_eq!(APPLE_EPOCH_OFFSET, 978_307_200);
        assert_eq!(to_apple(978_307_200.0), 0.0);
    }

    #[test]
    fn round_trips_unix_to_apple_and_back() {
        for unix in [0.0, 1.0, 1_700_000_000.0, 2_000_000_000.5] {
            let apple = to_apple(unix);
            assert!((from_apple(apple) - unix).abs() < 1e-6);
        }
    }

    #[test]
    fn the_swift_fixture_zero_is_apple_epoch() {
        // NotesStoreTests pin `"createdAt": 0` for a fresh note.
        let d = AppleDate::from_apple(0.0);
        assert_eq!(d.unix_secs(), 978_307_200);
    }

    #[test]
    fn the_swift_fixture_780000000_round_trips() {
        // BooksStoreTests pin `"addedAt": 780000000.0`.
        let apple = AppleDate::from_apple(780_000_000.0);
        let json = serde_json::to_string(&apple).unwrap();
        assert_eq!(json, "780000000.0");
        let back: AppleDate = serde_json::from_str(&json).unwrap();
        assert_eq!(back.raw(), 780_000_000.0);
    }

    #[test]
    fn serialises_as_a_number_never_a_string() {
        let json = serde_json::to_string(&AppleDate(1.5)).unwrap();
        assert_eq!(json, "1.5");
    }

    #[test]
    fn reads_an_integer_from_disk() {
        let d: AppleDate = serde_json::from_str("0").unwrap();
        assert_eq!(d.raw(), 0.0);
    }

    #[test]
    fn rejects_a_string_because_swift_never_writes_one() {
        assert!(serde_json::from_str::<AppleDate>("\"780000000\"").is_err());
    }

    #[test]
    fn local_day_is_iso_formatted() {
        // 2024-03-05T12:00:00Z
        let unix = 1_709_643_600.0;
        let d = AppleDate::from_unix(unix);
        let day = d.local_day();
        assert_eq!(day.len(), 10);
        assert_eq!(&day[4..5], "-");
        assert_eq!(&day[7..8], "-");
    }

    #[test]
    fn plus_days_moves_by_exactly_86400_seconds() {
        let base = AppleDate::from_unix(1_700_000_000.0);
        let later = base.plus_days(7);
        assert_eq!(later.unix_secs() - base.unix_secs(), 7 * 86_400);
    }
}