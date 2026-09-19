//! The claim matrix, the JWS assembly and the UUID transform, table driven
//! from the recorded probe output.
//!
//! `crates/wirepod-core/tests/jwt.rs` drives one recorded claim set and pins
//! the header, the key order and the two base64url segments. This file drives
//! what that one holds fixed: the `claims_matrix` section runs
//! `CreateJWT`'s claim build (`token.go:254-265`) over fifteen instants, five
//! requestor ids and three token ids, in two zones with real daylight saving
//! transitions and in UTC, and every payload and signing input below was
//! printed by `github.com/golang-jwt/jwt` v3.2.2 rather than written here.
//!
//! The two real zones are there for opposite reasons. `America/Los_Angeles`
//! is west of Greenwich and covers the offsets `expires` carries either side
//! of a transition and the two wall times a transition makes impossible or
//! ambiguous. `Europe/Paris` is east of it, and the single case that runs
//! there is what fixes the order of the two zone lookups
//! [`wirepod_core::timefmt::add_months`] makes; no zone at a negative offset
//! can separate them.
//!
//! The `jws` section is what the port cannot reproduce and does not try to.
//! Go signs with a throwaway 1024-bit RSA key (`token.go:266-267`) whose
//! public half no peer ever sees, so that section records only the shape
//! around the signature: the segment count, its byte and character lengths,
//! that the first two segments are the signing string untouched, and which of
//! two signings differ. This port fills the slot with drawn bytes, which is
//! deviation 28, and the assertions below are the invariants that survive that
//! substitution.
//!
//! The `uuid` section is `github.com/google/uuid` v1.6.0's own transform from
//! sixteen drawn bytes to the string the `token_id` claim carries.
//!
//! Integration tests are separate binaries and cannot share helpers, so the
//! recording parser and the step-table zone below are copied from
//! `tests/jwt.rs` and `tests/timefmt.rs` rather than imported. That is the
//! convention here.
//!
//! Nothing here binds a port, contacts a robot, or reads anything outside the
//! repository except the one temporary directory
//! [`write_token_hash_reads_the_clock_exactly_once`] creates under the system
//! temporary directory.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use wirepod_core::store::jdocs::JdocsStore;
use wirepod_core::timefmt::rfc3339_nano;
use wirepod_core::token::jwt::{
    ALG, Claims, HEADER, Requestor, SIGNATURE_LEN, encode, encode_segment, issue_token,
    marshal_claims, random_signature, signing_input, uuid_v4, write_token_hash,
};
use wirepod_core::wallclock::{FixedWallClock, WallClock, WallTime};

const EXPECTED: &str = include_str!("data/go-probe/expected.txt");

/// Seconds in a day, for building an instant out of a recorded civil date.
const SECS_PER_DAY: i64 = 86_400;

/// A ceiling on anything awaited, generous enough that only a hang reaches it.
/// A real duration, because this crate's tests never pause the runtime's clock.
const CEILING: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// The census
// ---------------------------------------------------------------------------

/// How many whole claim builds the `claims_matrix` section holds.
const MATRIX_CASES: usize = 15;

/// The three zone names: the two with transitions and the one at offset zero.
const MATRIX_CONSTS: usize = 3;

/// The transition edges the section records: the six `addmonth_local` records
/// for `America/Los_Angeles`, and `Europe/Paris`'s two 2026 transitions. Both
/// step tables below are rebuilt from these and from nothing else.
const MATRIX_OFFSETS: usize = 10;

/// The `requestor_id` values, recorded as hex of their UTF-8 bytes.
const MATRIX_REQUESTORS: usize = 5;

/// The `token_id` values, which are the three UUIDs `tests/jwt.rs` pins
/// [`uuid_v4`] against.
const MATRIX_TOKEN_IDS: usize = 3;

/// Lines per case: the payload, the `expires` string, its Unix second, its
/// offset and the signing input.
const MATRIX_LINES_PER_CASE: usize = 5;

/// Every line of the section, fixtures and cases together. Stated separately
/// from the parts so that the identity between them is itself an assertion.
const MATRIX_LINES: usize = 96;

/// Every line of the `jws` section.
const JWS_LINES: usize = 10;

/// Every line of the `uuid` section: two constants and five draws.
const UUID_LINES: usize = 7;

/// The draws the `uuid` section records.
const UUID_DRAWS: usize = 5;

/// The seven claims a payload carries (`token.go:255-264`).
const CLAIM_COUNT: usize = 7;

// ---------------------------------------------------------------------------
// The recording
// ---------------------------------------------------------------------------

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
    fn kind(&self) -> &'static str {
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

    /// The value of one input key. A missing key is a probe change this file
    /// has not caught up with, so it panics rather than defaulting.
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
}

/// Undoes Go's `%q` quoting.
///
/// Only the five escapes the recording can contain are accepted. Anything else
/// panics rather than being passed through, so a probe that starts emitting a
/// new escape can never be mis-parsed into a value that happens to compare
/// equal.
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

/// The outputs of one kind, keyed by the `case=` name they carry.
fn by_case<'a>(lines: &[&'a Case], kind: &str) -> HashMap<&'static str, &'a Case> {
    let mut map = HashMap::new();
    for case in lines.iter().filter(|case| case.kind() == kind) {
        assert!(
            map.insert(case.get("case"), *case).is_none(),
            "line {}: the {kind} kind repeats a case name",
            case.number
        );
    }
    assert!(!map.is_empty(), "the section holds no {kind} lines");
    map
}

/// Decodes one lowercase-hex field of the recording.
///
/// Two of the five requestor ids carry bytes no recording should hold
/// literally, so they travel as hex; see the probe's
/// "How the claims_matrix section is built".
fn from_hex(text: &str, number: usize) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2)
            && text
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "line {number}: {text} is not an even run of lowercase hex"
    );
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).expect("hex digits are ASCII");
            u8::from_str_radix(digits, 16).expect("two hex digits are one byte")
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Reading an instant back out of the recording
// ---------------------------------------------------------------------------

/// Parses one `time.RFC3339Nano` string back into the instant and offset that
/// produced it.
///
/// Copied from `tests/jwt.rs`. It exists so that no test here hand-writes the
/// instant behind a recorded timestamp: every use formats the result again and
/// asserts the round trip first, so a wrong parse fails as itself rather than
/// as a wrong claim.
fn parse_rfc3339(text: &str) -> (WallTime, i32) {
    let number = |range: std::ops::Range<usize>| -> i64 {
        text[range]
            .parse()
            .unwrap_or_else(|_| panic!("{text} is not an RFC 3339 timestamp"))
    };
    let year = number(0..4);
    let month = u32::try_from(number(5..7)).expect("a month fits in a u32");
    let day = u32::try_from(number(8..10)).expect("a day fits in a u32");
    let hour = number(11..13);
    let minute = number(14..16);
    let second = number(17..19);

    let rest = &text[19..];
    let (fraction, zone) = match rest.strip_prefix('.') {
        Some(after) => {
            let end = after
                .find(|character: char| !character.is_ascii_digit())
                .unwrap_or(after.len());
            (&after[..end], &after[end..])
        }
        None => ("", rest),
    };
    let mut digits = fraction.to_owned();
    while digits.len() < 9 {
        digits.push('0');
    }
    let nanos: u32 = digits.parse().expect("the fraction is nine digits");

    let offset = if zone == "Z" {
        0
    } else {
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let hours: i32 = zone[1..3]
            .parse()
            .expect("the zone carries two hour digits");
        let minutes: i32 = zone[4..6]
            .parse()
            .expect("the zone carries two minute digits");
        sign * (hours * 3600 + minutes * 60)
    };

    let secs = wirepod_core::timefmt::days_from_civil(year, month, day) * SECS_PER_DAY
        + hour * 3600
        + minute * 60
        + second
        - i64::from(offset);
    (WallTime::new(secs, nanos), offset)
}

// ---------------------------------------------------------------------------
// The zone, rebuilt from the recording
// ---------------------------------------------------------------------------

/// A zone rebuilt from the offsets the `claims_matrix` section recorded, and
/// from nothing else.
///
/// Copied from `tests/timefmt.rs`. The probe recorded each transition as a
/// pair, the last second of one offset and the first second of the next, which
/// is enough to rebuild the step function over the span the cases cover: a
/// transition is derived wherever two consecutive samples disagree. Six
/// samples for `America/Los_Angeles` rather than the four a single `AddDate`
/// would need, because a whole claim build asks the zone for an offset at five
/// instants, not three, and four for `Europe/Paris`, its two 2026 transitions.
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

    fn offset_at(&self, unix_secs: i64) -> i32 {
        let mut offset = self.base;
        for &(at, next) in &self.transitions {
            if unix_secs >= at {
                offset = next;
            }
        }
        offset
    }
}

/// A clock stopped at one instant, whose zone is the step table above.
///
/// Stopped rather than ticking because the probe formatted both claims from
/// one instant, and [`Claims::new`] reads the clock twice
/// (`token.go:195-196`). A clock that moved between the two readings would put
/// a different fraction in `expires` than the recording holds.
struct MatrixClock<'a> {
    now: WallTime,
    zone: &'a RecordedZone,
}

impl WallClock for MatrixClock<'_> {
    fn now(&self) -> WallTime {
        self.now
    }

    fn utc_offset_secs_at(&self, unix_secs: i64) -> i32 {
        self.zone.offset_at(unix_secs)
    }
}

// ---------------------------------------------------------------------------
// One matrix case, resolved
// ---------------------------------------------------------------------------

/// One `claims_matrix` case with its fixtures already looked up.
struct MatrixCase {
    /// The line the payload was recorded on.
    number: usize,
    name: &'static str,
    zone: &'static str,
    requestor: Requestor,
    token_id: String,
    /// The instant `iat` names, from the recorded Unix second and fraction.
    instant: WallTime,
    /// The offset in effect at that instant, as the probe recorded it.
    offset: i32,
    /// The `iat` claim the probe wrote.
    iat: &'static str,
    /// The whole payload, as `json.Marshal` wrote it.
    payload: String,
}

/// The zones and every case of the `claims_matrix` section, resolved against
/// its own fixtures.
///
/// The offset lines carry the zone they belong to, so the step tables are
/// built per zone rather than from one pooled list: mixing two zones' samples
/// into one table would derive transitions that neither zone has.
fn matrix() -> (BTreeMap<&'static str, RecordedZone>, Vec<MatrixCase>) {
    let cases = recorded_cases();
    let lines = section(&cases, "claims_matrix");

    let zone_name = lines
        .iter()
        .find(|case| case.kind() == "const" && case.get("name") == "zone")
        .expect("the section records no zone name");
    assert_eq!(
        zone_name.output, "America/Los_Angeles",
        "line {}: the fakes below are rebuilt from this zone's recorded \
         offsets, so a probe that switches zones has to be re-read rather than \
         re-run",
        zone_name.number
    );
    let zone_utc = lines
        .iter()
        .find(|case| case.kind() == "const" && case.get("name") == "zone_utc")
        .expect("the section records no zero-offset zone name");
    assert_eq!(zone_utc.output, "UTC", "line {}", zone_utc.number);
    let zone_paris = lines
        .iter()
        .find(|case| case.kind() == "const" && case.get("name") == "zone_paris")
        .expect("the section records no zone east of Greenwich");
    assert_eq!(
        zone_paris.output, "Europe/Paris",
        "line {}: the case that fixes the order of add_months's two zone \
         lookups runs in this zone, and only a zone at a positive offset can \
         fix it",
        zone_paris.number
    );

    let mut samples: BTreeMap<&'static str, Vec<(i64, i32)>> = BTreeMap::new();
    for case in lines.iter().filter(|case| case.kind() == "offset") {
        samples
            .entry(case.get("zone"))
            .or_default()
            .push((case.num("unix"), case.output_num()));
    }
    let zones: BTreeMap<&'static str, RecordedZone> = samples
        .into_iter()
        .map(|(name, drawn)| (name, RecordedZone::from_samples(&drawn)))
        .collect();
    assert_eq!(
        zones.keys().copied().collect::<Vec<&str>>(),
        vec!["America/Los_Angeles", "Europe/Paris"],
        "the section no longer records offsets for exactly the two zones this \
         file rebuilds"
    );
    for case in lines.iter().filter(|case| case.kind() == "offset") {
        let zone = zones.get(case.get("zone")).unwrap_or_else(|| {
            panic!(
                "line {}: no step table was built for {}",
                case.number,
                case.get("zone")
            )
        });
        assert_eq!(
            zone.offset_at(case.num("unix")),
            case.output_num::<i32>(),
            "line {}: the derived zone disagrees with the recorded offset",
            case.number
        );
    }

    // The requestor ids, decoded out of hex and turned back into the two arms
    // `CreateJWT` decides between (`token.go:187` and `token.go:223`).
    let mut requestors: HashMap<&str, Requestor> = HashMap::new();
    for case in lines.iter().filter(|case| case.kind() == "requestor_hex") {
        let label = case.get("name");
        let bytes = from_hex(&case.output, case.number);
        let id = String::from_utf8(bytes)
            .unwrap_or_else(|error| panic!("line {}: {error}", case.number));
        let requestor = if label == "unknown" {
            assert_eq!(
                id,
                Requestor::Unknown.id(),
                "line {}: the recording's unknown requestor is not token.go:187's serial",
                case.number
            );
            Requestor::Unknown
        } else {
            let serial = id.strip_prefix("vic:").unwrap_or_else(|| {
                panic!(
                    "line {}: {id} does not start with token.go:223's prefix",
                    case.number
                )
            });
            Requestor::Robot(serial.to_owned())
        };
        assert!(
            requestors.insert(label, requestor).is_none(),
            "line {}: the requestor label {label} is recorded twice",
            case.number
        );
    }

    let mut token_ids: HashMap<&str, String> = HashMap::new();
    for case in lines.iter().filter(|case| case.kind() == "token_id") {
        let label = case.get("name");
        assert!(
            token_ids.insert(label, case.output.clone()).is_none(),
            "line {}: the token id label {label} is recorded twice",
            case.number
        );
    }

    let resolved: Vec<MatrixCase> = lines
        .iter()
        .filter(|case| case.kind() == "payload")
        .map(|case| {
            let requestor = requestors
                .get(case.get("requestor"))
                .unwrap_or_else(|| {
                    panic!("line {}: no requestor fixture for this case", case.number)
                })
                .clone();
            let token_id = token_ids
                .get(case.get("token_id"))
                .unwrap_or_else(|| {
                    panic!("line {}: no token id fixture for this case", case.number)
                })
                .clone();
            MatrixCase {
                number: case.number,
                name: case.get("case"),
                zone: case.get("zone"),
                requestor,
                token_id,
                instant: WallTime::new(case.num("iat_unix"), case.num("ns")),
                offset: case.num("iat_off"),
                iat: case.get("iat"),
                payload: case.output.clone(),
            }
        })
        .collect();
    assert_eq!(
        resolved.len(),
        MATRIX_CASES,
        "the section no longer holds the cases this file covers"
    );
    (zones, resolved)
}

/// The clock one case runs on: the step table for the zone the case names, and
/// a constant zero for the two cases the probe ran in UTC.
fn clock_for<'a>(
    case: &MatrixCase,
    zones: &'a BTreeMap<&'static str, RecordedZone>,
) -> Box<dyn WallClock + 'a> {
    if case.zone == "UTC" {
        assert_eq!(
            case.offset, 0,
            "line {}: a UTC case recorded a non-zero offset",
            case.number
        );
        Box::new(FixedWallClock::new(case.instant, 0))
    } else {
        let zone = zones.get(case.zone).unwrap_or_else(|| {
            panic!(
                "line {}: the section records no offsets for {}",
                case.number, case.zone
            )
        });
        Box::new(MatrixClock {
            now: case.instant,
            zone,
        })
    }
}

/// The claim set one case describes, built the way the token server builds it.
fn claims_for(case: &MatrixCase, zones: &BTreeMap<&'static str, RecordedZone>) -> Claims {
    let clock = clock_for(case, zones);
    Claims::new(&case.requestor, case.token_id.clone(), &*clock)
}

// ---------------------------------------------------------------------------
// The census
// ---------------------------------------------------------------------------

/// Nothing in the section may go untested, and the number of cases is stated
/// here as well as in the probe so that a case added to one and not the other
/// fails rather than going unread.
#[test]
fn the_claims_matrix_section_is_exactly_the_cases_this_file_covers() {
    assert_eq!(
        MATRIX_LINES,
        MATRIX_CONSTS
            + MATRIX_OFFSETS
            + MATRIX_REQUESTORS
            + MATRIX_TOKEN_IDS
            + MATRIX_LINES_PER_CASE * MATRIX_CASES,
        "the parts of the census no longer add up to the whole"
    );

    let cases = recorded_cases();
    let lines = section(&cases, "claims_matrix");
    assert_eq!(
        lines.len(),
        MATRIX_LINES,
        "the claims_matrix section is no longer {MATRIX_LINES} lines"
    );

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for case in &lines {
        *counts.entry(case.kind()).or_default() += 1;
    }
    let expected: BTreeMap<&str, usize> = [
        ("const", MATRIX_CONSTS),
        ("offset", MATRIX_OFFSETS),
        ("requestor_hex", MATRIX_REQUESTORS),
        ("token_id", MATRIX_TOKEN_IDS),
        ("payload", MATRIX_CASES),
        ("expires", MATRIX_CASES),
        ("exp_unix", MATRIX_CASES),
        ("exp_off", MATRIX_CASES),
        ("signing_input", MATRIX_CASES),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        counts, expected,
        "the kinds in the claims_matrix section are not the ones this file reads"
    );

    // The five per-case kinds have to name the same fifteen cases, or a case
    // could be driven against another case's expires.
    let names: Vec<BTreeSet<&str>> = ["payload", "expires", "exp_unix", "exp_off", "signing_input"]
        .into_iter()
        .map(|kind| {
            lines
                .iter()
                .filter(|case| case.kind() == kind)
                .map(|case| case.get("case"))
                .collect()
        })
        .collect();
    for set in &names {
        assert_eq!(
            set.len(),
            MATRIX_CASES,
            "one per-case kind does not name {MATRIX_CASES} distinct cases"
        );
        assert_eq!(
            set, &names[0],
            "the per-case kinds do not all name the same cases"
        );
    }
}

// ---------------------------------------------------------------------------
// The payload and the signing input
// ---------------------------------------------------------------------------

/// Every case's whole payload, byte for byte, out of [`Claims::new`] and
/// [`marshal_claims`] rather than out of anything written here.
///
/// This is the test the zones matter to. Five cases sit on or across a
/// daylight saving transition, so an implementation that resolved one offset
/// for both claims, or that carried the input's offset into the result, writes
/// a different `expires` and fails here.
///
/// `paris_fall_back` is the one that fixes something none of the others can.
/// [`wirepod_core::timefmt::add_months`] looks the zone up twice, and the
/// recording cannot say on its own where the first lookup happens. In that
/// case it has to happen at the target wall time: taking it at the input
/// instant a month earlier resolves `expires` to 1792886400 at `+02:00`
/// instead of the recorded 1792890000 at `+01:00`, and this test is what
/// says so.
#[test]
fn every_matrix_case_builds_the_recorded_payload() {
    let (zones, cases) = matrix();
    for case in &cases {
        // The instant first, so a wrong reading of the recording fails as
        // itself rather than as a wrong claim.
        assert_eq!(
            rfc3339_nano(case.instant, case.offset),
            case.iat,
            "line {}: {}: the recorded iat is not the instant beside it",
            case.number,
            case.name
        );

        let claims = claims_for(case, &zones);
        assert_eq!(
            claims.iat, case.iat,
            "line {}: {}: the iat claim",
            case.number, case.name
        );
        let payload = String::from_utf8(marshal_claims(&claims)).expect("the payload is UTF-8");
        assert_eq!(
            payload, case.payload,
            "line {}: {}: the payload",
            case.number, case.name
        );
    }
    assert_eq!(cases.len(), MATRIX_CASES);
}

/// The two encoded segments a signature would be taken over, which is what the
/// robot actually receives.
///
/// The `control_escapes` case is the one that separates Go's encoder from
/// serde_json's: five characters reach the payload as `\u` escapes, and a
/// payload that carried them raw encodes to different bytes here. The
/// `control_alphabet` case is the one that separates base64url from base64: its
/// requestor forces six-bit groups 62 and 63, so the segment has to carry `-`
/// and `_`.
#[test]
fn every_matrix_case_builds_the_recorded_signing_input() {
    let (zones, cases) = matrix();
    let recorded = recorded_cases();
    let inputs = by_case(&section(&recorded, "claims_matrix"), "signing_input");

    for case in &cases {
        let claims = claims_for(case, &zones);
        let payload = marshal_claims(&claims);
        let expected = inputs
            .get(case.name)
            .unwrap_or_else(|| panic!("no signing_input line for {}", case.name));
        assert_eq!(
            signing_input(HEADER, &payload),
            expected.output,
            "line {}: {}",
            expected.number,
            case.name
        );

        if case.name == "control_escapes" {
            assert!(
                case.payload.contains("\\u003c")
                    && case.payload.contains("\\u0026")
                    && case.payload.contains("\\u003e")
                    && case.payload.contains("\\u2028")
                    && case.payload.contains("\\u2029"),
                "the escapes case no longer carries all five of encoding/json's escapes"
            );
        }
        if case.name == "control_alphabet" {
            let segment = expected
                .output
                .split_once('.')
                .expect("a signing input is two segments")
                .1;
            assert!(
                segment.contains('-') && segment.contains('_'),
                "the alphabet case no longer forces base64url groups 62 and 63"
            );
            assert!(
                !segment.contains('+') && !segment.contains('/') && !segment.contains('='),
                "a standard-alphabet byte reached a segment"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The expires claim
// ---------------------------------------------------------------------------

/// `expires` is an instant, not only a string, and both are checked.
///
/// The recorded Unix second and offset are what make this more than a string
/// comparison: a formatter that wrote the right characters for the wrong
/// instant, or resolved the right instant against the wrong offset, passes a
/// string compare and fails here.
#[test]
fn the_expires_claim_lands_on_the_recorded_instant_and_offset() {
    let (zones, cases) = matrix();
    let recorded = recorded_cases();
    let lines = section(&recorded, "claims_matrix");
    let expires = by_case(&lines, "expires");
    let exp_unix = by_case(&lines, "exp_unix");
    let exp_off = by_case(&lines, "exp_off");

    for case in &cases {
        let claims = claims_for(case, &zones);
        let recorded_string = &expires[case.name].output;
        assert_eq!(
            &claims.expires, recorded_string,
            "line {}: {}: the expires claim",
            expires[case.name].number, case.name
        );

        let (instant, offset) = parse_rfc3339(&claims.expires);
        assert_eq!(
            rfc3339_nano(instant, offset),
            claims.expires,
            "{}: the expires claim did not survive being parsed and formatted again",
            case.name
        );
        assert_eq!(
            instant.unix_secs,
            exp_unix[case.name].output_num::<i64>(),
            "line {}: {}: expires landed on the wrong instant",
            exp_unix[case.name].number,
            case.name
        );
        assert_eq!(
            offset,
            exp_off[case.name].output_num::<i32>(),
            "line {}: {}: expires resolved the wrong offset",
            exp_off[case.name].number,
            case.name
        );
    }
}

/// `time.RFC3339Nano` writes an offset of zero as `Z`, not as `+00:00`, and
/// the two cases the probe ran in UTC are what pin it inside a whole claim
/// build.
///
/// It matters because the robot parses both claims back with Go's `RFC3339`
/// layout (`identity/token.go:126`, `:135`), which accepts either spelling, so
/// nothing downstream would complain; the difference would only ever show up
/// as a byte diff against the Go server's tokens.
#[test]
fn a_claim_set_at_offset_zero_writes_z_rather_than_plus_zero() {
    let (zones, cases) = matrix();
    let mut checked = 0usize;
    for case in cases.iter().filter(|case| case.zone == "UTC") {
        let claims = claims_for(case, &zones);
        for (name, value) in [("iat", &claims.iat), ("expires", &claims.expires)] {
            assert!(
                value.ends_with('Z'),
                "{}: the {name} claim at offset zero is {value}",
                case.name
            );
            assert!(
                !value.contains("+00:00"),
                "{}: the {name} claim spells offset zero out",
                case.name
            );
        }
        checked += 1;
    }
    assert_eq!(
        checked, 2,
        "the recording no longer holds two zero-offset cases"
    );
}

// ---------------------------------------------------------------------------
// The shape of the payload
// ---------------------------------------------------------------------------

/// The payload holds the seven claims and nothing else, in the order Go's map
/// encoder sorts them into.
///
/// `tests/jwt.rs` asserts the order with a substring walk over one payload,
/// which cannot see an eighth key and cannot tell a key from a value that
/// happens to spell one. This parses the payload instead, so the key *set* is
/// an assertion, and then walks the needles over all fifteen.
#[test]
fn the_payload_holds_exactly_the_seven_recorded_keys_in_order() {
    let recorded = recorded_cases();
    let order = section(&recorded, "claims")
        .into_iter()
        .find(|case| case.kind() == "key_order")
        .expect("the claims section records no key order");
    let keys: Vec<&str> = order.output.split(',').collect();
    assert_eq!(
        keys.len(),
        CLAIM_COUNT,
        "line {}: the recorded key order is no longer seven keys",
        order.number
    );
    let expected: BTreeSet<&str> = keys.iter().copied().collect();

    let (zones, cases) = matrix();
    for case in &cases {
        let claims = claims_for(case, &zones);
        let payload = String::from_utf8(marshal_claims(&claims)).expect("the payload is UTF-8");

        let parsed: Value = serde_json::from_str(&payload)
            .unwrap_or_else(|error| panic!("{}: the payload is not JSON: {error}", case.name));
        let object = parsed
            .as_object()
            .unwrap_or_else(|| panic!("{}: the payload is not a JSON object", case.name));
        let present: BTreeSet<&str> = object.keys().map(String::as_str).collect();
        assert_eq!(
            present, expected,
            "{}: the payload's keys are not the seven the recording names",
            case.name
        );

        let mut previous: Option<usize> = None;
        for key in &keys {
            let needle = format!("\"{key}\":");
            let at = payload
                .find(&needle)
                .unwrap_or_else(|| panic!("{}: the payload has no {key} claim", case.name));
            if let Some(before) = previous {
                assert!(
                    at > before,
                    "{}: the {key} claim is not after the one the recording puts ahead of it",
                    case.name
                );
            }
            previous = Some(at);
        }
    }
}

/// `expires` is always after `iat`, in every case the recording holds.
///
/// The robot refreshes its token at `expires` minus three hours
/// (`vector-cloud/internal/token/identity/identity.go:191-193`), so a claim set
/// whose gap were zero or negative would put the refresh time in the past on
/// arrival and the robot would ask again forever. One calendar month is never
/// less than twenty-eight days, and the three cases whose target wall time
/// does not exist or happens twice are the ones where that could conceivably
/// slip.
#[test]
fn expires_is_always_after_iat() {
    let (zones, cases) = matrix();
    // Three hours, the refresh lead `identity.go:192` subtracts.
    const REFRESH_LEAD: i64 = 3 * 3600;
    for case in &cases {
        let claims = claims_for(case, &zones);
        let (issued, _) = parse_rfc3339(&claims.iat);
        let (expiry, _) = parse_rfc3339(&claims.expires);
        let gap = expiry.unix_secs - issued.unix_secs;
        assert!(
            gap > REFRESH_LEAD,
            "{}: expires is {gap} seconds after iat, which is not enough for the \
             robot's three-hour refresh lead",
            case.name
        );
        assert!(
            gap >= 27 * SECS_PER_DAY,
            "{}: one calendar month came out as {gap} seconds",
            case.name
        );
    }
    // Without this the test would pass on an empty case list, which is what a
    // recording the parser stopped recognising would produce.
    assert_eq!(cases.len(), MATRIX_CASES);
}

// ---------------------------------------------------------------------------
// The clock the hash write reads
// ---------------------------------------------------------------------------

/// A clock that counts its readings.
struct CountingClock {
    now: WallTime,
    offset_secs: i32,
    reads: AtomicU64,
}

impl CountingClock {
    fn new(now: WallTime, offset_secs: i32) -> Self {
        Self {
            now,
            offset_secs,
            reads: AtomicU64::new(0),
        }
    }

    fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }
}

impl WallClock for CountingClock {
    fn now(&self) -> WallTime {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.now
    }

    fn utc_offset_secs_at(&self, _unix_secs: i64) -> i32 {
        self.offset_secs
    }
}

/// A directory under the system temporary directory, removed when the test
/// ends. Copied from `tests/jwt.rs`.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is before the epoch")
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wirepod-jwtmatrix-{label}-{}-{nanos:x}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("could not create the temporary directory");
        Self { path }
    }

    fn jdocs_file(&self) -> PathBuf {
        self.path.join("jdocs.json")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// `WriteTokenHash` reads the clock once (`token.go:110`), which is a third
/// reading separate from the two `CreateJWT` takes at `token.go:195-196`.
///
/// It matters because `issued_at` lands in a file the robot reads back. A
/// second reading here would be harmless, but a call that reused one of
/// `CreateJWT`'s readings instead would stamp the document with the time the
/// claims were built rather than the time the hash was written, and the two
/// are separated by the whole GUID draw.
#[tokio::test]
async fn write_token_hash_reads_the_clock_exactly_once() {
    let (_zones, cases) = matrix();
    let case = cases
        .iter()
        .find(|case| case.name == "control_lower")
        .expect("the recording no longer holds the control_lower case");

    let directory = TempDir::new("clock");
    let store = JdocsStore::new(
        directory
            .jdocs_file()
            .to_str()
            .expect("the temporary path is UTF-8")
            .to_owned(),
    );
    let clock = CountingClock::new(case.instant, case.offset);

    // An all-zero serial and a placeholder hash of the right length: no live
    // value reaches this test.
    const PLACEHOLDER_HASH: &str =
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    tokio::time::timeout(
        CEILING,
        write_token_hash(&store, "00000000", PLACEHOLDER_HASH, &clock),
    )
    .await
    .expect("write_token_hash did not finish")
    .expect("the rewrite failed");

    assert_eq!(
        clock.reads(),
        1,
        "token.go:110 reads the clock once, and this is not one of CreateJWT's two readings"
    );

    let docs = store.snapshot();
    assert_eq!(docs.len(), 1, "one call wrote more than one document");
    assert!(
        docs[0].jdoc.json_doc.contains(case.iat),
        "issued_at is not the instant the clock was stopped at: {}",
        docs[0].jdoc.json_doc
    );
}

// ---------------------------------------------------------------------------
// The JWS assembly
// ---------------------------------------------------------------------------

/// Everything the `jws` section records that survives replacing Go's signature
/// with drawn bytes.
///
/// The signature itself does not survive and is not meant to: Go signs with a
/// throwaway 1024-bit key whose public half no peer ever sees
/// (`token.go:266-267`), and the robot parses with `ParseUnverified`
/// (`identity.go:158`), so nothing on either side checks it. This port draws
/// [`SIGNATURE_LEN`] bytes instead, which is deviation 28. What is asserted
/// here is the shape: the slot's size, the alphabet, that the first two
/// segments are untouched, and that two tokens issued from one claim set still
/// differ, which is the one observable property Go's per-request key gives.
#[test]
fn the_jws_section_matches_this_ports_assembly() {
    let recorded = recorded_cases();
    let lines = section(&recorded, "jws");
    assert_eq!(
        lines.len(),
        JWS_LINES,
        "the jws section is no longer {JWS_LINES} lines"
    );
    let value = |kind: &str| -> &Case {
        lines
            .iter()
            .find(|case| case.kind() == kind)
            .copied()
            .unwrap_or_else(|| panic!("the jws section records no {kind}"))
    };

    // The key size and the signature length are the same fact twice: an RS512
    // signature is exactly as wide as the modulus.
    let key_bits: usize = lines
        .iter()
        .find(|case| case.kind() == "const" && case.get("name") == "key_bits")
        .expect("the jws section records no key size")
        .output_num();
    let sig_bytes: usize = value("sig_bytes").output_num();
    assert_eq!(
        SIGNATURE_LEN, sig_bytes,
        "the signature slot is not the width token.go:266's key gives"
    );
    assert_eq!(
        sig_bytes * 8,
        key_bits,
        "line {}: the recorded key size and signature width disagree",
        value("sig_bytes").number
    );

    let sig_chars: usize = value("sig_chars").output_num();
    assert_eq!(
        encode_segment(&[0u8; SIGNATURE_LEN]).len(),
        sig_chars,
        "the encoded signature segment is not the length the library writes"
    );

    // The header, which the library builds and this port hard codes.
    let header = String::from_utf8(HEADER.to_vec()).expect("the header is UTF-8");
    assert_eq!(ALG, value("alg_header").output, "the alg the header names");
    assert_eq!(
        header,
        format!(
            "{{\"alg\":\"{}\",\"typ\":\"{}\"}}",
            value("alg_header").output,
            value("typ_header").output
        ),
        "the hard-coded header is not the one jwt.NewWithClaims writes"
    );

    let (zones, cases) = matrix();
    let claims = claims_for(&cases[0], &zones);
    let payload = marshal_claims(&claims);
    let signature = random_signature().expect("the OS random source refused");
    let token = encode(HEADER, &payload, &signature);

    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(
        segments.len(),
        value("segments").output_num::<usize>(),
        "a token is not the number of segments the library produces"
    );
    assert_eq!(
        format!("{}.{}", segments[0], segments[1]),
        signing_input(HEADER, &payload),
        "the first two segments are not the signing input untouched"
    );
    assert_eq!(
        value("head_is_signing_input").output,
        "true",
        "line {}: the recording no longer says SignedString leaves them untouched",
        value("head_is_signing_input").number
    );

    assert_eq!(
        value("sig_has_no_standard_alphabet").output,
        "true",
        "line {}",
        value("sig_has_no_standard_alphabet").number
    );
    assert!(
        !segments[2].contains('+') && !segments[2].contains('/') && !segments[2].contains('='),
        "a standard-alphabet byte reached the signature segment: {}",
        segments[2]
    );
    assert_eq!(
        segments[2].len(),
        sig_chars,
        "the signature segment is not the recorded length"
    );

    // Go's per-request key is what makes two tokens over one claim set differ;
    // here it is the draw. Either way the recording says they differ.
    assert_eq!(
        value("fresh_key_each_call_differs").output,
        "true",
        "line {}",
        value("fresh_key_each_call_differs").number
    );
    let first = issue_token(&claims).expect("the OS random source refused");
    let second = issue_token(&claims).expect("the OS random source refused");
    assert_ne!(
        first, second,
        "two tokens issued from one claim set are identical, so the slot is not drawn"
    );
    assert_eq!(
        first.split('.').take(2).collect::<Vec<_>>(),
        second.split('.').take(2).collect::<Vec<_>>(),
        "the two tokens differ somewhere other than the signature segment"
    );

    // Recorded because it is the reason Go needs a fresh key at all: PKCS#1
    // v1.5 is deterministic, so the same key would sign the same claim set the
    // same way. This port has no key, so there is nothing here to reproduce;
    // the line is asserted so a probe that started answering otherwise would
    // be read rather than ignored.
    assert_eq!(
        value("same_key_twice_differs").output,
        "false",
        "line {}: PKCS#1 v1.5 is no longer deterministic, which would change why \
         token.go:266 generates a key per request",
        value("same_key_twice_differs").number
    );
}

// ---------------------------------------------------------------------------
// The token id
// ---------------------------------------------------------------------------

/// [`uuid_v4`] is `github.com/google/uuid`'s `NewRandomFromReader`
/// (`version4.go:47`), which is the transform `uuid.New` (`version4.go:13`)
/// runs over the sixteen bytes it draws and which `token.go:180-183` turns into
/// the `token_id` claim.
#[test]
fn the_uuid_section_matches_gos_own_formatting() {
    let recorded = recorded_cases();
    let lines = section(&recorded, "uuid");
    assert_eq!(
        lines.len(),
        UUID_LINES,
        "the uuid section is no longer {UUID_LINES} lines"
    );

    let layout = lines
        .iter()
        .find(|case| case.kind() == "const" && case.get("name") == "layout")
        .expect("the uuid section records no layout");
    let widths: Vec<usize> = layout
        .output
        .split('-')
        .map(|group| group.parse().expect("the layout is a run of numbers"))
        .collect();

    let mut checked = 0usize;
    for case in lines.iter().filter(|case| case.kind() == "uuid") {
        let bytes = from_hex(case.get("draw"), case.number);
        let draw: [u8; 16] = bytes
            .try_into()
            .unwrap_or_else(|_| panic!("line {}: a draw is not sixteen bytes", case.number));
        assert_eq!(
            uuid_v4(&draw),
            case.output,
            "line {}: the draw {} formats differently here",
            case.number,
            case.get("draw")
        );
        let groups: Vec<usize> = case.output.split('-').map(str::len).collect();
        assert_eq!(
            groups, widths,
            "line {}: the id is not the recorded 8-4-4-4-12 layout",
            case.number
        );
        checked += 1;
    }
    assert_eq!(
        checked, UUID_DRAWS,
        "the uuid section no longer holds {UUID_DRAWS} draws"
    );
}
