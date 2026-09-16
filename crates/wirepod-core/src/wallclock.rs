//! The wall clock seam: calendar time, and the local UTC offset at an instant.
//!
//! [`Clock`](crate::clock::Clock) is deliberately monotonic and carries no
//! calendar accessor, because nothing the pinger measures needs one. The token
//! server does need one: `CreateJWT` stamps `time.Now()` and
//! `time.Now().AddDate(0, 1, 0)` into the `iat` and `expires` claims
//! (`token.go:195-196`) and formats both with `time.RFC3339Nano`
//! (`token.go:29`), which is `time.Local` there, so the local offset reaches
//! the wire. This module is that second seam, kept apart from the first so the
//! monotonic clock stays a clock with no calendar in it.
//!
//! The offset is resolved per instant rather than read once, because the two
//! claims are one calendar month apart and can sit on opposite sides of a
//! daylight saving transition. For roughly two months a year `expires` carries
//! a different offset from `iat`, which is the rule the probe's
//! `addmonth_local` section records.
//!
//! The clock is read twice there as well, once per claim (`token.go:195`,
//! `token.go:196`), so Go's two claims carry sub-second fractions a few
//! hundred nanoseconds apart rather than the same one. A caller that wants the
//! bytes Go writes calls [`WallClock::now`] twice too. Nothing downstream can
//! observe the difference, and nothing can pin it either, since both values
//! are timestamps, but the claim payload is a byte contract and matching it
//! costs a second call.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A point on the wall clock, as Go's `time.Time` reaches the format layer.
///
/// Go keeps a `time.Time` as seconds plus a nanosecond field that is always in
/// `[0, 1e9)`, and its `Location` separately. The split here is the same, with
/// the location left to the [`WallClock`] that resolved it, because every
/// formatter in [`timefmt`](crate::timefmt) takes the offset as an argument
/// rather than carrying a zone around.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WallTime {
    /// Seconds since the Unix epoch, negative before it.
    pub unix_secs: i64,
    /// Nanoseconds within the second, always in `[0, 1e9)`.
    pub nanos: u32,
}

impl WallTime {
    /// A wall time at `unix_secs` plus `nanos`.
    ///
    /// The nanosecond field is not normalized: a value at or above one second
    /// is a caller bug, and the formatters would write a ten digit fraction
    /// rather than carrying it into the seconds the way Go's `time.Date`
    /// does.
    pub const fn new(unix_secs: i64, nanos: u32) -> Self {
        Self { unix_secs, nanos }
    }
}

/// A calendar clock, read as an instant plus the local offset at an instant.
///
/// Two methods rather than one reading that carries its own offset, because
/// [`add_months`](crate::timefmt::add_months) has to ask about an instant it
/// computed rather than about now: the whole point of the seam is that the
/// offset for `expires` is looked up at `expires`, not inherited from `iat`.
pub trait WallClock: Send + Sync {
    /// The current instant.
    fn now(&self) -> WallTime;

    /// The local zone's offset from UTC, in seconds, in effect at `unix_secs`.
    ///
    /// East of Greenwich is positive, matching Go's `Time.Zone`. A zone that
    /// observes daylight saving returns two different values across its
    /// transition, so the same wall clock answers differently for two instants
    /// a month apart.
    fn utc_offset_secs_at(&self, unix_secs: i64) -> i32;
}

/// A [`WallClock`] reading the system clock and the system's local zone.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWallClock;

impl SystemWallClock {
    /// A clock over the real system clock.
    pub const fn new() -> Self {
        Self
    }
}

impl WallClock for SystemWallClock {
    fn now(&self) -> WallTime {
        wall_time_from_reading(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|before| before.duration()),
        )
    }

    fn utc_offset_secs_at(&self, unix_secs: i64) -> i32 {
        platform::utc_offset_secs_at(unix_secs)
    }
}

/// Splits a system clock reading into Go's seconds plus non-negative fraction.
///
/// `Ok` is the distance after the epoch and `Err` the distance before it,
/// which is the shape [`SystemTime::duration_since`] reports. A reading before
/// the epoch, which a badly set clock can produce, borrows a second so the
/// nanosecond field stays in `[0, 1e9)`: that is the invariant `time.Time`
/// keeps, and every formatter in [`timefmt`](crate::timefmt) writes the
/// fraction as unsigned digits, so a negative one would come out as garbage
/// rather than as an earlier instant.
///
/// This is split out of [`SystemWallClock::now`] because the pre-epoch arm is
/// unreachable from a correctly set machine and would otherwise ship untested.
fn wall_time_from_reading(reading: Result<Duration, Duration>) -> WallTime {
    match reading {
        Ok(since) => WallTime::new(
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
            since.subsec_nanos(),
        ),
        Err(before) => {
            let secs = i64::try_from(before.as_secs()).unwrap_or(i64::MAX);
            let nanos = before.subsec_nanos();
            if nanos == 0 {
                WallTime::new(-secs, 0)
            } else {
                WallTime::new(-secs - 1, 1_000_000_000 - nanos)
            }
        }
    }
}

/// A [`WallClock`] frozen at one instant in one fixed offset zone.
///
/// This is Go's `time.FixedZone` plus a stopped clock, and it is what makes
/// every test that formats a time deterministic. A fixed offset answers the
/// same for every instant, so a test that needs a daylight saving transition
/// needs its own [`WallClock`] rather than this one.
#[derive(Clone, Copy, Debug, Default)]
pub struct FixedWallClock {
    now: WallTime,
    offset_secs: i32,
}

impl FixedWallClock {
    /// A clock stopped at `now`, in a zone whose offset is always
    /// `offset_secs`.
    pub const fn new(now: WallTime, offset_secs: i32) -> Self {
        Self { now, offset_secs }
    }
}

impl WallClock for FixedWallClock {
    fn now(&self) -> WallTime {
        self.now
    }

    fn utc_offset_secs_at(&self, _unix_secs: i64) -> i32 {
        self.offset_secs
    }
}

/// The local offset, which is the one thing here that is not portable.
///
/// Go resolves it through a `Location`, built on Windows from
/// `GetTimeZoneInformation` and on Unix from the timezone database. Each
/// platform gets the call that matches what Go does there.
#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

    use crate::timefmt::{SECS_PER_DAY, civil_from_days, days_from_civil};

    /// The first and last years `SYSTEMTIME` can hold, which is the range the
    /// Win32 conversions accept.
    const FIRST_YEAR: i64 = 1601;
    const LAST_YEAR: i64 = 30827;

    /// The offset by difference: convert the instant to a local wall time
    /// through Win32, then subtract.
    ///
    /// A null zone argument means the currently active zone, which is the same
    /// `TIME_ZONE_INFORMATION` Go builds `time.Local` from on Windows
    /// (`time/zoneinfo_windows.go:230`, `initLocal`, on
    /// `syscall.GetTimeZoneInformation`). That carries Go's documented Windows
    /// bug with it (`time/zoneinfo_windows.go:17-20`), that this year's
    /// daylight saving rule is assumed to hold for every year, so the two
    /// disagree with a tzdata build in the same way and agree with each other.
    ///
    /// Go falls back to UTC when the zone cannot be read at all, and so does
    /// this, which is also what an instant outside the `SYSTEMTIME` range
    /// gets. Neither is reachable from a system clock reading.
    pub(super) fn utc_offset_secs_at(unix_secs: i64) -> i32 {
        let Some(utc) = system_time_from_unix(unix_secs) else {
            return 0;
        };
        let mut local = SYSTEMTIME {
            wYear: 0,
            wMonth: 0,
            wDayOfWeek: 0,
            wDay: 0,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        // SAFETY: the zone pointer is null, which the call documents as "the
        // active zone"; the other two point at live, correctly typed locals
        // that outlive the call, and the call writes only through the third.
        let converted =
            unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
        if converted == 0 {
            return 0;
        }
        i32::try_from(unix_from_system_time(&local) - unix_secs).unwrap_or(0)
    }

    /// The UTC calendar fields of an instant, or `None` outside the range
    /// `SYSTEMTIME` can hold.
    fn system_time_from_unix(unix_secs: i64) -> Option<SYSTEMTIME> {
        let date = civil_from_days(unix_secs.div_euclid(SECS_PER_DAY));
        if date.year < FIRST_YEAR || date.year > LAST_YEAR {
            return None;
        }
        let secs = unix_secs.rem_euclid(SECS_PER_DAY);
        Some(SYSTEMTIME {
            wYear: u16::try_from(date.year).ok()?,
            wMonth: u16::try_from(date.month).ok()?,
            // Ignored by the conversion, which derives it from the date.
            wDayOfWeek: 0,
            wDay: u16::try_from(date.day).ok()?,
            wHour: u16::try_from(secs / 3600).ok()?,
            wMinute: u16::try_from((secs / 60) % 60).ok()?,
            wSecond: u16::try_from(secs % 60).ok()?,
            wMilliseconds: 0,
        })
    }

    /// The same fields read back as seconds, so the two can be subtracted.
    ///
    /// Milliseconds are dropped because the difference is the zone offset,
    /// which the conversion never moves by a fraction of a second.
    fn unix_from_system_time(value: &SYSTEMTIME) -> i64 {
        days_from_civil(
            i64::from(value.wYear),
            u32::from(value.wMonth),
            u32::from(value.wDay),
        ) * SECS_PER_DAY
            + i64::from(value.wHour) * 3600
            + i64::from(value.wMinute) * 60
            + i64::from(value.wSecond)
    }
}

#[cfg(unix)]
mod platform {
    use std::sync::Once;

    /// `localtime_r`'s `tm_gmtoff`, which is the offset the timezone database
    /// gives for that instant.
    ///
    /// Go does not call libc here. It reads the same zone files itself, with
    /// its own tzfile parser, from the directories listed at
    /// `time/zoneinfo_unix.go:21-26` and through `initLocal` at
    /// `time/zoneinfo_unix.go:28`. So the two read one database through two
    /// readers: they agree on a host whose libc reads those same files, which
    /// is every glibc host, but an unusual `TZ` value or a musl build can part
    /// them. The Windows arm above is the call Go itself makes and is the
    /// platform this server runs on, so this arm is the one a Linux cutover
    /// would have to re-check.
    ///
    /// `tzset` runs once before the first conversion because POSIX does not
    /// require `localtime_r` to run it, unlike `localtime`. A failed
    /// conversion falls back to UTC, matching what Go does when it cannot load
    /// a zone at all.
    pub(super) fn utc_offset_secs_at(unix_secs: i64) -> i32 {
        static TZSET: Once = Once::new();
        // SAFETY: `tzset` reads the environment and writes the libc zone
        // globals. The `Once` is what keeps it off the path of a concurrent
        // `localtime_r` after the first call.
        TZSET.call_once(|| unsafe { libc::tzset() });

        let Ok(when) = libc::time_t::try_from(unix_secs) else {
            return 0;
        };
        let mut broken: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers are to live locals that outlive the call, and
        // the reentrant form writes only through the second one.
        let resolved = unsafe { libc::localtime_r(&when, &mut broken) };
        if resolved.is_null() {
            return 0;
        }
        i32::try_from(broken.tm_gmtoff).unwrap_or(0)
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    /// UTC, because there is no call to ask on a platform that is neither.
    pub(super) fn utc_offset_secs_at(_unix_secs: i64) -> i32 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, WallTime, wall_time_from_reading};

    /// Go's `time.Time` keeps its nanosecond field in `[0, 1e9)` on both sides
    /// of the epoch, so a reading before it borrows a second rather than
    /// carrying a negative fraction. Nothing on a correctly set machine
    /// reaches this arm, which is exactly why it is asserted here.
    #[test]
    fn a_reading_before_the_epoch_borrows_a_second_for_its_fraction() {
        assert_eq!(
            wall_time_from_reading(Ok(Duration::ZERO)),
            WallTime::new(0, 0)
        );
        assert_eq!(
            wall_time_from_reading(Ok(Duration::new(1, 500_000_000))),
            WallTime::new(1, 500_000_000)
        );
        assert_eq!(
            wall_time_from_reading(Err(Duration::new(1, 500_000_000))),
            WallTime::new(-2, 500_000_000)
        );
        assert_eq!(
            wall_time_from_reading(Err(Duration::new(1, 0))),
            WallTime::new(-1, 0)
        );
        assert_eq!(
            wall_time_from_reading(Err(Duration::new(0, 1))),
            WallTime::new(-1, 999_999_999)
        );
    }
}
