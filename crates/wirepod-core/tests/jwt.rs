//! The robot's JWT and the token hash write, table driven from the recorded
//! probe output.
//!
//! `docs/phases/P1-robot-connect-auth/go-probe/expected.txt` is the stdout of
//! the Go program committed beside it, built against the same
//! `github.com/golang-jwt/jwt` v3.2.2 the Go server pins, so every claim byte
//! below was printed by that library rather than written here. Each line is
//! `section\tinput\toutput`: the input is space-separated `key=value` pairs
//! whose first pair is always `kind=`, and the output is always a Go `%q`
//! quoted literal. A `kind=` this file does not recognize fails a test, so a
//! new probe line cannot be silently skipped.
//!
//! The one expectation that is not in the recording is the `vic.AppTokens`
//! document's bytes, which the `claims` section does not cover. It was printed
//! by a throwaway Go program built from the two structs at `hashing.go:36-45`
//! and the assignments at `token.go:109-115`, and is transcribed into
//! [`GO_APP_TOKENS_DOC`] with the hash replaced by a run of `A`s of the length
//! a real one has.
//!
//! Everything that touches the disk runs in a directory under the system
//! temporary directory, named for the process, so nothing here can reach the
//! repository or the live `%APPDATA%\wire-pod` the Go server is serving from.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use wirepod_core::esn::Esn;
use wirepod_core::store::jdocs::{JdocsStore, parse_jdocs};
use wirepod_core::timefmt::{days_from_civil, rfc3339_nano};
use wirepod_core::token::jwt::{
    ALG, APP_TOKENS_DOC, Claims, ClientTokenManager, DEFAULT_REQUESTOR_ID, HEADER,
    NEW_TOKEN_METADATA, NEW_TOKEN_VERSION, Requestor, SIGNATURE_LEN, TOKEN_TYPE, TokenBundle,
    USER_ID, encode, encode_segment, generate_token_id, issue_token, marshal_claims,
    random_signature, signing_input, uuid_v4, write_token_hash,
};
use wirepod_core::wallclock::{FixedWallClock, WallClock, WallTime};

const EXPECTED: &str =
    include_str!("../../../docs/phases/P1-robot-connect-auth/go-probe/expected.txt");

/// How many cases the `claims` section holds. A second statement of the number,
/// so that a case added to the probe fails here rather than going untested.
const CLAIMS_CASES: usize = 16;

/// Seconds in a day, for building an instant out of a recorded civil date.
const SECS_PER_DAY: i64 = 86_400;

/// A ceiling on anything awaited, generous enough that only a hang reaches it.
/// Real durations, because this crate's tests never pause the runtime's clock.
const CEILING: Duration = Duration::from_secs(30);

/// A stand-in for a stored hash: the length a real one has
/// ([`wirepod_core::HASHED_B64_LEN`]) and none of its bytes.
const PLACEHOLDER_HASH: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// The `vic.AppTokens` document Go writes for [`PLACEHOLDER_HASH`] issued at
/// the instant the probe's `iat` names.
///
/// Printed by a throwaway Go program holding `ClientToken` and
/// `ClientTokenManager` copied verbatim from `hashing.go:36-45`, running
/// `token.go:109-115` over those two fixed inputs, and printing
/// `json.Marshal`'s result with `%q`. The field order is Go's declaration
/// order and every one of the four is present, because none carries
/// `omitempty`.
const GO_APP_TOKENS_DOC: &str = concat!(
    r#"{"client_tokens":[{"hash":"#,
    r#""AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","#,
    r#""client_name":"wirepod","app_id":"SDK","#,
    r#""issued_at":"2026-09-09T12:34:56.789012345-07:00"}]}"#,
);

// ---------------------------------------------------------------------------
// The recording
// ---------------------------------------------------------------------------

/// One recorded case: its section, its `key=value` inputs and its unquoted
/// output.
struct Case<'a> {
    /// The one-based line number in the recording, for failure messages.
    line: usize,
    section: &'a str,
    input: Vec<(&'a str, &'a str)>,
    output: String,
}

impl<'a> Case<'a> {
    /// The `kind=` pair, which every line carries first.
    fn kind(&self) -> &'a str {
        let line = self.line;
        self.input
            .first()
            .filter(|(key, _)| *key == "kind")
            .unwrap_or_else(|| panic!("line {line}: the input column does not start with kind="))
            .1
    }

    /// The value of one input pair. A missing key is a probe change this file
    /// has not caught up with, so it panics rather than defaulting.
    fn get(&self, key: &str) -> &'a str {
        let line = self.line;
        self.input
            .iter()
            .find(|(name, _)| *name == key)
            .unwrap_or_else(|| panic!("line {line}: no {key}= pair in the input column"))
            .1
    }

    /// The input column as the probe wrote it, which is what keys a case.
    fn input_column(&self) -> String {
        self.input
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Undoes Go's `%q` quoting.
///
/// Only the escapes the recording actually uses are accepted. Anything else
/// panics, so a probe that starts emitting a new escape can never be silently
/// mis-parsed into a value that happens to compare equal.
fn unquote(literal: &str, line: usize) -> String {
    let body = literal
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("line {line}: the output column is not a Go %q literal"));
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
            other => panic!("line {line}: unsupported escape {other:?}"),
        }
    }
    out
}

/// Every case in the recording, comments dropped.
fn recorded_cases() -> Vec<Case<'static>> {
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
            line: number,
            section,
            input: input
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
fn section(cases: &[Case<'static>], name: &str) -> Vec<Case<'static>> {
    let selected: Vec<Case<'static>> = cases
        .iter()
        .filter(|case| case.section == name)
        .map(|case| Case {
            line: case.line,
            section: case.section,
            input: case.input.clone(),
            output: case.output.clone(),
        })
        .collect();
    assert!(!selected.is_empty(), "the recording holds no {name} cases");
    selected
}

/// The `claims` section keyed by its input column, which is unique per line.
fn claims_by_input() -> BTreeMap<String, String> {
    let cases = recorded_cases();
    let mut map = BTreeMap::new();
    for case in section(&cases, "claims") {
        let key = case.input_column();
        assert!(
            map.insert(key.clone(), case.output.clone()).is_none(),
            "line {}: the claims section repeats the input {key}",
            case.line
        );
    }
    map
}

/// One recorded `claims` output, by its input column.
fn claim(recorded: &BTreeMap<String, String>, input: &str) -> String {
    recorded
        .get(input)
        .unwrap_or_else(|| panic!("the claims section has no `{input}` case"))
        .clone()
}

// ---------------------------------------------------------------------------
// Reading an instant back out of the recording
// ---------------------------------------------------------------------------

/// Parses one `time.RFC3339Nano` string back into the instant and offset that
/// produced it.
///
/// This exists so that no test here hand-writes the instant behind a recorded
/// timestamp: the recording's own `iat` is parsed and then formatted again, and
/// every test that uses it first asserts the round trip, so a wrong parse fails
/// as itself rather than as a wrong claim.
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

    let secs =
        days_from_civil(year, month, day) * SECS_PER_DAY + hour * 3600 + minute * 60 + second
            - i64::from(offset);
    (WallTime::new(secs, nanos), offset)
}

/// The claim set the recording describes, rebuilt from the recorded inputs and
/// nothing else.
fn recorded_claims(recorded: &BTreeMap<String, String>) -> Claims {
    let iat = claim(recorded, "kind=value name=iat");
    let (instant, offset) = parse_rfc3339(&iat);
    assert_eq!(
        rfc3339_nano(instant, offset),
        iat,
        "the recorded iat did not survive being parsed and formatted again"
    );

    let requestor_id = claim(recorded, "kind=value name=requestor_id");
    let serial = requestor_id
        .strip_prefix("vic:")
        .expect("the recorded requestor id is `vic:` plus a serial");

    let clock = FixedWallClock::new(instant, offset);
    Claims::new(
        &Requestor::Robot(Esn::new(serial)),
        claim(recorded, "kind=value name=token_id"),
        &clock,
    )
}

// ---------------------------------------------------------------------------
// The claims, byte for byte
// ---------------------------------------------------------------------------

/// The section is exactly the sixteen lines this file reads, and every one of
/// them is read. A case added to the probe and not handled here fails the
/// second assertion rather than passing unnoticed.
#[test]
fn the_claims_section_is_exactly_the_cases_this_file_covers() {
    let cases = recorded_cases();
    let claims = section(&cases, "claims");
    assert_eq!(
        claims.len(),
        CLAIMS_CASES,
        "the claims section is no longer {CLAIMS_CASES} cases"
    );

    let covered: Vec<String> = claims
        .iter()
        .filter(|case| {
            matches!(
                case.kind(),
                "header"
                    | "payload"
                    | "header_b64url"
                    | "payload_b64url"
                    | "signing_input"
                    | "alg"
                    | "key_order"
                    | "value"
                    | "const"
            )
        })
        .map(Case::input_column)
        .collect();
    assert_eq!(
        covered.len(),
        CLAIMS_CASES,
        "the claims section grew a kind this file does not read"
    );
}

/// The claim payload, the header, both segments and the signing input are the
/// bytes the `golang-jwt/jwt` library produced (`token.go:254-265`).
#[test]
fn the_claims_match_the_recorded_bytes() {
    let recorded = claims_by_input();
    let claims = recorded_claims(&recorded);
    let payload = marshal_claims(&claims);

    assert_eq!(
        String::from_utf8(payload.clone()).expect("the payload is UTF-8"),
        claim(&recorded, "kind=payload"),
        "the claim payload is not the bytes Go's map encoder wrote"
    );
    assert_eq!(
        String::from_utf8(HEADER.to_vec()).expect("the header is UTF-8"),
        claim(&recorded, "kind=header"),
        "the header is not the one jwt.NewWithClaims built for RS512"
    );
    assert_eq!(
        encode_segment(HEADER),
        claim(&recorded, "kind=header_b64url"),
        "the header segment is not jwt.EncodeSegment's"
    );
    assert_eq!(
        encode_segment(&payload),
        claim(&recorded, "kind=payload_b64url"),
        "the payload segment is not jwt.EncodeSegment's"
    );
    assert_eq!(
        signing_input(HEADER, &payload),
        claim(&recorded, "kind=signing_input"),
        "the two segments are not joined the way SigningString joins them"
    );
    assert_eq!(ALG, claim(&recorded, "kind=alg"));
}

/// Every claim value the recording carries, field by field, so a payload that
/// matched by accident still names which claim moved.
#[test]
fn each_claim_value_matches_the_recorded_one() {
    let recorded = claims_by_input();
    let claims = recorded_claims(&recorded);

    assert_eq!(claims.expires, claim(&recorded, "kind=value name=expires"));
    assert_eq!(claims.iat, claim(&recorded, "kind=value name=iat"));
    assert_eq!(
        claims.requestor_id,
        claim(&recorded, "kind=value name=requestor_id")
    );
    assert_eq!(
        claims.token_id,
        claim(&recorded, "kind=value name=token_id")
    );
    assert_eq!(
        claims.token_type,
        claim(&recorded, "kind=value name=token_type")
    );
    assert_eq!(claims.user_id, claim(&recorded, "kind=value name=user_id"));

    // `token.go:257` writes an untyped nil, which marshals to `null`, and the
    // recording spells the value out rather than leaving the key absent.
    let payload = String::from_utf8(marshal_claims(&claims)).expect("the payload is UTF-8");
    let permissions = claim(&recorded, "kind=value name=permissions");
    assert!(
        payload.contains(&format!(r#""permissions":{permissions}"#)),
        "the permissions claim is not the recorded {permissions}"
    );

    // The two literals the Go server hard codes (`token.go:31`, `token.go:187`).
    assert_eq!(USER_ID, claim(&recorded, "kind=const name=UserId"));
    assert_eq!(
        DEFAULT_REQUESTOR_ID,
        claim(&recorded, "kind=const name=default_requestor_id")
    );
    assert_eq!(
        Requestor::Unknown.id(),
        DEFAULT_REQUESTOR_ID,
        "an unknown requestor does not claim token.go:187's serial"
    );
}

/// The seven keys appear in the order Go's map encoder sorts them into
/// (`encoding/json/encode.go:745-775`), which the probe records as a case of
/// its own.
#[test]
fn the_claim_keys_are_in_the_recorded_order() {
    let recorded = claims_by_input();
    let claims = recorded_claims(&recorded);
    let payload = String::from_utf8(marshal_claims(&claims)).expect("the payload is UTF-8");

    let order = claim(&recorded, "kind=key_order");
    let keys: Vec<&str> = order.split(',').collect();
    assert_eq!(keys.len(), 7, "the recorded key order is no longer seven");

    let mut previous = 0;
    for key in keys {
        let needle = format!(r#""{key}":"#);
        let at = payload
            .find(&needle)
            .unwrap_or_else(|| panic!("the payload has no {key} claim"));
        assert!(
            at >= previous,
            "the {key} claim comes before the one the recording puts ahead of it"
        );
        previous = at;
    }
}

/// `token_type` and `user_id` are the literals the server hard codes, and
/// nothing can construct a claim set with anything else in them.
#[test]
fn the_two_hard_coded_claims_are_the_ones_go_writes() {
    let clock = FixedWallClock::new(WallTime::new(0, 0), 0);
    let claims = Claims::new(&Requestor::Unknown, "id", &clock);
    assert_eq!(claims.token_type, TOKEN_TYPE, "token.go:263");
    assert_eq!(claims.user_id, USER_ID, "token.go:31, token.go:264");
}

// ---------------------------------------------------------------------------
// The clock is read twice
// ---------------------------------------------------------------------------

/// A clock that advances by one nanosecond per read and whose zone steps once.
///
/// The advance is what makes Go's two `time.Now()` calls (`token.go:195-196`)
/// visible: a claim set built from one reading carries the same fraction in
/// both claims, and this clock makes that a failure rather than a coincidence.
/// The step is what makes the per-instant offset lookup visible: `iat` falls on
/// one side of it and `expires`, a month later, on the other.
struct TickingClock {
    /// The first reading; every later one is this plus the read count.
    base: WallTime,
    /// How many times [`WallClock::now`] has been called.
    reads: AtomicU64,
    /// The instant the zone steps at.
    steps_at: i64,
    /// The offset before the step.
    before: i32,
    /// The offset from the step onwards.
    after: i32,
}

impl TickingClock {
    fn new(base: WallTime, steps_at: i64, before: i32, after: i32) -> Self {
        Self {
            base,
            reads: AtomicU64::new(0),
            steps_at,
            before,
            after,
        }
    }

    fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }
}

impl WallClock for TickingClock {
    fn now(&self) -> WallTime {
        let read = self.reads.fetch_add(1, Ordering::Relaxed);
        let step = u32::try_from(read).expect("a test reads the clock a handful of times");
        WallTime::new(self.base.unix_secs, self.base.nanos + step)
    }

    fn utc_offset_secs_at(&self, unix_secs: i64) -> i32 {
        if unix_secs >= self.steps_at {
            self.after
        } else {
            self.before
        }
    }
}

/// `token.go:195-196` calls `time.Now()` twice, so the two claims carry
/// different fractions, and resolves each claim's zone at its own instant, so
/// they can carry different offsets.
#[test]
fn the_clock_is_read_once_per_claim_and_the_zone_once_per_instant() {
    // 2026-10-20T12:00:00-07:00, a fraction chosen so that both readings write
    // nine digits and neither loses a trailing zero.
    let base_local = days_from_civil(2026, 10, 20) * SECS_PER_DAY + 12 * 3600;
    let base = WallTime::new(base_local + 7 * 3600, 123_456_788);
    // The zone steps the day after `iat` and well before `expires`.
    let clock = TickingClock::new(base, base.unix_secs + SECS_PER_DAY, -7 * 3600, -8 * 3600);

    let claims = Claims::new(&Requestor::Unknown, "token-id", &clock);

    assert_eq!(
        clock.reads(),
        2,
        "token.go:195-196 reads the clock twice, once per claim"
    );
    assert!(
        claims.iat.contains(".123456788"),
        "the iat claim is not the first reading: {}",
        claims.iat
    );
    assert!(
        claims.expires.contains(".123456789"),
        "the expires claim is not the second reading: {}",
        claims.expires
    );
    assert!(
        claims.iat.ends_with("-07:00"),
        "the iat claim did not take the offset in effect at its own instant: {}",
        claims.iat
    );
    assert!(
        claims.expires.ends_with("-08:00"),
        "the expires claim took the offset in effect at iat rather than at its own instant: {}",
        claims.expires
    );
}

/// `expires` is `AddDate(0, 1, 0)`, which normalizes rather than clamps, so a
/// token issued at a month end expires in the month after next.
///
/// Every case comes from the recording's `addmonth` section, whose cases run in
/// a `time.FixedZone` and so can be driven by a [`FixedWallClock`]. The `in=`
/// column is asserted as `iat`, which settles that the instant was rebuilt
/// correctly before anything was added to it.
#[test]
fn the_expires_claim_normalizes_a_month_end_the_way_add_date_does() {
    let cases = recorded_cases();
    let month_ends = section(&cases, "addmonth");
    let mut normalized = 0usize;

    for case in &month_ends {
        assert_eq!(case.kind(), "add_months", "line {}", case.line);
        let offset: i32 = case.get("off").parse().expect("the offset is a number");
        let (instant, parsed_offset) = parse_rfc3339(case.get("in"));
        assert_eq!(parsed_offset, offset, "line {}", case.line);

        let clock = FixedWallClock::new(instant, offset);
        let claims = Claims::new(&Requestor::Unknown, "token-id", &clock);

        assert_eq!(claims.iat, case.get("in"), "line {}", case.line);
        assert_eq!(claims.expires, case.output, "line {}", case.line);

        // The day of the month moving is the normalisation, which is what a
        // clamping implementation would get wrong.
        if claims.expires[8..10] != claims.iat[8..10] {
            normalized += 1;
        }
    }

    assert!(
        normalized > 0,
        "no recorded addmonth case rolls past the end of the target month, so this test \
         proves nothing about normalisation"
    );
}

// ---------------------------------------------------------------------------
// The token id
// ---------------------------------------------------------------------------

/// The two fixed nibbles, against a draw this test chooses, so the version and
/// variant bits are pinned rather than inferred from a random id.
#[test]
fn the_token_id_carries_the_version_and_variant_a_uuid_v4_has() {
    // Every bit set, so both overwrites are visible: the version nibble drops
    // from `f` to `4` and the variant bits from `11` to `10`.
    assert_eq!(
        uuid_v4(&[0xff; 16]),
        "ffffffff-ffff-4fff-bfff-ffffffffffff",
        "the version nibble or the variant bits are not the ones RFC 4122 names"
    );
    // Every bit clear, which is the placeholder shape the probe records.
    assert_eq!(uuid_v4(&[0x00; 16]), "00000000-0000-4000-8000-000000000000");
    // Nothing outside bytes 6 and 8 is touched.
    let mut draw = [0u8; 16];
    for (index, byte) in draw.iter_mut().enumerate() {
        *byte = u8::try_from(index).expect("sixteen fits in a u8") * 0x11;
    }
    assert_eq!(uuid_v4(&draw), "00112233-4455-4677-8899-aabbccddeeff");
}

/// The shape, and that two draws differ.
#[test]
fn a_drawn_token_id_has_the_shape_and_is_not_the_same_twice() {
    let first = generate_token_id().expect("the OS random source refused");
    let second = generate_token_id().expect("the OS random source refused");
    assert_ne!(
        first, second,
        "two token ids are the same, so the draw is not random"
    );

    for id in [&first, &second] {
        assert_eq!(id.len(), 36, "{id} is not 36 characters");
        let bytes = id.as_bytes();
        for at in [8, 13, 18, 23] {
            assert_eq!(bytes[at], b'-', "{id} has no hyphen at {at}");
        }
        assert_eq!(bytes[14], b'4', "{id} does not name version 4");
        assert!(
            matches!(bytes[19], b'8' | b'9' | b'a' | b'b'),
            "{id} does not name the RFC 4122 variant"
        );
        for (at, byte) in bytes.iter().enumerate() {
            if matches!(at, 8 | 13 | 18 | 23) {
                continue;
            }
            assert!(
                byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
                "{id} is not lowercase hex at {at}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The segments
// ---------------------------------------------------------------------------

/// `jwt.EncodeSegment` is `base64.RawURLEncoding`: the URL alphabet and no
/// padding (`golang-jwt/jwt@v3.2.2/token.go:97-99`).
///
/// The payload used here is a real claim set whose `requestor_id` carries a run
/// of `?` and `~`. Those are the only two ASCII bytes that can put a 62 or a 63
/// in a base64 group, and a run of them guarantees one of each lands in the
/// third byte of a group, so the standard alphabet really does produce a `+`
/// and a `/` for this payload and the URL alphabet really is what keeps them
/// out of the token.
#[test]
fn a_segment_uses_the_url_alphabet_and_carries_no_padding() {
    let clock = FixedWallClock::new(WallTime::new(0, 0), 0);
    let claims = Claims::new(&Requestor::Robot(Esn::new("?~?~?~?~")), "t", &clock);
    let payload = marshal_claims(&claims);

    let standard = STANDARD.encode(&payload);
    assert!(
        standard.contains('+') && standard.contains('/'),
        "this payload does not exercise the two characters the alphabets disagree on: {standard}"
    );
    assert!(
        standard.contains('='),
        "this payload does not exercise padding: {standard}"
    );

    let segment = encode_segment(&payload);
    assert!(!segment.contains('+'), "a segment carries a +: {segment}");
    assert!(!segment.contains('/'), "a segment carries a /: {segment}");
    assert!(!segment.contains('='), "a segment is padded: {segment}");
    assert!(
        segment.contains('-') && segment.contains('_'),
        "a segment does not use the URL alphabet: {segment}"
    );
    assert_eq!(
        segment,
        standard
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_"),
        "a segment is not the standard encoding with the URL alphabet and no padding"
    );
}

/// Three segments, joined by dots, with the signing input as the first two.
#[test]
fn a_token_is_three_dot_joined_segments() {
    let recorded = claims_by_input();
    let claims = recorded_claims(&recorded);
    let payload = marshal_claims(&claims);
    let signature = random_signature().expect("the OS random source refused");

    let token = encode(HEADER, &payload, &signature);
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(segments.len(), 3, "a token is not three segments: {token}");
    assert_eq!(
        format!("{}.{}", segments[0], segments[1]),
        signing_input(HEADER, &payload),
        "the first two segments are not the signing input"
    );
    assert_eq!(segments[2], encode_segment(&signature));
}

/// The signature slot is [`SIGNATURE_LEN`] drawn bytes, which is the length an
/// RS512 signature over Go's 1024-bit key has, and two tokens issued from the
/// same claims differ in it and nowhere else. This is deviation 28.
#[test]
fn the_signature_slot_is_drawn_and_differs_between_two_tokens() {
    assert_eq!(
        SIGNATURE_LEN, 128,
        "an RS512 signature over the 1024-bit key at token.go:266 is 128 bytes"
    );

    let first = random_signature().expect("the OS random source refused");
    let second = random_signature().expect("the OS random source refused");
    assert_eq!(first.len(), SIGNATURE_LEN);
    assert_ne!(
        first, second,
        "two signatures are the same, so the draw is not random"
    );

    let recorded = claims_by_input();
    let claims = recorded_claims(&recorded);
    let one = issue_token(&claims).expect("the OS random source refused");
    let two = issue_token(&claims).expect("the OS random source refused");
    assert_ne!(one, two, "two tokens from the same claims are identical");

    let head = |token: &str| {
        let mut parts = token.splitn(3, '.');
        let header = parts
            .next()
            .expect("a token has a header segment")
            .to_owned();
        let payload = parts
            .next()
            .expect("a token has a payload segment")
            .to_owned();
        (header, payload)
    };
    assert_eq!(
        head(&one),
        head(&two),
        "two tokens from the same claims differ outside the signature"
    );

    let signature = one.rsplit('.').next().expect("a token has a third segment");
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature)
            .expect("the signature segment is base64url")
            .len(),
        SIGNATURE_LEN,
        "the signature segment does not decode to {SIGNATURE_LEN} bytes"
    );
}

/// The bundle mirrors the two fields Go sets on `tokenpb.TokenBundle`
/// (`token.go:244`, `token.go:268`) without this crate depending on the
/// generated message.
#[test]
fn the_bundle_carries_the_two_fields_go_sets() {
    let bundle = TokenBundle {
        token: "a.b.c".to_owned(),
        client_token_guid: "guid".to_owned(),
    };
    assert_eq!(bundle.token, "a.b.c");
    assert_eq!(bundle.client_token_guid, "guid");
    assert_eq!(TokenBundle::default(), TokenBundle::default());
}

// ---------------------------------------------------------------------------
// The stored hash
// ---------------------------------------------------------------------------

/// A directory under the system temporary directory, removed when the test
/// ends.
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
            "wirepod-jwt-{label}-{}-{nanos:x}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("could not create the temporary directory");
        Self { path }
    }

    /// Where the store writes.
    fn jdocs_file(&self) -> PathBuf {
        self.path.join("jdocs.json")
    }

    /// The bytes currently in the file.
    fn file(&self) -> Vec<u8> {
        fs::read(self.jdocs_file()).expect("jdocs.json is missing")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A store in its own temporary directory, and a clock stopped at the instant
/// the recording's `iat` names.
fn store_and_clock(label: &str) -> (TempDir, JdocsStore, FixedWallClock) {
    let recorded = claims_by_input();
    let iat = claim(&recorded, "kind=value name=iat");
    let (instant, offset) = parse_rfc3339(&iat);
    assert_eq!(
        rfc3339_nano(instant, offset),
        iat,
        "the recorded iat did not survive being parsed and formatted again"
    );

    let directory = TempDir::new(label);
    let store = JdocsStore::new(
        directory
            .jdocs_file()
            .to_str()
            .expect("the temporary path is UTF-8")
            .to_owned(),
    );
    (directory, store, FixedWallClock::new(instant, offset))
}

/// The serial the probe's placeholder requestor id carries, which is not a
/// robot this machine has ever seen.
const TEST_ESN: &str = "00000000";

/// `token.go:99-128`: the document lands under `vic:` plus the serial, at
/// version one, with the new-token metadata, and its `json_doc` is the bytes Go
/// marshals.
#[tokio::test]
async fn write_token_hash_writes_the_document_go_writes() {
    let (directory, store, clock) = store_and_clock("write");

    tokio::time::timeout(
        CEILING,
        write_token_hash(&store, TEST_ESN, PLACEHOLDER_HASH, &clock),
    )
    .await
    .expect("write_token_hash did not finish")
    .expect("the rewrite failed");

    let docs = store.snapshot();
    assert_eq!(docs.len(), 1, "one call wrote more than one document");
    let entry = &docs[0];
    assert_eq!(
        entry.thing,
        format!("vic:{TEST_ESN}"),
        "token.go:125 stores under `vic:` plus the serial"
    );
    assert_eq!(entry.name, APP_TOKENS_DOC, "token.go:125");
    // The literal, not the constant, because a constant asserted against
    // itself would move with a change to it. `doc_version` also carries
    // `omitempty` (`vars.go:131`), so a zero here would drop the key from the
    // file the robot reads back.
    assert_eq!(
        NEW_TOKEN_VERSION, 1,
        "token.go:104-105 writes the literal 1"
    );
    assert_eq!(entry.jdoc.doc_version, 1, "token.go:104");
    assert_eq!(entry.jdoc.fmt_version, 1, "token.go:105");
    assert_eq!(
        entry.jdoc.client_metadata, NEW_TOKEN_METADATA,
        "token.go:106"
    );
    assert_eq!(
        entry.jdoc.json_doc, GO_APP_TOKENS_DOC,
        "the document is not the bytes Go's json.Marshal writes"
    );

    // The lookup at `token.go:101` uses the bare serial and so never finds what
    // `token.go:125` stored under the prefixed one.
    assert!(
        store.get_jdoc(TEST_ESN, APP_TOKENS_DOC).is_none(),
        "the bare serial found the document, so the two spellings have been reconciled"
    );
    assert!(
        store
            .get_jdoc(&format!("vic:{TEST_ESN}"), APP_TOKENS_DOC)
            .is_some(),
        "the prefixed serial did not find the document"
    );

    // And the file is what C8's decoder reads back.
    let parsed = parse_jdocs(&directory.file()).expect("the file is not the shape Go writes");
    assert_eq!(parsed, docs, "the file and the list disagree");
}

/// The document never accumulates: the lookup at `token.go:101` never finds
/// what `token.go:125` stored, so every call starts from the blank document and
/// the robot's `vic.AppTokens` holds exactly one client token at version one
/// however many times it authenticates.
#[tokio::test]
async fn a_second_write_replaces_the_document_rather_than_appending_to_it() {
    let (directory, store, clock) = store_and_clock("accumulate");

    for _ in 0..2 {
        tokio::time::timeout(
            CEILING,
            write_token_hash(&store, TEST_ESN, PLACEHOLDER_HASH, &clock),
        )
        .await
        .expect("write_token_hash did not finish")
        .expect("the rewrite failed");
    }

    let docs = store.snapshot();
    assert_eq!(docs.len(), 1, "the second call appended a second document");
    assert_eq!(
        docs[0].jdoc.doc_version, 1,
        "the version moved, so the second call found the first call's document"
    );
    assert_eq!(
        docs[0].jdoc.json_doc, GO_APP_TOKENS_DOC,
        "the second call did not start from the blank document"
    );

    let manager: ClientTokenManager =
        serde_json::from_str(&docs[0].jdoc.json_doc).expect("the document is a client token list");
    assert_eq!(
        manager.client_tokens.len(),
        1,
        "the document accumulated a second client token, which Go's never does"
    );
    assert_eq!(manager.client_tokens[0].hash, PLACEHOLDER_HASH);
    assert_eq!(manager.client_tokens[0].client_name, "wirepod");
    assert_eq!(manager.client_tokens[0].app_id, "SDK");

    let parsed = parse_jdocs(&directory.file()).expect("the file is not the shape Go writes");
    assert_eq!(parsed, docs, "the file and the list disagree");
}

/// A second robot gets its own document rather than sharing the first's.
#[tokio::test]
async fn two_robots_get_two_documents() {
    let (_directory, store, clock) = store_and_clock("two-robots");

    for esn in ["00000000", "00000001"] {
        tokio::time::timeout(
            CEILING,
            write_token_hash(&store, esn, PLACEHOLDER_HASH, &clock),
        )
        .await
        .expect("write_token_hash did not finish")
        .expect("the rewrite failed");
    }

    let docs = store.snapshot();
    assert_eq!(docs.len(), 2, "the two robots shared one document");
    assert_eq!(docs[0].thing, "vic:00000000");
    assert_eq!(docs[1].thing, "vic:00000001");
}
