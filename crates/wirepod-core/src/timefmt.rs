//! Go's time formatting and its one calendar arithmetic, ported.
//!
//! Three Go behaviors reach the wire or the disk and have to be reproduced
//! byte for byte. `time.RFC3339Nano` is what the token server writes into the
//! `iat` and `expires` claims (`token.go:29`, `token.go:195-196`), and it
//! trims trailing zeros from the fraction and drops the decimal point when
//! there is no fraction at all, which no other RFC 3339 spelling does.
//! `2006.01.02 15:04:05` is the stamp on every logger line (`logger.go:103`,
//! `logger.go:114`). `AddDate(0, 1, 0)` is the one month the `expires` claim
//! sits ahead of `iat` (`token.go:196`), and Go normalizes rather than clamps,
//! so 31 January plus a month is 3 March.
//!
//! No date crate does this. Go's own rules are the contract, `AddDate` has to
//! resolve its result against the zone rather than inherit the offset it
//! started from, and the workspace resolves no calendar dependency, so the
//! civil date conversion below is the standard days-from-civil algorithm
//! (Howard Hinnant, *chrono-Compatible Low-Level Date Algorithms*), which is
//! the same shape Go's own `dateToAbsDays` uses.
//!
//! Every function here is pinned by the `rfc3339`, `addmonth`,
//! `addmonth_local` and `legacystamp` sections of
//! `docs/phases/P1-robot-connect-auth/go-probe/expected.txt`, which is the
//! recorded stdout of the Go probe program committed beside it.

use crate::wallclock::{WallClock, WallTime};

/// Seconds in a day, which is exact because these are Unix seconds and Unix
/// seconds do not carry leap seconds.
pub(crate) const SECS_PER_DAY: i64 = 86_400;

/// The days from the epoch to 1 January 1970 in the civil algorithm's own era
/// numbering, which starts at 1 March 0000.
const EPOCH_SHIFT: i64 = 719_468;

/// Days in a 400 year era, the cycle the Gregorian calendar repeats on.
const DAYS_PER_ERA: i64 = 146_097;

/// A civil, that is a calendar, date.
///
/// Year is astronomical: 1 BC is year 0 and 2 BC is year -1, which is what the
/// days-from-civil algorithm and Go's `time.Date` both count in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CivilDate {
    /// The year, negative before 1 AD.
    pub year: i64,
    /// The month, 1 through 12.
    pub month: u32,
    /// The day of the month, 1 through 31.
    pub day: u32,
}

/// The number of days from 1 January 1970 to `year-month-day`, negative before
/// it.
///
/// `month` must be 1 through 12. `day` is **not** clamped to the length of the
/// month: it is counted forward linearly, so 31 February 2023 is 3 March 2023.
/// That is not a convenience, it is the contract. Go's `time.Date` counts the
/// day the same way (`time/time.go`, `dateToAbsDays`), which is the whole
/// reason `AddDate(0, 1, 0)` rolls a month end forward instead of clamping it.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let month = i64::from(month);
    let day = i64::from(day);
    // March starts the algorithm's year, so that the leap day lands at its end
    // and never has to be counted around.
    let year = year - i64::from(month <= 2);
    let era = (if year >= 0 { year } else { year - 399 }) / 400;
    let year_of_era = year - era * 400;
    let shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * DAYS_PER_ERA + day_of_era - EPOCH_SHIFT
}

/// The civil date `days` days after 1 January 1970, the inverse of
/// [`days_from_civil`] for an in-range day.
pub fn civil_from_days(days: i64) -> CivilDate {
    let shifted = days + EPOCH_SHIFT;
    let era = (if shifted >= 0 {
        shifted
    } else {
        shifted - (DAYS_PER_ERA - 1)
    }) / DAYS_PER_ERA;
    let day_of_era = shifted - era * DAYS_PER_ERA;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    CivilDate {
        year: year + i64::from(month <= 2),
        month: u32::try_from(month).unwrap_or(1),
        day: u32::try_from(day).unwrap_or(1),
    }
}

/// An instant split into the calendar fields of one zone, which is what every
/// Go layout is written out of.
#[derive(Clone, Copy, Debug)]
struct Broken {
    date: CivilDate,
    hour: i64,
    minute: i64,
    second: i64,
}

/// Splits an instant into the calendar fields of the zone `utc_offset_secs`
/// describes.
fn break_down(unix_secs: i64, utc_offset_secs: i32) -> Broken {
    let local = unix_secs + i64::from(utc_offset_secs);
    let seconds = local.rem_euclid(SECS_PER_DAY);
    Broken {
        date: civil_from_days(local.div_euclid(SECS_PER_DAY)),
        hour: seconds / 3600,
        minute: (seconds / 60) % 60,
        second: seconds % 60,
    }
}

/// Go's `appendInt` (`format.go:418-446`): a minus sign if negative, then the
/// digits zero padded to `width`.
///
/// The cited range is the whole function. Go splits it into a width-2 and a
/// width-4 fast path at `:427-432` and a general arm whose padding loop is at
/// `:443-446`, and all three write the same bytes, so one loop covers them
/// here.
///
/// Rust's own `{:04}` counts the sign inside the width, so it writes `-001`
/// where Go writes `-0001`. Only a year before 1 AD can tell the two apart,
/// which nothing in the server produces, but the layouts are a contract and
/// this is what the contract says.
fn push_int(out: &mut String, value: i64, width: usize) {
    if value < 0 {
        out.push('-');
    }
    let digits = value.unsigned_abs().to_string();
    for _ in digits.len()..width {
        out.push('0');
    }
    out.push_str(&digits);
}

/// Writes `date` and the time of day as `YYYY-MM-DDTHH:MM:SS`, the part every
/// RFC 3339 spelling shares.
fn push_date_and_time(out: &mut String, broken: &Broken) {
    push_int(out, broken.date.year, 4);
    out.push('-');
    push_int(out, i64::from(broken.date.month), 2);
    out.push('-');
    push_int(out, i64::from(broken.date.day), 2);
    out.push('T');
    push_int(out, broken.hour, 2);
    out.push(':');
    push_int(out, broken.minute, 2);
    out.push(':');
    push_int(out, broken.second, 2);
}

/// Writes the fraction the way Go's `.999999999` verb does: trailing zeros
/// trimmed, and nothing at all, not even the point, when every digit is zero.
///
/// This is the difference between `RFC3339Nano` and `RFC3339`, and it is
/// visible on the wire: `iat` is a whole second often enough that a decimal
/// point that should not be there would show up in the first token the robot
/// is handed.
fn push_fraction(out: &mut String, nanos: u32) {
    if nanos == 0 {
        return;
    }
    let mut digits = format!("{nanos:09}");
    while digits.ends_with('0') {
        digits.pop();
    }
    out.push('.');
    out.push_str(&digits);
}

/// Writes the zone the way Go's `Z07:00` verb does.
///
/// `Z` for UTC, and otherwise a sign and `HH:MM`. Two quirks are Go's and are
/// kept: the offset is truncated to whole minutes, so a zone with seconds in
/// its offset loses them, and the sign comes from that minute count, so an
/// offset smaller than a minute but not zero is written `+00:00`. Both follow
/// from `zone := offset / 60` preceding `if zone < 0` at
/// `format_rfc3339.go:49-58`. The probe records only whole-minute offsets, so
/// these two are pinned by hand-written cases derived from that source rather
/// than from the recording.
fn push_zone(out: &mut String, utc_offset_secs: i32) {
    if utc_offset_secs == 0 {
        out.push('Z');
        return;
    }
    let minutes = utc_offset_secs / 60;
    out.push(if minutes < 0 { '-' } else { '+' });
    let minutes = i64::from(minutes.unsigned_abs());
    push_int(out, minutes / 60, 2);
    out.push(':');
    push_int(out, minutes % 60, 2);
}

/// Formats `at` in the zone `utc_offset_secs` describes, the way Go's
/// `time.RFC3339Nano` layout does.
///
/// That layout is `2006-01-02T15:04:05.999999999Z07:00`: the fraction loses
/// its trailing zeros and disappears entirely when the nanosecond field is
/// zero, and the zone is `Z` at UTC and a signed `HH:MM` everywhere else. The
/// token server formats both of its claims with it (`token.go:29`), in
/// `time.Local`, so the offset a caller passes here is the one
/// [`WallClock::utc_offset_secs_at`] resolved for that instant and not a
/// constant.
pub fn rfc3339_nano(at: WallTime, utc_offset_secs: i32) -> String {
    let broken = break_down(at.unix_secs, utc_offset_secs);
    // `2006-01-02T15:04:05.999999999+07:00` is 35 bytes, the longest an
    // in-range year produces.
    let mut out = String::with_capacity(35);
    push_date_and_time(&mut out, &broken);
    push_fraction(&mut out, at.nanos);
    push_zone(&mut out, utc_offset_secs);
    out
}

/// Formats `at` in the zone `utc_offset_secs` describes, the way Go's
/// `2006.01.02 15:04:05` layout does.
///
/// This is the stamp the logger puts at the front of every line, both in the
/// legacy form the tray reads (`logger.go:103`) and in the file form
/// (`logger.go:114`). There is no fraction and no zone in the layout, so a
/// line says nothing about which offset it was written in, and two lines a
/// daylight saving transition apart can read as out of order. That is Go's
/// behavior and the log files on disk already have it.
pub fn legacy_stamp(at: WallTime, utc_offset_secs: i32) -> String {
    let broken = break_down(at.unix_secs, utc_offset_secs);
    let mut out = String::with_capacity(19);
    push_int(&mut out, broken.date.year, 4);
    out.push('.');
    push_int(&mut out, i64::from(broken.date.month), 2);
    out.push('.');
    push_int(&mut out, i64::from(broken.date.day), 2);
    out.push(' ');
    push_int(&mut out, broken.hour, 2);
    out.push(':');
    push_int(&mut out, broken.minute, 2);
    out.push(':');
    push_int(&mut out, broken.second, 2);
    out
}

/// One calendar month after `at`, the way Go's `AddDate(0, 1, 0)` does it.
///
/// Go reads the calendar fields of `at` in its own zone, adds one to the
/// month, and hands the result back to `time.Date`, which normalizes rather
/// than clamps. A day past the end of the shorter target month counts forward
/// into the month after it, so 31 January is 3 March in a common year and 2
/// March in a leap year, and 31 August is 1 October. This is the arithmetic
/// behind the token server's `expires` claim (`token.go:196`), so the dates
/// the robot is handed carry it.
///
/// The result is resolved against `clock` rather than against the offset `at`
/// was in, which is the reason this takes a clock at all. `time.Date` looks
/// the zone up at the wall time read as if it were UTC, and then again at the
/// instant that first offset implies, keeping the second answer
/// (`time/time.go:1752-1760`). Two things fall out of that and both are
/// recorded in the probe's `addmonth_local` section: a target on the far side
/// of a daylight saving transition carries the offset in effect there, and a
/// target wall time that does not exist, because the clocks jumped over it,
/// lands one gap earlier rather than one gap later.
///
/// Go guards the pair with `if offset != 0`, which is a no-op: a zero first
/// offset leaves the candidate instant equal to the wall value the lookup
/// already placed inside its own interval, so the unconditional two-step below
/// is equivalent rather than merely close.
///
/// The first lookup has to happen at the target wall time, and the sign of the
/// offset is why. `wall` below is the target civil time read as if it were
/// UTC, so it sits `offset` seconds *after* the instant it names in a zone
/// east of Greenwich and that far *before* it in a zone west of Greenwich.
/// East of Greenwich a target inside the hour a fall back repeats therefore
/// puts `wall` on the far side of the transition while the input instant, a
/// month earlier, is still on the near side, and the two candidate instants
/// `wall - guess` then straddle the transition and resolve different offsets.
/// West of Greenwich `wall` leans the same way the earlier input does and both
/// candidates land on the same side, which is why `America/Los_Angeles` cannot
/// separate the two orders however the cases are chosen.
///
/// The probe's `claims_matrix` case `paris_fall_back` is the one that fixes
/// it: 02:00 on 25 October 2026 in `Europe/Paris` is inside the repeated hour,
/// and `AddDate` answers 1792890000 at `+01:00` where a first lookup taken at
/// the input instant answers 1792886400 at `+02:00`. Driven against Go over
/// every hour of 2026 and 2027 in all 597 zones the tzdata release embeds, the
/// order written here matched `AddDate` in every case, 103 of those zones
/// separated the two orders somewhere in that span, and every separation had a
/// positive offset at the input instant.
pub fn add_months(at: WallTime, clock: &dyn WallClock) -> WallTime {
    let broken = break_down(at.unix_secs, clock.utc_offset_secs_at(at.unix_secs));
    let (year, month) = if broken.date.month == 12 {
        (broken.date.year + 1, 1)
    } else {
        (broken.date.year, broken.date.month + 1)
    };
    let wall = days_from_civil(year, month, broken.date.day) * SECS_PER_DAY
        + broken.hour * 3600
        + broken.minute * 60
        + broken.second;
    let guess = clock.utc_offset_secs_at(wall);
    let offset = clock.utc_offset_secs_at(wall - i64::from(guess));
    WallTime::new(wall - i64::from(offset), at.nanos)
}
