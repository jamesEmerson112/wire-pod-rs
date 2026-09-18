//! Go's time formatting and calendar arithmetic, table driven from the
//! recorded probe output.
//!
//! `docs/phases/P1-robot-connect-auth/go-probe/expected.txt` is the stdout of
//! the Go program committed beside it, so every expectation below was printed
//! by Go rather than written by hand. Each line is
//! `section\tinput\toutput`, the input column is space separated `key=value`
//! pairs whose first pair is `kind=`, and the output column is always a Go
//! `%q` quoted string literal.
//!
//! A section or a kind this file does not recognize fails
//! [`the_probe_file_holds_no_unrecognized_case`], so a probe line cannot be
//! silently skipped: the sections belonging to other commits are listed there
//! by name, and anything else is an error.

use std::collections::HashMap;
use std::fmt::Debug;
use std::str::FromStr;

use wirepod_core::timefmt::{
    CivilDate, add_months, civil_from_days, days_from_civil, legacy_stamp, rfc3339_nano,
};
use wirepod_core::wallclock::{FixedWallClock, WallClock, WallTime};

const EXPECTED: &str =
    include_str!("../../../docs/phases/P1-robot-connect-auth/go-probe/expected.txt");

/// Seconds in a day, for building an instant out of a recorded civil date.
const SECS_PER_DAY: i64 = 86_400;

/// The `legacystamp` kinds this file owns, and how many lines the recording
/// holds of each.
///
/// `timefmt` owns the stamp itself and the layout constant that produces it,
/// which is all this crate's formatter does with that section.
const TIMEFMT_STAMP_KINDS: [(&str, usize); 2] = [("stamp", 7), ("const", 1)];

/// The `legacystamp` kinds the logger owns, and how many lines the recording
/// holds of each.
///
/// `legacy_line` and `file_line` are Go's whole-line layouts (`logger.go:102`,
/// `logger.go:113`) and `level_string` is `Level.String`. None of them is a
/// time format: the logger takes an already formatted stamp, so
/// `crates/wirepod-core/tests/logger.rs` asserts these against these same
/// lines and this file asserts only that they are still there and still this
/// many. Between the two counts, a line added to either half fails a suite
/// rather than falling into the gap between them.
const LOGGER_STAMP_KINDS: [(&str, usize); 3] =
    [("legacy_line", 7), ("file_line", 7), ("level_string", 5)];

/// One recorded case: its section, its input pairs, and its unquoted output.
struct Case {
    /// The line number in the recording, so a failure names the line.
    number: usize,
    section: &'static str,
    inputs: Vec<(&'static str, &'static str)>,
    output: String,
}

impl Case {
    /// The `kind=` pair, which every line carries first.
    fn kind(&self) -> &str {
        self.inputs
            .first()
            .filter(|(key, _)| *key == "kind")
            .map(|(_, value)| *value)
            .unwrap_or_else(|| {
                panic!(
                    "line {}: the input column does not start with kind=",
                    self.number
                )
            })
    }

    /// The value of one input key.
    fn get(&self, key: &str) -> &'static str {
        self.inputs
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
            .unwrap_or_else(|| panic!("line {}: no {key}= pair", self.number))
    }

    /// The value of one input key, parsed as a number.
    fn num<T: FromStr>(&self, key: &str) -> T
    where
        T::Err: Debug,
    {
        self.get(key).parse().unwrap_or_else(|error| {
            panic!("line {}: {key} is not a number: {error:?}", self.number)
        })
    }

    /// The output column, parsed as a number. The recording quotes even its
    /// plain numbers, so this is the unquoted text.
    fn output_num<T: FromStr>(&self) -> T
    where
        T::Err: Debug,
    {
        self.output.parse().unwrap_or_else(|error| {
            panic!(
                "line {}: the output is not a number: {error:?}",
                self.number
            )
        })
    }

    /// The instant a case describes with civil fields plus an offset.
    ///
    /// Only the sections whose zone has one offset for all time can be read
    /// this way. In a real zone a wall time can be missing or repeated, which
    /// is exactly what the `addmonth_local` section is for, and those cases
    /// carry the instant itself instead.
    fn instant_from_civil_fields(&self, utc_offset_secs: i32) -> WallTime {
        let secs = days_from_civil(self.num("y"), self.num("mo"), self.num("d")) * SECS_PER_DAY
            + self.num::<i64>("h") * 3600
            + self.num::<i64>("mi") * 60
            + self.num::<i64>("s")
            - i64::from(utc_offset_secs);
        WallTime::new(secs, self.num("ns"))
    }
}

/// Undoes Go's `%q` quoting.
///
/// Only the five escapes the recording can contain are accepted. Anything else
/// panics rather than being passed through, so a probe change that introduces
/// a new escape cannot be mis-parsed into a passing test.
fn unquote(field: &str, number: usize) -> String {
    let body = field
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| {
            panic!("line {number}: the output column is not a Go %q literal: {field}")
        });
    let mut out = String::with_capacity(body.len());
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            other => panic!("line {number}: unsupported escape {other:?} in {field}"),
        }
    }
    out
}

/// Every case in the recording, comments dropped.
fn recorded_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (index, raw) in EXPECTED.lines().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let number = index + 1;
        let mut columns = line.split('\t');
        let section = columns
            .next()
            .unwrap_or_else(|| panic!("line {number}: missing the section column"));
        let input = columns
            .next()
            .unwrap_or_else(|| panic!("line {number}: missing the input column"));
        let output = columns
            .next()
            .unwrap_or_else(|| panic!("line {number}: missing the output column"));
        assert!(
            columns.next().is_none(),
            "line {number}: more than three columns"
        );
        cases.push(Case {
            number,
            section,
            inputs: input
                .split(' ')
                .map(|pair| {
                    pair.split_once('=')
                        .unwrap_or_else(|| panic!("line {number}: {pair} is not a key=value pair"))
                })
                .collect(),
            output: unquote(output, number),
        });
    }
    assert!(!cases.is_empty(), "the recording holds no cases");
    cases
}

/// The cases of one section, in the order the probe wrote them.
fn section<'a>(cases: &'a [Case], name: &str) -> Vec<&'a Case> {
    let selected: Vec<&Case> = cases.iter().filter(|case| case.section == name).collect();
    assert!(!selected.is_empty(), "the recording holds no {name} cases");
    selected
}

/// A zone rebuilt from the offsets the `addmonth_local` section recorded, and
/// from nothing else.
///
/// The probe recorded six offsets around three `America/Los_Angeles`
/// transitions, at the last second of one offset and the first second of the
/// next. That is enough to rebuild the step function over the span its cases
/// cover, so the test needs no timezone database of its own and hand-writes no
/// offset: a transition is derived wherever two consecutive samples disagree.
struct RecordedZone {
    /// The offset before the first recorded transition.
    base: i32,
    /// `(instant, offset)`, ascending, one per derived transition.
    transitions: Vec<(i64, i32)>,
}

impl RecordedZone {
    fn from_samples(samples: &[(i64, i32)]) -> Self {
        assert!(
            samples.len() >= 2,
            "a zone cannot be derived from fewer than two recorded offsets"
        );
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let base = sorted[0].1;
        let mut transitions = Vec::new();
        let mut previous = base;
        for &(unix_secs, offset) in &sorted {
            if offset != previous {
                transitions.push((unix_secs, offset));
                previous = offset;
            }
        }
        assert!(
            !transitions.is_empty(),
            "the recorded offsets never change, so no transition can be derived"
        );
        Self { base, transitions }
    }
}

impl WallClock for RecordedZone {
    fn now(&self) -> WallTime {
        unreachable!("the recorded zone is asked for offsets only, never for the current time")
    }

    fn utc_offset_secs_at(&self, unix_secs: i64) -> i32 {
        let mut offset = self.base;
        for &(at, next) in &self.transitions {
            if unix_secs >= at {
                offset = next;
            }
        }
        offset
    }
}

#[test]
fn rfc3339_nano_matches_the_recorded_probe() {
    let cases = recorded_cases();
    for case in section(&cases, "rfc3339") {
        assert_eq!(case.kind(), "format", "line {}", case.number);
        let at = WallTime::new(case.num("unix"), case.num("nsec"));
        let offset: i32 = case.num("off");
        assert_eq!(
            rfc3339_nano(at, offset),
            case.output,
            "line {}: {:?}",
            case.number,
            case.inputs
        );
    }
}

#[test]
fn add_months_matches_the_recorded_fixed_zone_probe() {
    let cases = recorded_cases();
    for case in section(&cases, "addmonth") {
        assert_eq!(case.kind(), "add_months", "line {}", case.number);
        let offset: i32 = case.num("off");
        let at = case.instant_from_civil_fields(offset);

        // The recorded `in=` string is what Go printed for the input, so this
        // also settles that the instant was rebuilt from the civil fields
        // correctly before anything is added to it.
        assert_eq!(
            rfc3339_nano(at, offset),
            case.get("in"),
            "line {}: the input instant",
            case.number
        );

        // A Go `time.FixedZone` has one offset for all time, which is what
        // this clock is.
        let clock = FixedWallClock::new(at, offset);
        let result = add_months(at, &clock);
        assert_eq!(
            rfc3339_nano(result, clock.utc_offset_secs_at(result.unix_secs)),
            case.output,
            "line {}: {:?}",
            case.number,
            case.inputs
        );
    }
}

/// The section that matters most, because the fixed-offset cases alone cannot
/// fail an implementation that carries the input's offset into the result.
///
/// Two rules only a real zone can show are recorded here and both are checked:
/// a target one month later carries the offset in effect *there*, and a target
/// wall time the clocks jumped over resolves backwards by the gap rather than
/// forwards. The `starts_in_missing_hour` case additionally pins that the
/// calendar fields are read back off the instant, because the fields the probe
/// asked Go for are not the fields Go ended up with.
#[test]
fn add_months_matches_the_recorded_local_zone_probe() {
    let cases = recorded_cases();
    let local = section(&cases, "addmonth_local");

    let zone_name = local
        .iter()
        .find(|case| case.kind() == "const" && case.get("name") == "zone")
        .unwrap_or_else(|| panic!("the section records no zone name"));
    assert_eq!(
        zone_name.output, "America/Los_Angeles",
        "line {}: the fake below is rebuilt from this zone's recorded offsets, \
         so a probe that switches zones has to be re-read rather than re-run",
        zone_name.number
    );

    let samples: Vec<(i64, i32)> = local
        .iter()
        .filter(|case| case.kind() == "offset")
        .map(|case| (case.num("unix"), case.output_num()))
        .collect();
    let zone = RecordedZone::from_samples(&samples);
    for case in local.iter().filter(|case| case.kind() == "offset") {
        assert_eq!(
            zone.utc_offset_secs_at(case.num("unix")),
            case.output_num::<i32>(),
            "line {}: the derived zone disagrees with the recorded offset",
            case.number
        );
    }

    let expected_unix: HashMap<&str, i64> = local
        .iter()
        .filter(|case| case.kind() == "out_unix")
        .map(|case| (case.get("case"), case.output_num()))
        .collect();
    let expected_offset: HashMap<&str, i32> = local
        .iter()
        .filter(|case| case.kind() == "out_off")
        .map(|case| (case.get("case"), case.output_num()))
        .collect();

    let mut checked = 0usize;
    for case in local
        .iter()
        .filter(|case| case.kind() == "add_months_local")
    {
        let name = case.get("case");
        let at = WallTime::new(case.num("in_unix"), case.num("ns"));
        let in_offset: i32 = case.num("in_off");
        assert_eq!(
            zone.utc_offset_secs_at(at.unix_secs),
            in_offset,
            "line {}: the derived zone disagrees with the recorded input offset",
            case.number
        );
        assert_eq!(
            rfc3339_nano(at, in_offset),
            case.get("in"),
            "line {}: the input instant",
            case.number
        );

        let result = add_months(at, &zone);
        assert_eq!(
            result.unix_secs, expected_unix[name],
            "line {}: {name} landed on the wrong instant",
            case.number
        );
        let offset = zone.utc_offset_secs_at(result.unix_secs);
        assert_eq!(
            offset, expected_offset[name],
            "line {}: {name} resolved the wrong offset",
            case.number
        );
        assert_eq!(
            rfc3339_nano(result, offset),
            case.output,
            "line {}: {name}",
            case.number
        );
        checked += 1;
    }
    assert_eq!(
        checked,
        expected_unix.len(),
        "every recorded output belongs to a case that ran"
    );
}

/// The `legacystamp` section is split between this file and the logger's, and
/// the split is asserted rather than assumed.
///
/// Both halves are counted here: the kinds this file formats, and the kinds it
/// deliberately leaves alone. A new kind fails the final arm, and a new line of
/// an existing kind fails its count, so a probe change cannot land in the gap
/// between the two suites with neither of them noticing.
#[test]
fn the_legacystamp_section_is_split_between_this_file_and_the_logger() {
    let cases = recorded_cases();
    let mut counted: HashMap<&str, usize> = HashMap::new();
    for case in section(&cases, "legacystamp") {
        *counted.entry(case.kind()).or_default() += 1;
    }
    for (kind, lines) in TIMEFMT_STAMP_KINDS.iter().chain(LOGGER_STAMP_KINDS.iter()) {
        assert_eq!(
            counted.remove(kind).unwrap_or(0),
            *lines,
            "the recording no longer holds {lines} legacystamp {kind} lines"
        );
    }
    assert!(
        counted.is_empty(),
        "the legacystamp section grew kinds neither this file nor the logger claims: {:?}",
        counted.keys().collect::<Vec<_>>()
    );
}

#[test]
fn legacy_stamp_matches_the_recorded_probe() {
    let cases = recorded_cases();
    let mut stamps = 0usize;
    let mut layouts = 0usize;
    for case in section(&cases, "legacystamp") {
        match case.kind() {
            "stamp" => {
                let offset: i32 = case.num("off");
                assert_eq!(
                    legacy_stamp(case.instant_from_civil_fields(offset), offset),
                    case.output,
                    "line {}: {:?}",
                    case.number,
                    case.inputs
                );
                stamps += 1;
            }
            "const" => {
                assert_eq!(case.get("name"), "stamp_layout", "line {}", case.number);
                // A Go layout is the reference time, 15:04:05 on 2 January
                // 2006 in MST, written in the layout's own shape. So
                // formatting that instant has to reproduce the layout string
                // itself, byte for byte.
                let mountain_standard = -7 * 3600;
                let reference = WallTime::new(
                    days_from_civil(2006, 1, 2) * SECS_PER_DAY + 15 * 3600 + 4 * 60 + 5
                        - i64::from(mountain_standard),
                    0,
                );
                assert_eq!(
                    legacy_stamp(reference, mountain_standard),
                    case.output,
                    "line {}: the reference time does not write itself as the layout",
                    case.number
                );
                layouts += 1;
            }
            // The whole-line layouts and the level names are the logger's, and
            // `tests/logger.rs` asserts them against these same lines. Their
            // counts are asserted above rather than here.
            kind if LOGGER_STAMP_KINDS.iter().any(|(name, _)| *name == kind) => {}
            other => panic!("line {}: unknown legacystamp kind {other}", case.number),
        }
    }
    // Every line of the two kinds this file owns ran, counted against the
    // recording rather than against a lower bound.
    assert_eq!(
        [("stamp", stamps), ("const", layouts)],
        TIMEFMT_STAMP_KINDS,
        "the stamp and layout cases this file formats are no longer all of them"
    );
}

/// The civil date pair is hand-rolled, so it is pinned by its own algebra
/// rather than by the handful of dates the probe happens to carry.
///
/// Every formatter here and the Windows zone table in
/// [`wirepod_core::wallclock`] are built on these two functions, but the
/// recording only ever reaches dates between 1999 and 2100, which leaves the
/// century rule and the 400 year era boundary untested. Walking the round trip
/// over roughly 400 BC to 3065 AD covers six era boundaries and every century
/// inside them, and hand-writes no expected value: the property is that the two
/// functions are inverses.
///
/// The lower end is not a round number. `civil_from_days` corrects the era
/// division for a negative day by subtracting `DAYS_PER_ERA - 1` before
/// dividing, and subtracting `DAYS_PER_ERA` instead gives the same answer
/// everywhere except on a day that is an exact negative multiple of 146097.
/// The first of those below the epoch is day -865565, 1 March of the proleptic
/// year -400, so the walk starts there. Year -400 is not a date anything
/// formats; it is the nearest day on which that correction decides an answer.
#[test]
fn the_civil_date_conversion_round_trips_over_two_millennia() {
    for days in -865_565..=400_000 {
        let date = civil_from_days(days);
        assert!(
            (1..=12).contains(&date.month),
            "day {days} decoded to month {}",
            date.month
        );
        assert!(
            (1..=31).contains(&date.day),
            "day {days} decoded to day {}",
            date.day
        );
        assert_eq!(
            days_from_civil(date.year, date.month, date.day),
            days,
            "day {days} decoded to {date:?}, which encodes to a different day"
        );
    }

    // The anchors the round trip alone cannot name: the epoch itself, the leap
    // day at the end of a 400 year era, the century that is not a leap year,
    // the day after the February before it, the two neighbouring era boundaries
    // the window above reaches, and the era boundary it starts on.
    for (year, month, day) in [
        (1970, 1, 1),
        (2000, 2, 29),
        (2100, 2, 28),
        (1900, 3, 1),
        (1600, 2, 29),
        (2400, 2, 29),
        (-400, 3, 1),
    ] {
        let days = days_from_civil(year, month, day);
        assert_eq!(
            civil_from_days(days),
            CivilDate { year, month, day },
            "{year}-{month}-{day} does not survive the round trip"
        );
    }
    assert_eq!(days_from_civil(1970, 1, 1), 0, "the epoch is day zero");
    assert_eq!(
        days_from_civil(-400, 3, 1),
        -865_565,
        "the era correction is only visible on an exact negative multiple of \
         146097, and this is the first one below the epoch"
    );
}

/// The two zone-writing quirks the recording cannot reach, taken from Go's
/// source rather than from the probe.
///
/// `format_rfc3339.go:49-58` computes `zone := offset / 60` and only then asks
/// `if zone < 0`, so an offset is truncated toward zero to whole minutes and
/// the sign comes from the truncated count. Every offset the probe records is
/// a whole number of minutes, which cannot tell that apart from taking the
/// sign from the raw seconds. These three are written by hand because the
/// recording holds nothing like them: a sub-minute offset, the historical
/// `America/Los_Angeles` LMT offset of -28378 seconds that a tzdata build
/// returns for an instant before 1883, and a half-hour zone for the positive
/// side.
#[test]
fn the_zone_writer_truncates_to_minutes_the_way_go_does() {
    assert_eq!(
        rfc3339_nano(WallTime::new(0, 0), -30),
        "1969-12-31T23:59:30+00:00",
        "an offset smaller than a minute truncates to zero minutes, and the \
         sign follows the truncated count rather than the seconds"
    );
    assert_eq!(
        rfc3339_nano(WallTime::new(0, 0), -28_378),
        "1969-12-31T16:07:02-07:52",
        "the seconds in the offset are dropped from the zone but not from the \
         wall time it shifts"
    );
    assert_eq!(
        rfc3339_nano(WallTime::new(0, 0), 34_200),
        "1970-01-01T09:30:00+09:30",
        "a half hour zone writes its minutes"
    );
    assert_eq!(
        rfc3339_nano(WallTime::new(0, 0), 0),
        "1970-01-01T00:00:00Z",
        "a zero offset is Z and never +00:00"
    );
}

/// Go's `appendInt` puts the minus sign outside the width, which only a year
/// before 1 AD can show.
///
/// `format.go:418-446` appends the sign first and pads the digits of the
/// absolute value to the full width afterwards, so year -1 is `-0001` where
/// Rust's own `{:04}` would give `-001`. Nothing the server formats produces
/// such a year, which is exactly why the rule would otherwise ship on the
/// strength of a comment alone.
#[test]
fn a_year_before_the_common_era_pads_the_way_go_does() {
    let at_midnight = |year: i64| WallTime::new(days_from_civil(year, 1, 1) * SECS_PER_DAY, 0);
    assert_eq!(
        rfc3339_nano(at_midnight(0), 0),
        "0000-01-01T00:00:00Z",
        "year zero fills the width with zeros"
    );
    assert_eq!(
        rfc3339_nano(at_midnight(-1), 0),
        "-0001-01-01T00:00:00Z",
        "the sign sits outside the four digit width"
    );
    assert_eq!(
        rfc3339_nano(at_midnight(-12_345), 0),
        "-12345-01-01T00:00:00Z",
        "a year wider than the width is written whole, with no padding"
    );
}

/// The system clock has to read the system clock.
///
/// Everything else in this file runs on a fixed or a recorded clock, so a
/// [`SystemWallClock`] that returned a constant would pass the whole suite
/// while stamping the same `iat` into every token the robot is handed. The
/// bracket is the two readings taken around the call, widened by two seconds
/// on each side so a slow machine cannot fail it, which is still far tighter
/// than any constant a wrong implementation would return.
#[test]
fn the_system_wall_clock_reads_the_system_clock() {
    use std::time::{SystemTime, UNIX_EPOCH};
    use wirepod_core::wallclock::SystemWallClock;

    let epoch_secs = || {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("this machine's clock is set after the epoch");
        i64::try_from(since.as_secs()).expect("the epoch offset fits in an i64")
    };

    let before = epoch_secs();
    let reading = SystemWallClock::new().now();
    let after = epoch_secs();

    assert!(
        reading.nanos < 1_000_000_000,
        "the nanosecond field has to stay inside its second, but it is {}",
        reading.nanos
    );
    assert!(
        (before - 2..=after + 2).contains(&reading.unix_secs),
        "the clock read {}, which is outside the {before} to {after} the call \
         was bracketed by",
        reading.unix_secs
    );
}

/// A fixed clock has to answer the same thing every time it is asked, which is
/// what lets a token test assert a whole JWT payload.
#[test]
fn the_fixed_wall_clock_is_deterministic() {
    let at = WallTime::new(1_700_000_000, 123_456_789);
    let offset = -7 * 3600;
    let clock = FixedWallClock::new(at, offset);

    assert_eq!(clock.now(), at);
    assert_eq!(clock.now(), clock.now());

    // A fixed zone has one offset for all time, including at instants nothing
    // would ever ask about.
    for probe in [i64::MIN, -1, 0, at.unix_secs, i64::MAX] {
        assert_eq!(clock.utc_offset_secs_at(probe), offset);
    }

    let formatted = rfc3339_nano(clock.now(), clock.utc_offset_secs_at(clock.now().unix_secs));
    let expires = add_months(clock.now(), &clock);
    for _ in 0..3 {
        assert_eq!(
            rfc3339_nano(clock.now(), clock.utc_offset_secs_at(clock.now().unix_secs)),
            formatted
        );
        assert_eq!(add_months(clock.now(), &clock), expires);
    }
    assert_ne!(expires, at, "one month later is a different instant");
}

/// The system clock resolves the offset per instant, so a zone that observes
/// daylight saving answers differently in January and in July.
///
/// This is the property the `expires` claim depends on and the one a cached
/// offset would break. The zone this machine is set to decides which branch
/// runs, and the branch is decided by the zone's own declared rule rather than
/// by assuming anything about it.
#[cfg(windows)]
#[test]
fn the_system_offset_follows_the_zone_daylight_rule() {
    use windows_sys::Win32::System::Time::{
        GetTimeZoneInformation, TIME_ZONE_ID_INVALID, TIME_ZONE_INFORMATION,
    };
    use wirepod_core::wallclock::SystemWallClock;

    // SAFETY: the pointer is to a live local of the right type, and the call
    // writes only through it.
    let mut zone: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    let id = unsafe { GetTimeZoneInformation(&mut zone) };
    if id == TIME_ZONE_ID_INVALID {
        // No zone to ask about, which is a machine setting rather than a
        // failure of the code under test.
        return;
    }

    let clock = SystemWallClock::new();
    let winter = clock.utc_offset_secs_at(days_from_civil(2026, 1, 1) * SECS_PER_DAY);
    let summer = clock.utc_offset_secs_at(days_from_civil(2026, 7, 1) * SECS_PER_DAY);
    assert_eq!(winter % 60, 0, "a zone offset is a whole number of minutes");
    assert_eq!(summer % 60, 0, "a zone offset is a whole number of minutes");

    // A zero month is Windows' way of saying the zone has no daylight rule.
    if zone.DaylightDate.wMonth == 0 {
        assert_eq!(
            winter, summer,
            "a zone with no daylight rule has one offset all year"
        );
        return;
    }
    assert_ne!(
        winter, summer,
        "a zone with a daylight rule cannot answer the same in January and July"
    );
    assert_eq!(
        (summer - winter).abs(),
        ((zone.DaylightBias - zone.StandardBias) * 60).abs(),
        "the two offsets differ by exactly the zone's declared daylight bias"
    );
}

/// This machine's `TIME_ZONE_INFORMATION`, or `None` when there is none.
///
/// A machine with no readable zone is a machine setting rather than a failure
/// of the code under test, so the tests below return instead of failing.
#[cfg(windows)]
fn live_zone() -> Option<windows_sys::Win32::System::Time::TIME_ZONE_INFORMATION> {
    use windows_sys::Win32::System::Time::{
        GetTimeZoneInformation, TIME_ZONE_ID_INVALID, TIME_ZONE_INFORMATION,
    };

    // SAFETY: the struct is plain integers and arrays of them, so an all-zero
    // value is a valid one, and the pointer is to a live local that outlives
    // the call, which writes only through it.
    let mut zone: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetTimeZoneInformation(&mut zone) } == TIME_ZONE_ID_INVALID {
        return None;
    }
    Some(zone)
}

/// The UTC year Go centres its Windows transition table on
/// (`time/zoneinfo_windows.go:188-189`).
#[cfg(windows)]
fn base_year() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("this machine's clock is set after the epoch")
        .as_secs();
    let secs = i64::try_from(secs).expect("the epoch offset fits in an i64");
    civil_from_days(secs.div_euclid(SECS_PER_DAY)).year
}

/// The instant a Windows daylight rule names in `year`, derived here rather
/// than asked of the code under test.
///
/// Windows writes a rule as a month, a weekday counting from Sunday, a week
/// within that month from 1 to 5 where 5 means the last one, and the wall time
/// the change happens at. `offset_before` is the offset in effect just before
/// it, which is what turns that wall time into an instant.
///
/// This is deliberately not the arithmetic
/// [`wirepod_core::wallclock`](wirepod_core::wallclock) uses. It finds the end
/// of the month from the first day of the next one instead of from a table of
/// month lengths, so the two agree only because both read the same rule out of
/// the same struct.
#[cfg(windows)]
fn declared_transition(
    year: i64,
    rule: &windows_sys::Win32::Foundation::SYSTEMTIME,
    offset_before: i32,
) -> i64 {
    let month = u32::from(rule.wMonth);
    let first = days_from_civil(year, month, 1);
    // 1 January 1970 was a Thursday, which is 4 counting from Sunday.
    let weekday = (first + 4).rem_euclid(7);
    let mut day = 1 + (i64::from(rule.wDayOfWeek) - weekday).rem_euclid(7);
    if rule.wDay >= 5 {
        let next_month = if month == 12 {
            days_from_civil(year + 1, 1, 1)
        } else {
            days_from_civil(year, month + 1, 1)
        };
        day += 4 * 7;
        if first + day > next_month {
            day -= 7;
        }
    } else {
        day += (i64::from(rule.wDay) - 1) * 7;
    }
    (first + day - 1) * SECS_PER_DAY
        + i64::from(rule.wHour) * 3600
        + i64::from(rule.wMinute) * 60
        + i64::from(rule.wSecond)
        - i64::from(offset_before)
}

/// Each offset is the one the zone declares, sign and all, and it changes at
/// the exact minute the rule names.
///
/// [`the_system_offset_follows_the_zone_daylight_rule`] asks only that January
/// and July differ by the declared bias. An implementation that negated every
/// offset passes that, and so does one that reads the rule's hour and drops its
/// minute, because the difference between the two offsets is untouched by
/// either. This test derives both of this year's transition instants from the
/// `TIME_ZONE_INFORMATION` and then asserts the whole offset on each side of
/// each of them, a minute apart, which neither mistake survives.
///
/// Reading the transitions from the rules rather than naming dates is what
/// keeps this honest on a machine set to some other zone: a southern hemisphere
/// zone changes into daylight saving in October and out of it in April, and
/// both are still the instant its own `DaylightDate` and `StandardDate` name.
#[cfg(windows)]
#[test]
fn the_system_offset_changes_at_the_minute_the_zone_declares() {
    use wirepod_core::wallclock::SystemWallClock;

    let Some(zone) = live_zone() else {
        return;
    };
    // A zero standard month is Windows saying the zone has no daylight rule, so
    // there is no transition to sit either side of.
    if zone.StandardDate.wMonth == 0 {
        return;
    }

    let standard = -(zone.Bias + zone.StandardBias) * 60;
    let daylight = -(zone.Bias + zone.DaylightBias) * 60;
    let clock = SystemWallClock::new();
    let year = base_year();
    let into_daylight = declared_transition(year, &zone.DaylightDate, standard);
    let into_standard = declared_transition(year, &zone.StandardDate, daylight);

    assert_eq!(
        clock.utc_offset_secs_at(into_daylight - 60),
        standard,
        "a minute before the zone starts daylight saving the offset is \
         -(Bias + StandardBias) * 60"
    );
    assert_eq!(
        clock.utc_offset_secs_at(into_daylight),
        daylight,
        "at the minute the zone starts daylight saving the offset is \
         -(Bias + DaylightBias) * 60"
    );
    assert_eq!(
        clock.utc_offset_secs_at(into_standard - 60),
        daylight,
        "a minute before the zone ends daylight saving it is still in it"
    );
    assert_eq!(
        clock.utc_offset_secs_at(into_standard),
        standard,
        "at the minute the zone ends daylight saving the offset is the \
         standard one again"
    );
}

/// The offset stops changing outside the two hundred year window.
///
/// Go builds two transitions a year for a hundred years each side of the year
/// it starts in (`time/zoneinfo_windows.go:185-201`), and nothing outside that.
/// Windows itself would answer for any year at all, so this is the difference
/// the Windows arm exists to reproduce and the bound has to be pinned on both
/// sides. A year inside the window has transitions and so cannot answer the
/// same in every month; a year outside it has none and so must.
///
/// Which offset holds above the window depends on the hemisphere, so only the
/// year below it is asserted by value: everything under the first transition is
/// the standard zone, whatever the zone is (`lookupFirstZone`,
/// `time/zoneinfo.go:233-257`).
#[cfg(windows)]
#[test]
fn the_system_offset_has_no_transition_outside_the_two_hundred_year_window() {
    use wirepod_core::wallclock::SystemWallClock;

    let Some(zone) = live_zone() else {
        return;
    };
    if zone.StandardDate.wMonth == 0 {
        return;
    }

    let clock = SystemWallClock::new();
    let base = base_year();
    let varies = |year: i64| {
        let offsets: Vec<i32> = (1..=12)
            .map(|month| {
                clock
                    .utc_offset_secs_at(days_from_civil(year, month, 15) * SECS_PER_DAY + 12 * 3600)
            })
            .collect();
        offsets.iter().any(|offset| *offset != offsets[0])
    };

    assert!(
        varies(base - 100),
        "the first year of the window still has its two transitions"
    );
    assert!(
        varies(base + 99),
        "the last year of the window still has its two transitions"
    );
    assert!(
        !varies(base - 101),
        "the year below the window has no transition, so one offset covers it"
    );
    assert!(
        !varies(base + 100),
        "the year above the window has no transition, so one offset covers it"
    );

    assert_eq!(
        clock.utc_offset_secs_at(days_from_civil(base - 101, 1, 1) * SECS_PER_DAY),
        -(zone.Bias + zone.StandardBias) * 60,
        "below the first transition every zone answers its standard offset"
    );
}

/// The Unix side of the same call answers something a zone could actually be.
///
/// The daylight assertion above cannot run here, because the machine's zone is
/// whatever the build agent is set to and a container is usually UTC. What can
/// be asserted is that `localtime_r` was reached and its answer is a zone
/// offset rather than a stray value, which is enough to fail a path that
/// panics, returns garbage or never links.
#[cfg(unix)]
#[test]
fn the_system_offset_is_a_plausible_zone_offset() {
    use wirepod_core::wallclock::SystemWallClock;

    let clock = SystemWallClock::new();
    for day in [days_from_civil(2026, 1, 1), days_from_civil(2026, 7, 1)] {
        let offset = clock.utc_offset_secs_at(day * SECS_PER_DAY);
        assert_eq!(offset % 60, 0, "a zone offset is a whole number of minutes");
        assert!(
            (-12 * 3600..=14 * 3600).contains(&offset),
            "no zone is outside -12:00 to +14:00, but this one answered {offset}"
        );
    }
}

/// Nothing in the recording may go unread.
///
/// Every case belongs either to a section this file tests or to a section
/// another commit tests, and both lists are spelled out. A new section, or a
/// new kind inside one, fails here rather than being skipped in silence.
#[test]
fn the_probe_file_holds_no_unrecognized_case() {
    for case in &recorded_cases() {
        let known = match case.section {
            // Tested here.
            "rfc3339" => case.kind() == "format",
            "addmonth" => case.kind() == "add_months",
            "addmonth_local" => matches!(
                case.kind(),
                "const" | "add_months_local" | "out_unix" | "out_off" | "offset"
            ),
            // Split with the logger, kind by kind and line for line, by
            // [`the_legacystamp_section_is_split_between_this_file_and_the_logger`].
            "legacystamp" => TIMEFMT_STAMP_KINDS
                .iter()
                .chain(LOGGER_STAMP_KINDS.iter())
                .any(|(kind, _)| *kind == case.kind()),
            // Tested by the token hashing, float formatting and JWT commits.
            "hash" | "f32json" | "claims" => true,
            _ => false,
        };
        assert!(
            known,
            "line {}: unrecognized section {} kind {}",
            case.number,
            case.section,
            case.kind()
        );
    }
}
