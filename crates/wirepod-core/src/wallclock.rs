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
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::{
        GetTimeZoneInformation, TIME_ZONE_ID_INVALID, TIME_ZONE_INFORMATION,
    };

    use crate::timefmt::{SECS_PER_DAY, civil_from_days, days_from_civil};

    /// How far either side of the year it runs in `initLocalFromTZI` builds
    /// transitions (`time/zoneinfo_windows.go:185-191`).
    const YEARS_EITHER_SIDE: i64 = 100;

    /// Two transitions a year for every year in the window, which is the length
    /// Go gives the slice at `time/zoneinfo_windows.go:186`.
    const TRANSITIONS: usize = 4 * YEARS_EITHER_SIDE as usize;

    /// The offset `time.Local` reports for `unix_secs`, rebuilt the way Go
    /// builds it rather than asked of Windows.
    ///
    /// Windows will answer this question itself, through
    /// `SystemTimeToTzSpecificLocalTime`, and its answer is not Go's. That call
    /// consults the per-year daylight rules the operating system keeps, so it
    /// knows the United States moved the end of daylight saving in 2007 and
    /// reports -08:00 for `2005-11-01T19:00:00Z`. Go does not. `initLocal`
    /// (`time/zoneinfo_windows.go:230-237`) reads one `TIME_ZONE_INFORMATION`
    /// through `GetTimeZoneInformation`, and `initLocalFromTZI` (`:137-202`)
    /// applies that one rule to every year from a hundred before this one to a
    /// hundred after it. The package says so itself, in a `BUG` note at
    /// `:17-20`. Go answers -07:00 for that instant, and measured on this
    /// machine the two also part at `1985-11-01T19:00:00Z` and at every instant
    /// outside the two hundred year window.
    ///
    /// `time.RFC3339Nano` writes that offset into the `iat` and `expires`
    /// claims (`token.go:29`, `token.go:195-196`), so what reaches the robot is
    /// Go's answer, and this arm reproduces Go's rather than Windows'.
    ///
    /// The zone is read once per process, because `initLocal` reads it once, so
    /// a zone changed while the server runs is ignored by both. The year the
    /// window is centred on is pinned at the same moment, because Go centres it
    /// on `Now().UTC().Year()` (`:188-189`) and never rebuilds the table
    /// either.
    pub(super) fn utc_offset_secs_at(unix_secs: i64) -> i32 {
        local_zone().offset_secs_at(unix_secs)
    }

    /// `time.Local`, built once, which is what `initLocal` is
    /// (`time/zoneinfo_windows.go:230-237`).
    ///
    /// Go falls back to UTC when the zone cannot be read at all: `:233-234`
    /// leaves `localLoc` with an empty zone slice, which `lookup` answers as
    /// UTC (`time/zoneinfo.go:152-159`). So does this.
    fn local_zone() -> &'static LocalZone {
        static LOCAL: OnceLock<LocalZone> = OnceLock::new();

        LOCAL.get_or_init(|| {
            // SAFETY: `TIME_ZONE_INFORMATION` is a plain struct of integers and
            // arrays of them, so an all-zero value is a valid one, and the
            // pointer is to a live local that outlives the call, which writes
            // only through it.
            let mut tzi: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
            let id = unsafe { GetTimeZoneInformation(&mut tzi) };
            if id == TIME_ZONE_ID_INVALID {
                return LocalZone::utc();
            }
            LocalZone::from_tzi(&tzi, current_utc_year())
        })
    }

    /// The UTC year `initLocalFromTZI` centres its table on
    /// (`time/zoneinfo_windows.go:188-189`).
    fn current_utc_year() -> i64 {
        use super::WallClock as _;

        let now = super::SystemWallClock::new().now();
        civil_from_days(now.unix_secs.div_euclid(SECS_PER_DAY)).year
    }

    /// One entry of Go's `l.tx`: an instant, and the zone in effect from it.
    struct Transition {
        /// The instant the offset changes, in Unix seconds.
        when: i64,
        /// The index into [`LocalZone::zone`] that applies from `when` on.
        index: usize,
    }

    /// As much of Go's `time.Local` as an offset lookup needs.
    struct LocalZone {
        /// Go's `l.zone`: the standard offset first, the daylight offset second
        /// (`time/zoneinfo_windows.go:146-172`). A zone with no daylight rule
        /// leaves the second slot unused, as Go's one-element slice does.
        zone: [i64; 2],
        /// Go's `l.tx`, empty when the zone has no daylight rule. Go builds one
        /// entry there instead, at the start of time and pointing at the
        /// standard zone (`:155-160`), which answers the same for every instant
        /// as having no transitions at all.
        tx: Vec<Transition>,
    }

    impl LocalZone {
        /// UTC, which is what Go is left with when it cannot read the zone.
        fn utc() -> Self {
            Self {
                zone: [0, 0],
                tx: Vec::new(),
            }
        }

        /// Go's `initLocalFromTZI` (`time/zoneinfo_windows.go:137-202`), with
        /// the year the table is centred on passed in rather than read off the
        /// clock, so that a test can fix it.
        ///
        /// Only the offsets are built. The zone names `abbrev` (`:90-107`) digs
        /// out of the registry never reach a formatter here, because
        /// `time.RFC3339Nano` writes `Z07:00` and not `MST`.
        fn from_tzi(tzi: &TIME_ZONE_INFORMATION, base_year: i64) -> Self {
            // A zero standard month is how Windows says the zone has no
            // daylight rule (`:142-145`). Go then answers `-Bias` for all time
            // and ignores `StandardBias`, which its own comment at `:164-166`
            // calls out: that bias is only read after the single-zone return at
            // `:153-161`.
            if tzi.StandardDate.wMonth == 0 {
                return Self {
                    zone: [-i64::from(tzi.Bias) * 60, 0],
                    tx: Vec::new(),
                };
            }

            // `:167-172`. Both biases are minutes west of Greenwich, so the
            // sign flips and the scale is sixty.
            let zone = [
                -(i64::from(tzi.Bias) + i64::from(tzi.StandardBias)) * 60,
                -(i64::from(tzi.Bias) + i64::from(tzi.DaylightBias)) * 60,
            ];

            // `:174-183`: `d0` is whichever rule falls in the earlier month and
            // `i0` the zone it switches to, so the pairs come out in order.
            let (d0, d1, i0, i1) = if tzi.StandardDate.wMonth > tzi.DaylightDate.wMonth {
                (&tzi.DaylightDate, &tzi.StandardDate, 1, 0)
            } else {
                (&tzi.StandardDate, &tzi.DaylightDate, 0, 1)
            };

            // `:185-201`. Each rule names a wall time, so the offset in effect
            // just before the change is what turns it into an instant, and that
            // is the other zone's.
            let mut tx = Vec::with_capacity(TRANSITIONS);
            for year in (base_year - YEARS_EITHER_SIDE)..(base_year + YEARS_EITHER_SIDE) {
                tx.push(Transition {
                    when: pseudo_unix(year, d0) - zone[i1],
                    index: i0,
                });
                tx.push(Transition {
                    when: pseudo_unix(year, d1) - zone[i0],
                    index: i1,
                });
            }
            Self { zone, tx }
        }

        /// Go's `lookup` (`time/zoneinfo.go:149-206`), reduced to the offset.
        ///
        /// Before the first transition Go asks `lookupFirstZone` (`:170-182`,
        /// `:233-257`), and all four of that function's cases answer zero here:
        /// the first zone is the standard one, it is never the daylight one,
        /// and the case that walks back from the daylight index lands on
        /// exactly it. An instant older than the table therefore carries the
        /// standard offset, which is why a US Pacific machine answers -08:00
        /// for July 1925 and -07:00 for July 1926.
        ///
        /// Past the last transition Go would consult `l.extend`, the POSIX TZ
        /// string a tzfile can carry (`:207-213`). A Windows `Location` has
        /// none, so the last transition simply holds, and the zone it points at
        /// is whichever the later-month rule switches to rather than the
        /// standard one.
        ///
        /// The search is Go's own loop rather than a slice helper, because the
        /// loop is what decides the answer if a transition table is ever not
        /// sorted, which a `TIME_ZONE_INFORMATION` naming both rules in one
        /// month would produce.
        fn offset_secs_at(&self, unix_secs: i64) -> i32 {
            let offset = match self.tx.first() {
                None => self.zone[0],
                Some(first) if unix_secs < first.when => self.zone[0],
                Some(_) => {
                    let mut lo = 0;
                    let mut hi = self.tx.len();
                    while hi - lo > 1 {
                        let mid = (lo + hi) / 2;
                        if unix_secs < self.tx[mid].when {
                            hi = mid;
                        } else {
                            lo = mid;
                        }
                    }
                    self.zone[self.tx[lo].index]
                }
            };
            // Unreachable for any zone an operating system can report, and a
            // fallback to UTC rather than a panic for one it cannot.
            i32::try_from(offset).unwrap_or(0)
        }
    }

    /// Go's `pseudoUnix` (`time/zoneinfo_windows.go:112-135`): the instant a
    /// "day in month" rule names in `year`, counted as though the local wall
    /// clock were UTC.
    ///
    /// Windows writes a daylight rule as a month, a weekday, a week within that
    /// month from 1 to 5 where 5 means the last one, and the wall time the
    /// change happens at. The caller subtracts the offset in effect before the
    /// change to turn the result into a real instant.
    ///
    /// Go's hour, minute and second go through `time.Date`, which normalises
    /// each into the unit above it (`time/time.go:1737-1741`); adding them as
    /// seconds to a linearly counted day, as below, is the same arithmetic
    /// written without the carries.
    fn pseudo_unix(year: i64, rule: &SYSTEMTIME) -> i64 {
        // Go builds `Date(year, Month(d.Month), 1, ...)` and reads the weekday
        // off it, and `time.Date` normalises a month outside 1 to 12 into the
        // year first (`time/time.go:1732-1735`).
        let (first_year, first_month) = normalize_month(year, i64::from(rule.wMonth));
        let first = days_from_civil(first_year, first_month, 1);
        // 1 January 1970 was a Thursday, which is 4 in Go's `Weekday`.
        let weekday = (first + 4).rem_euclid(7);

        let mut skew = i64::from(rule.wDayOfWeek) - weekday;
        if skew < 0 {
            skew += 7;
        }
        let mut day = 1 + skew;
        let week = i64::from(rule.wDay) - 1;
        if week < 4 {
            day += week * 7;
        } else {
            // The last instance of that weekday: take the fifth and step back
            // if it fell out of the month.
            day += 4 * 7;
            // Go asks `daysIn` with the rule's own month and the year it was
            // handed, neither of them normalised (`:130`).
            if day > days_in(i64::from(rule.wMonth), year) {
                day -= 7;
            }
        }

        (first + day - 1) * SECS_PER_DAY
            + i64::from(rule.wHour) * 3600
            + i64::from(rule.wMinute) * 60
            + i64::from(rule.wSecond)
    }

    /// `time.Date`'s month normalisation (`time/time.go:1732-1735`), which is
    /// `norm(year, month - 1, 12)` (`:1695-1707`) written as Rust's Euclidean
    /// division.
    fn normalize_month(year: i64, month: i64) -> (i64, u32) {
        let zero_based = month - 1;
        let normalized = zero_based.rem_euclid(12) + 1;
        (
            year + zero_based.div_euclid(12),
            u32::try_from(normalized).unwrap_or(1),
        )
    }

    /// Go's `daysIn` (`time/time.go:1285-1297`), bit trick and all: `m & 1`
    /// alternates the month lengths and `(m >> 3) & 1` inverts the alternation
    /// from August on.
    fn days_in(month: i64, year: i64) -> i64 {
        if month == 2 {
            if is_leap(year) { 29 } else { 28 }
        } else {
            30 + ((month + (month >> 3)) & 1)
        }
    }

    /// Go's `isLeap` (`time/time.go:1679-1689`), written as the rule rather
    /// than as the bit trick that stands in for it there.
    fn is_leap(year: i64) -> bool {
        year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
    }

    #[cfg(test)]
    mod tests {
        use super::{LocalZone, SECS_PER_DAY, SYSTEMTIME, TIME_ZONE_INFORMATION, days_from_civil};

        /// The year every zone below is built against, which is also the year
        /// the Go runs these expectations come from were taken in.
        const BASE_YEAR: i64 = 2026;

        /// One Windows daylight rule: the month, the weekday counting from
        /// Sunday, the week within the month from 1 to 5, and the wall time.
        const fn rule(month: u16, weekday: u16, week: u16, hour: u16, minute: u16) -> SYSTEMTIME {
            SYSTEMTIME {
                wYear: 0,
                wMonth: month,
                wDayOfWeek: weekday,
                wDay: week,
                wHour: hour,
                wMinute: minute,
                wSecond: 0,
                wMilliseconds: 0,
            }
        }

        /// The all-zero rule a zone with no daylight saving carries.
        const NO_RULE: SYSTEMTIME = rule(0, 0, 0, 0, 0);

        /// A `TIME_ZONE_INFORMATION` with the names left empty, which nothing
        /// here reads.
        const fn zone_info(
            bias: i32,
            standard: SYSTEMTIME,
            standard_bias: i32,
            daylight: SYSTEMTIME,
            daylight_bias: i32,
        ) -> TIME_ZONE_INFORMATION {
            TIME_ZONE_INFORMATION {
                Bias: bias,
                StandardName: [0; 32],
                StandardDate: standard,
                StandardBias: standard_bias,
                DaylightName: [0; 32],
                DaylightDate: daylight,
                DaylightBias: daylight_bias,
            }
        }

        /// Go's own `usPacific` fixture (`time/zoneinfo_windows.go:204-215`),
        /// which is also the `TIME_ZONE_INFORMATION` this machine reports.
        const US_PACIFIC: TIME_ZONE_INFORMATION =
            zone_info(8 * 60, rule(11, 0, 1, 2, 0), 0, rule(3, 0, 2, 2, 0), -60);

        /// Go's own `aus` fixture (`:217-228`). Its standard rule falls in the
        /// earlier month, so `initLocalFromTZI` does not swap the pair and the
        /// table ends in the daylight zone rather than the standard one.
        const AUS: TIME_ZONE_INFORMATION =
            zone_info(-10 * 60, rule(4, 0, 1, 3, 0), 0, rule(10, 0, 1, 2, 0), -60);

        /// The US Pacific rule moved to 02:45, with the autumn change on the
        /// last Sunday of October rather than the first of November, so the
        /// minute field and the "last instance" arm both decide an answer.
        const QUARTER_PAST: TIME_ZONE_INFORMATION =
            zone_info(8 * 60, rule(10, 0, 5, 3, 45), 0, rule(3, 0, 2, 2, 45), -60);

        /// A zone with no daylight rule, carrying a `StandardBias` Go discards
        /// (`:164-166`).
        const NO_DAYLIGHT: TIME_ZONE_INFORMATION = zone_info(7 * 60, NO_RULE, 99, NO_RULE, -60);

        /// One UTC instant as Unix seconds.
        fn utc(year: i64, month: u32, day: u32, hour: i64, minute: i64, second: i64) -> i64 {
            days_from_civil(year, month, day) * SECS_PER_DAY + hour * 3600 + minute * 60 + second
        }

        /// Asserts a whole table of instants against one zone.
        fn check(tzi: &TIME_ZONE_INFORMATION, cases: &[(&str, i64, i32)]) {
            let zone = LocalZone::from_tzi(tzi, BASE_YEAR);
            for (label, at, want) in cases {
                assert_eq!(zone.offset_secs_at(*at), *want, "{label}");
            }
        }

        /// This year's rule, applied to every year, which is the whole of Go's
        /// documented Windows bug (`time/zoneinfo_windows.go:17-20`).
        ///
        /// The two historical instants are where it shows. The United States
        /// moved the end of daylight saving from the last Sunday in October to
        /// the first in November in 2007, and Windows knows that: asked through
        /// `SystemTimeToTzSpecificLocalTime`, this machine answers -08:00 for
        /// both of them. Go applies the current rule to 2005 and to 1985 and
        /// answers -07:00, because 1 November is before the first Sunday in
        /// November in both years.
        #[test]
        fn this_years_daylight_rule_is_applied_to_every_year() {
            check(
                &US_PACIFIC,
                &[
                    (
                        "a second before the 2026 spring change",
                        utc(2026, 3, 8, 9, 59, 59),
                        -28800,
                    ),
                    ("the 2026 spring change", utc(2026, 3, 8, 10, 0, 0), -25200),
                    (
                        "a second before the 2026 autumn change",
                        utc(2026, 11, 1, 8, 59, 59),
                        -25200,
                    ),
                    ("the 2026 autumn change", utc(2026, 11, 1, 9, 0, 0), -28800),
                    (
                        "1 November 2005, which the pre-2007 rule had in standard time",
                        utc(2005, 11, 1, 19, 0, 0),
                        -25200,
                    ),
                    (
                        "1 November 1985, the same",
                        utc(1985, 11, 1, 19, 0, 0),
                        -25200,
                    ),
                    ("a January instant", utc(2026, 1, 15, 12, 0, 0), -28800),
                    ("a July instant", utc(2026, 7, 15, 12, 0, 0), -25200),
                    ("the epoch", 0, -28800),
                ],
            );
        }

        /// The table covers a hundred years each side of the year it was built
        /// in, and nothing outside it (`:185-201`, the loop bound at `:191`).
        ///
        /// Below the first transition Go's `lookupFirstZone` answers the
        /// standard zone, so July 1925 is -08:00 where July 1926 is -07:00.
        /// Above the last one that last transition holds, and for this zone it
        /// is the November one, so July 2126 is -08:00 where July 2125 is
        /// -07:00. Windows itself answers -07:00 for all four.
        #[test]
        fn the_transition_table_stops_a_hundred_years_either_side() {
            check(
                &US_PACIFIC,
                &[
                    (
                        "the first year of the window, in daylight saving",
                        utc(1926, 7, 1, 12, 0, 0),
                        -25200,
                    ),
                    (
                        "the year before the window",
                        utc(1925, 7, 1, 12, 0, 0),
                        -28800,
                    ),
                    (
                        "the last year of the window, in daylight saving",
                        utc(2125, 7, 1, 12, 0, 0),
                        -25200,
                    ),
                    (
                        "the year after the window",
                        utc(2126, 7, 1, 12, 0, 0),
                        -28800,
                    ),
                    ("far below the window", utc(1800, 7, 1, 12, 0, 0), -28800),
                    ("far above the window", utc(2400, 7, 1, 12, 0, 0), -28800),
                ],
            );
        }

        /// A zone whose standard rule falls in the earlier month is not
        /// swapped, so its table ends in the daylight zone.
        ///
        /// That is what makes "outside the window" mean the standard offset
        /// only below the table. Above it the answer is whichever zone the
        /// later-month rule switches to, which here is daylight saving.
        #[test]
        fn a_southern_hemisphere_table_ends_in_daylight_saving() {
            check(
                &AUS,
                &[
                    (
                        "a January instant, in daylight saving",
                        utc(2026, 1, 15, 12, 0, 0),
                        39600,
                    ),
                    (
                        "a July instant, in standard time",
                        utc(2026, 7, 15, 12, 0, 0),
                        36000,
                    ),
                    (
                        "below the window, the standard zone",
                        utc(1925, 7, 1, 12, 0, 0),
                        36000,
                    ),
                    (
                        "above the window, where the last transition holds",
                        utc(2126, 7, 1, 12, 0, 0),
                        39600,
                    ),
                    ("far above the window", utc(2400, 7, 1, 12, 0, 0), 39600),
                ],
            );
        }

        /// The minute of the rule and its "last instance of the weekday" arm
        /// both decide an answer.
        ///
        /// `US_PACIFIC` changes on the hour, in the first and second weeks of
        /// its months, so neither the minute field nor the `week == 5` arm of
        /// `pseudoUnix` can be seen through it. This zone changes at 02:45 and
        /// on the last Sunday of October, which in 2026 is the 25th.
        #[test]
        fn the_rule_minute_and_the_last_week_arm_place_the_transition() {
            check(
                &QUARTER_PAST,
                &[
                    (
                        "a minute before the spring change",
                        utc(2026, 3, 8, 10, 44, 59),
                        -28800,
                    ),
                    (
                        "the spring change, at 02:45 local",
                        utc(2026, 3, 8, 10, 45, 0),
                        -25200,
                    ),
                    (
                        "a minute before the autumn change",
                        utc(2026, 10, 25, 10, 44, 59),
                        -25200,
                    ),
                    (
                        "the autumn change, on the last Sunday of October",
                        utc(2026, 10, 25, 10, 45, 0),
                        -28800,
                    ),
                    (
                        "the first Sunday of November, already standard",
                        utc(2026, 11, 1, 9, 0, 0),
                        -28800,
                    ),
                ],
            );
        }

        /// A zone with no daylight rule is `-Bias` for all time, and its
        /// `StandardBias` is discarded.
        ///
        /// Go returns from `initLocalFromTZI` at `:161`, before it reads
        /// `StandardBias` at `:167`, and says why in the comment at `:164-166`.
        /// A port that added the bias anyway would put this zone an hour and
        /// thirty nine minutes out.
        #[test]
        fn a_zone_with_no_daylight_rule_discards_its_standard_bias() {
            check(
                &NO_DAYLIGHT,
                &[
                    ("a January instant", utc(2026, 1, 15, 12, 0, 0), -25200),
                    ("a July instant", utc(2026, 7, 15, 12, 0, 0), -25200),
                    ("below any window", utc(1800, 7, 1, 12, 0, 0), -25200),
                    ("above any window", utc(2400, 7, 1, 12, 0, 0), -25200),
                    ("the epoch", 0, -25200),
                ],
            );
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::sync::Once;

    // `tzset` from the C library. The pinned `libc` crate (0.2.189) declares
    // it only for Windows (`libc/src/windows/mod.rs:454`), so the Unix arm
    // binds it here itself; every libc this crate can link on Unix exports
    // it, since POSIX requires it. A plain comment, because rustdoc does not
    // document extern blocks and a doc comment here is a lint error.
    unsafe extern "C" {
        fn tzset();
    }

    /// `localtime_r`'s `tm_gmtoff`, which is the offset the timezone database
    /// gives for that instant.
    ///
    /// Go does not call libc here. It reads the same zone files itself, with
    /// its own tzfile parser, from the directories listed at
    /// `time/zoneinfo_unix.go:21-26` and through `initLocal` at
    /// `time/zoneinfo_unix.go:28`. So the two read one database through two
    /// readers: they agree on a host whose libc reads those same files, which
    /// is every glibc host, but an unusual `TZ` value or a musl build can part
    /// them. The Windows arm above rebuilds what Go itself builds, out of the
    /// call Go itself makes, and is the platform this server runs on, so this
    /// arm is the one a Linux cutover would have to re-check.
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
        TZSET.call_once(|| unsafe { tzset() });

        // `time_t` is 64 bits wide on the targets this server builds for and
        // 32 bits on some others, so the conversion is infallible on the
        // former and the lint would otherwise fire there.
        #[allow(irrefutable_let_patterns)]
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
