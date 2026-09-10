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

use wirepod_core::timefmt::{add_months, days_from_civil, legacy_stamp, rfc3339_nano};
use wirepod_core::wallclock::{FixedWallClock, WallClock, WallTime};

const EXPECTED: &str =
    include_str!("../../../docs/phases/P1-robot-connect-auth/go-probe/expected.txt");

/// Seconds in a day, for building an instant out of a recorded civil date.
const SECS_PER_DAY: i64 = 86_400;

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

#[test]
fn legacy_stamp_matches_the_recorded_probe() {
    let cases = recorded_cases();
    let mut stamps = 0usize;
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
            }
            // The whole-line layouts and the level names are the logger's, and
            // the logger commit tests them against these same lines.
            "legacy_line" | "file_line" | "level_string" => {}
            other => panic!("line {}: unknown legacystamp kind {other}", case.number),
        }
    }
    assert!(stamps > 0, "the recording holds no stamp cases");
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
            "legacystamp" => matches!(
                case.kind(),
                "stamp" | "const" | "legacy_line" | "file_line" | "level_string"
            ),
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
