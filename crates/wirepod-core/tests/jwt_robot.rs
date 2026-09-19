//! The other end of the wire: what the robot does with the token this server
//! hands it, table driven from the recorded probe output.
//!
//! Every other test in this crate asks whether the port writes the bytes Go
//! writes. This one asks the question that actually matters on the wire:
//! whether a robot would accept them. The robot parses with
//! `new(jwt.Parser).ParseUnverified`
//! (`vector-cloud/internal/token/identity/identity.go:158`) and then reads the
//! claims with `FromJwtToken`
//! (`vector-cloud/internal/token/identity/token.go:96-161`). The
//! `robot_parse` section of the probe runs that pair over thirty-five crafted
//! tokens and records what it answers; [`accept`] below is the same pair in
//! Rust, and [`the_acceptor_agrees_with_every_recorded_verdict`] is what holds
//! the two together.
//!
//! # The module the recording substitutes
//!
//! The robot builds against `github.com/dgrijalva/jwt-go`
//! v3.2.1-0.20180719211823-0b96aaa70776+incompatible
//! (`vector-cloud/go.mod:8`), which is not in this machine's module cache. The
//! probe runs `github.com/golang-jwt/jwt` v3.2.2+incompatible instead, the
//! maintained fork of the same code at the same major version and the one the
//! Go server itself pins (`chipper/go.mod:16`). Both module paths are recorded
//! as constants and the census below asserts they are still there.
//!
//! One recorded verdict is known to be able to differ between the two.
//! `padded_payload_segment` is golang-jwt's answer: its `DecodeSegment`
//! (`token.go:102-104`) is `base64.RawURLEncoding`, which refuses a `=`
//! outright, while the older dgrijalva build re-pads the segment before
//! decoding and may accept it. Nothing this port writes is padded, so the
//! difference is unreachable from a token wire-pod issues.
//!
//! # Where this acceptor is not the robot
//!
//! [`accept`] is a model of the robot's reader, not a translation of it, and
//! it is deliberately narrower in six places. Every one of them refuses
//! something the robot would have taken, none of them takes something the
//! robot would have refused, and nothing this port writes reaches any of them.
//! They are listed here so the next reader finds them as decisions rather than
//! as bugs. All six are candidate numbered deviations for the docs commit.
//!
//! The first is base64 strictness, and it is the same difference the token
//! hash decoder records (`token/hash.rs`, `TokenHashError::Decode`). Go's
//! `base64` only checks the bits left over in the last group when the encoding
//! is `Strict()`, and `DecodeSegment` uses the plain `RawURLEncoding`
//! (`base64.go:394-396` and `:401-403` are the checks and the `enc.strict`
//! guard on them), so Go decodes `QR` to `[65]` and `eyC` to `[123 32]` while
//! [`decode_segment`] refuses both. A segment Go's own encoder wrote never
//! leaves those bits set, so nothing on the wire is affected.
//!
//! The next four are the two JSON readers `parser.go` uses. The payload goes
//! through `json.NewDecoder(..).Decode` (`parser.go:123-136`), which reads one
//! JSON value and leaves whatever follows it unread, and the header through
//! `json.Unmarshal` (`parser.go:112`); decoding a JSON `null` into a map is a
//! no-op in both rather than an error. This file reads each segment as one
//! whole `serde_json::Value` and requires an object, so four recorded cases
//! part company, each one listed in [`DISCLOSED_DIVERGENCES`] and asserted
//! there:
//!
//! | case | the robot | this acceptor |
//! |---|---|---|
//! | `payload_null` | `missing claim token_id` | `claims not object` |
//! | `payload_trailing_bytes` | `ok` | `claims not object` |
//! | `payload_second_value` | `ok` | `claims not object` |
//! | `header_null` | `signing method (alg) is unspecified.` | `header not json` |
//!
//! The sixth is `time.ParseInLocation`'s fallback. [`parses_as_rfc3339`]
//! models Go's `parseRFC3339` fast path only, and Go falls back to its general
//! parser when that path fails, which reaches three spellings this file
//! refuses: a one-digit hour (`2026-09-09T2:34:56Z`), a zone field in the
//! general parser's wider range (`+24:00` and `+23:60` are accepted, `+30:00`
//! and `+23:99` are not), and a comma in place of the decimal point
//! (`2026-09-09T12:34:56,5Z`). [`rfc3339_nano`] cannot write any of the three:
//! it always pads the hour to two digits, always writes an offset out of a
//! real zone, and always writes a `.`.
//!
//! # What the verdicts are
//!
//! A closed vocabulary ([`Verdict`]), never an error message from
//! `encoding/json`, `time` or `encoding/base64`: those strings move with the
//! Go toolchain, nothing on the wire carries them, and the robot only ever
//! branches on whether the parse failed. The three that are literals are
//! verbatim, and named at [`Verdict::text`].
//!
//! Integration tests are separate binaries and cannot share helpers, so the
//! recording parser below is copied from `tests/jwt.rs` rather than imported.
//! That is the convention here. Nothing in this file binds a port, touches the
//! disk, or contacts a robot.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use wirepod_core::gojson::go_marshal;
use wirepod_core::timefmt::rfc3339_nano;
use wirepod_core::token::jwt::{
    Claims, HEADER, Requestor, TOKEN_TYPE, USER_ID, encode_segment, issue_token, marshal_claims,
    random_signature, signing_input,
};
use wirepod_core::wallclock::{FixedWallClock, WallTime};

const EXPECTED: &str = include_str!("data/go-probe/expected.txt");

/// How many crafted tokens the `robot_parse` section runs.
const ROBOT_VERDICTS: usize = 35;

/// How many constants it records: the required claims, the optional one, the
/// parse layout and the two module paths.
const ROBOT_CONSTS: usize = 5;

/// Every line of the section.
const ROBOT_LINES: usize = 40;

/// How many random claim sets [`every_issued_token_is_one_the_robot_accepts`]
/// draws on top of the recorded ones.
const RANDOM_DRAWS: usize = 200;

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

/// Decodes one lowercase-hex field of the recording.
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
// The verdict vocabulary
// ---------------------------------------------------------------------------

/// What the robot's reader answers, as a closed set.
///
/// An enum rather than a string so the vocabulary is closed in the type
/// system: a new outcome has to be added here before it can be produced.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Verdict {
    /// The token parsed and every required claim was there.
    Ok,
    /// `parser.go:97-100`: the token was not three dot-separated parts.
    Segments,
    /// `parser.go:106` or `:120` through `DecodeSegment`, which is
    /// `base64.RawURLEncoding` (`token.go:102-104`) and refuses padding.
    Base64,
    /// `parser.go:112-114`: the header did not unmarshal into a map.
    HeaderNotJson,
    /// `parser.go:123-136`: the payload did not decode into a claim map.
    ClaimsNotObject,
    /// `identity/token.go:126` or `:135`: `time.ParseInLocation` refused the
    /// timestamp.
    TimeParse,
    /// `parser.go:107-108`, verbatim.
    Bearer,
    /// `parser.go:141`, verbatim.
    AlgUnavailable,
    /// `parser.go:144`, verbatim.
    AlgUnspecified,
    /// `identity/token.go:167-169`, verbatim, for the claim it names.
    MissingClaim(&'static str),
}

impl Verdict {
    /// The recording's spelling of this verdict.
    fn text(&self) -> String {
        match self {
            // The recording's spelling of a nil error, per the phase README.
            Self::Ok => "ok".to_owned(),
            Self::Segments => "segments".to_owned(),
            Self::Base64 => "base64 error".to_owned(),
            Self::HeaderNotJson => "header not json".to_owned(),
            Self::ClaimsNotObject => "claims not object".to_owned(),
            Self::TimeParse => "time parse error".to_owned(),
            Self::Bearer => "tokenstring should not contain 'bearer '".to_owned(),
            Self::AlgUnavailable => "signing method (alg) is unavailable.".to_owned(),
            Self::AlgUnspecified => "signing method (alg) is unspecified.".to_owned(),
            Self::MissingClaim(name) => format!("missing claim {name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// The acceptor
// ---------------------------------------------------------------------------

/// The claims `FromJwtToken` requires, in the order it reads them
/// (`identity/token.go:101`, `:106`, `:111`, `:116`, `:121`, `:131`). Every one
/// is read with a type assertion to `string`, so a claim that is present but is
/// a number, an object or null fails exactly the way a missing one does. The
/// six lines cited are the assertions themselves; each one's
/// `errorMissingClaim` sits two lines below it.
const REQUIRED_CLAIMS: [&str; 6] = [
    "token_id",
    "token_type",
    "user_id",
    "requestor_id",
    "iat",
    "expires",
];

/// The signing methods `github.com/golang-jwt/jwt` v3.2.2 registers in its
/// package `init` functions, which is the set `GetSigningMethod`
/// (`parser.go:140`) can answer for: `ecdsa.go:35-47`, `ed25519.go:24`,
/// `hmac.go:27-39`, `none.go:18`, `rsa.go:26-38` and `rsa_pss.go:43-77`.
const REGISTERED_ALGS: [&str; 14] = [
    "ES256", "ES384", "ES512", "EdDSA", "HS256", "HS384", "HS512", "none", "PS256", "PS384",
    "PS512", "RS256", "RS384", "RS512",
];

/// `base64.RawURLEncoding.DecodeString`, which is what `DecodeSegment`
/// (`golang-jwt/jwt@v3.2.2/token.go:102-104`) is.
///
/// Written out rather than taken from the `base64` crate so that the padding
/// rule is this file's assertion and not a crate configuration flag: the URL
/// alphabet, no padding accepted, a trailing group of one character rejected,
/// and the leftover bits of the last group required to be zero.
///
/// The first three are what Go's decoder does. The fourth is deliberately
/// stricter than Go, in the safe direction: `RawURLEncoding` is not
/// `Strict()`, and the checks that would reject non-zero leftover bits are
/// guarded by `enc.strict` (Go's `encoding/base64/base64.go:394-396` and
/// `:401-403`), so Go decodes `QR` to `[65]` and `eyC` to `[123 32]` where
/// this refuses both. Refusing them can only turn a token the robot would have
/// read into one this acceptor does not, never the reverse, and Go's own
/// encoder never writes a segment with those bits set, so no token this server
/// issues can reach the difference. It is disclosed in the module doc above.
fn decode_segment(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    for byte in text.bytes() {
        let value: u8 = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            // '=' lands here with everything else: raw encoding has no padding
            // and refuses to read any.
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((accumulator >> bits) & 0xff).expect("eight bits are a byte"));
        }
    }
    // Six leftover bits mean a trailing group of one character, which no input
    // can produce and which Go refuses too. The second half of the test is the
    // stricter one: the leftover bits of a well-formed group are zero, and Go
    // only checks that under `Strict()`. See the doc comment above.
    if bits >= 6 || accumulator & ((1u32 << bits) - 1) != 0 {
        return None;
    }
    Some(out)
}

/// How many days a month has, for the day range `parseRFC3339` checks.
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Whether `time.ParseInLocation(time.RFC3339, ...)` accepts this timestamp
/// (`identity/token.go:126`, `:135`).
///
/// This is Go's `parseRFC3339` fast path, which is the one that runs for the
/// `time.RFC3339` layout: `YYYY-MM-DDTHH:MM:SS`, an optional `.` and at least
/// one fraction digit, then either `Z` or a signed `HH:MM`, with every field
/// range-checked and the day checked against the month.
///
/// Go falls back to its general parser when that path fails, and the fallback
/// is not modelled here, so three spellings the robot takes are refused: a
/// one-digit hour (`2026-09-09T2:34:56Z`), a zone in the general parser's
/// wider range (`+24:00` and `+23:60` accepted, `+30:00` and `+23:99` not),
/// and a comma for the decimal point (`2026-09-09T12:34:56,5Z`). None is a
/// shape [`rfc3339_nano`] can write, all three are refusals rather than
/// acceptances, and the module doc lists them with the other five differences.
fn parses_as_rfc3339(text: &str) -> bool {
    // Every byte Go's `time.RFC3339` layout can match is ASCII, so a timestamp
    // carrying a multibyte character cannot parse under either parser. Saying
    // so here is also what keeps the byte-index slicing below from splitting a
    // character: without it, a two-byte character starting at byte eighteen
    // makes `&text[17..19]` panic on a boundary that is not one.
    if !text.is_ascii() {
        return false;
    }
    let bytes = text.as_bytes();
    if bytes.len() < "2006-01-02T15:04:05".len() {
        return false;
    }
    let field = |range: std::ops::Range<usize>, low: i64, high: i64| -> Option<i64> {
        let piece = &text[range];
        if !piece.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let value: i64 = piece.parse().ok()?;
        (low..=high).contains(&value).then_some(value)
    };
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return false;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return false;
    }
    let Some(year) = field(0..4, 0, 9999) else {
        return false;
    };
    let Some(month) = field(5..7, 1, 12) else {
        return false;
    };
    let last = i64::from(days_in_month(
        year,
        u32::try_from(month).expect("1..=12 fits"),
    ));
    if field(8..10, 1, last).is_none()
        || field(11..13, 0, 23).is_none()
        || field(14..16, 0, 59).is_none()
        || field(17..19, 0, 59).is_none()
    {
        return false;
    }

    let mut rest = &text[19..];
    if let Some(after) = rest.strip_prefix('.') {
        let digits = after
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after.len());
        if digits == 0 {
            return false;
        }
        rest = &after[digits..];
    }

    if rest == "Z" {
        return true;
    }
    if rest.len() != "-07:00".len() {
        return false;
    }
    let zone = rest.as_bytes();
    if (zone[0] != b'-' && zone[0] != b'+') || zone[3] != b':' {
        return false;
    }
    let hours = &rest[1..3];
    let minutes = &rest[4..6];
    let numeric = |piece: &str, high: i64| -> bool {
        piece.chars().all(|c| c.is_ascii_digit())
            && piece.parse::<i64>().map(|v| v <= high).unwrap_or(false)
    };
    numeric(hours, 23) && numeric(minutes, 59)
}

/// `identity.go:158` followed by `identity/token.go:96-161`, in the order the
/// two run.
///
/// The signature segment is split off and never decoded, which is the whole
/// reason this port can fill it with drawn bytes (deviation 28).
fn accept(token: &str) -> Verdict {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Verdict::Segments;
    }

    // `parser.go:105-111`, with the one error message that names the caller's
    // mistake instead of the library's.
    let Some(header_bytes) = decode_segment(parts[0]) else {
        if token.to_ascii_lowercase().starts_with("bearer ") {
            return Verdict::Bearer;
        }
        return Verdict::Base64;
    };
    // `parser.go:112-114`: `token.Header` is a `map[string]interface{}`, so
    // anything that is not a JSON object fails to unmarshal into it.
    let Ok(Value::Object(header)) = serde_json::from_slice::<Value>(&header_bytes) else {
        return Verdict::HeaderNotJson;
    };

    // `parser.go:116-136`.
    let Some(claim_bytes) = decode_segment(parts[1]) else {
        return Verdict::Base64;
    };
    let Ok(Value::Object(claims)) = serde_json::from_slice::<Value>(&claim_bytes) else {
        return Verdict::ClaimsNotObject;
    };

    // `parser.go:138-145`, which runs after the claims are decoded and before
    // `FromJwtToken` ever sees them.
    match header.get("alg").and_then(Value::as_str) {
        Some(alg) if REGISTERED_ALGS.contains(&alg) => {}
        Some(_) => return Verdict::AlgUnavailable,
        None => return Verdict::AlgUnspecified,
    }

    // `identity/token.go:103-138`, in that order.
    for name in REQUIRED_CLAIMS {
        let present = claims.get(name).and_then(Value::as_str);
        match (name, present) {
            (_, None) => {
                let named = REQUIRED_CLAIMS
                    .iter()
                    .find(|candidate| **candidate == name)
                    .expect("the name came out of this array");
                return Verdict::MissingClaim(named);
            }
            // `:126` and `:135`: each timestamp is parsed the moment it is
            // read, before the next claim is looked at.
            ("iat" | "expires", Some(value)) => {
                if !parses_as_rfc3339(value) {
                    return Verdict::TimeParse;
                }
            }
            _ => {}
        }
    }

    // `identity/token.go:153-156`: `permissions` is optional and only a JSON
    // object populates it. Null, an array, a string and a missing key all
    // leave the field nil, and none of them is an error.
    let _ = claims.get("permissions").and_then(Value::as_object);

    Verdict::Ok
}

// ---------------------------------------------------------------------------
// Rebuilding the crafted tokens
// ---------------------------------------------------------------------------

/// The fixed placeholder claim set the probe crafts from. None of these is a
/// live value: the serial is all zeros and the token id is the all-zero UUID
/// with the nibbles a v4 carries.
fn base_claims() -> BTreeMap<String, Value> {
    [
        (
            "expires",
            Value::from("2026-10-09T12:34:56.789012345-07:00"),
        ),
        ("iat", Value::from("2026-09-09T12:34:56.789012345-07:00")),
        ("permissions", Value::Null),
        ("requestor_id", Value::from("vic:00000000")),
        (
            "token_id",
            Value::from("00000000-0000-4000-8000-000000000000"),
        ),
        ("token_type", Value::from("user+robot")),
        ("user_id", Value::from("wirepod")),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect()
}

/// One claim set, marshalled the way `encoding/json` marshals a
/// `map[string]interface{}`: keys in byte order, and Go's five HTML escapes.
fn payload(mutate: impl FnOnce(&mut BTreeMap<String, Value>)) -> Vec<u8> {
    let mut claims = base_claims();
    mutate(&mut claims);
    go_marshal(&claims).expect("a claim map holds nothing unserialisable")
}

/// The signature segment every crafted token carries. Never a real signature:
/// nothing in the robot's path decodes this segment.
fn placeholder_signature() -> String {
    encode_segment(b"probe-placeholder")
}

/// Assembles three segments the way `SignedString` does.
fn assemble(header: &[u8], claims: &[u8]) -> String {
    format!(
        "{}.{}",
        signing_input(header, claims),
        placeholder_signature()
    )
}

/// The thirty-five crafted tokens, rebuilt here exactly as the probe builds
/// them, keyed by the case name the recording gives each.
fn crafted_tokens() -> BTreeMap<&'static str, String> {
    let valid_payload = payload(|_| {});
    let header_segment = encode_segment(HEADER);
    let payload_segment = encode_segment(&valid_payload);
    let valid = assemble(HEADER, &valid_payload);

    let with_iat = |value: &'static str| {
        assemble(
            HEADER,
            &payload(|claims| {
                claims.insert("iat".to_owned(), Value::from(value));
            }),
        )
    };
    let without = |name: &'static str| {
        assemble(
            HEADER,
            &payload(|claims| {
                claims.remove(name);
            }),
        )
    };
    let with = |name: &'static str, value: Value| {
        assemble(
            HEADER,
            &payload(|claims| {
                claims.insert(name.to_owned(), value);
            }),
        )
    };

    // The padded segment, the probe's rule verbatim: `base64.URLEncoding` over
    // the same payload, plus one insignificant trailing space when the length
    // is a multiple of three so that the padded encoder really emits a `=`.
    let mut padded_source = valid_payload.clone();
    if padded_source.len().is_multiple_of(3) {
        padded_source.push(b' ');
    }
    let padded_segment = pad_url_base64(&padded_source);
    assert!(
        padded_segment.contains('='),
        "the padded payload segment carries no padding, so the case proves nothing"
    );

    [
        ("valid", valid.clone()),
        ("missing_token_id", without("token_id")),
        ("missing_token_type", without("token_type")),
        ("missing_user_id", without("user_id")),
        ("missing_requestor_id", without("requestor_id")),
        ("missing_iat", without("iat")),
        ("missing_expires", without("expires")),
        ("numeric_iat", with("iat", Value::from(1_789_000_496i64))),
        (
            "numeric_expires",
            with("expires", Value::from(1_791_592_496i64)),
        ),
        ("null_permissions", with("permissions", Value::Null)),
        (
            "object_permissions",
            with("permissions", serde_json::json!({"robot": true})),
        ),
        (
            "array_permissions",
            with("permissions", serde_json::json!(["robot"])),
        ),
        ("empty_user_id", with("user_id", Value::from(""))),
        ("iat_fraction_0", with_iat("2026-09-09T12:34:56-07:00")),
        ("iat_fraction_3", with_iat("2026-09-09T12:34:56.789-07:00")),
        (
            "iat_fraction_9",
            with_iat("2026-09-09T12:34:56.789012345-07:00"),
        ),
        ("iat_zulu", with_iat("2026-09-09T12:34:56Z")),
        (
            "iat_plus_zero_offset",
            with_iat("2026-09-09T12:34:56+00:00"),
        ),
        ("iat_no_zone", with_iat("2026-09-09T12:34:56")),
        ("iat_space_separator", with_iat("2026-09-09 12:34:56Z")),
        (
            "two_segments",
            format!("{header_segment}.{payload_segment}"),
        ),
        ("four_segments", format!("{valid}.extra")),
        (
            "empty_signature_segment",
            format!("{header_segment}.{payload_segment}."),
        ),
        (
            "garbage_signature_segment",
            format!("{header_segment}.{payload_segment}.this-is-not-a-signature"),
        ),
        (
            "padded_payload_segment",
            format!(
                "{header_segment}.{padded_segment}.{}",
                placeholder_signature()
            ),
        ),
        (
            "unknown_alg",
            assemble(br#"{"alg":"RS999","typ":"JWT"}"#, &valid_payload),
        ),
        ("missing_alg", assemble(br#"{"typ":"JWT"}"#, &valid_payload)),
        (
            "alg_none",
            assemble(br#"{"alg":"none","typ":"JWT"}"#, &valid_payload),
        ),
        ("header_not_json", assemble(b"[1,2]", &valid_payload)),
        (
            "payload_not_object",
            format!(
                "{header_segment}.{}.{}",
                encode_segment(b"[1,2]"),
                placeholder_signature()
            ),
        ),
        ("bearer_prefix", format!("bearer {valid}")),
        // The four the two JSON readers decide differently from this file.
        // Each is listed in [`DISCLOSED_DIVERGENCES`] with the verdict this
        // acceptor reaches, and the module doc says why.
        ("payload_null", assemble(HEADER, b"null")),
        ("payload_trailing_bytes", {
            let mut bytes = valid_payload.clone();
            bytes.extend_from_slice(b"zz");
            assemble(HEADER, &bytes)
        }),
        ("payload_second_value", {
            let mut bytes = valid_payload.clone();
            bytes.extend_from_slice(br#"{"a":1}"#);
            assemble(HEADER, &bytes)
        }),
        ("header_null", assemble(b"null", &valid_payload)),
    ]
    .into_iter()
    .collect()
}

/// The recorded cases where [`accept`] deliberately answers something other
/// than the robot does, with the verdict it answers.
///
/// Every entry is a refusal where the robot was lenient, so it can only turn a
/// token the robot would have read into one this acceptor does not. The module
/// doc explains each. [`the_acceptor_agrees_with_every_recorded_verdict`]
/// asserts both halves: that the acceptor reaches the verdict named here, and
/// that the verdict named here is not the recorded one, so a Go toolchain or
/// module that stopped diverging fails rather than going unnoticed.
const DISCLOSED_DIVERGENCES: [(&str, Verdict); 4] = [
    ("payload_null", Verdict::ClaimsNotObject),
    ("payload_trailing_bytes", Verdict::ClaimsNotObject),
    ("payload_second_value", Verdict::ClaimsNotObject),
    ("header_null", Verdict::HeaderNotJson),
];

/// `base64.URLEncoding`: the URL alphabet with padding, which is the one
/// encoder a token must never carry.
fn pad_url_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for group in bytes.chunks(3) {
        let mut block = [0u8; 3];
        block[..group.len()].copy_from_slice(group);
        let packed = (u32::from(block[0]) << 16) | (u32::from(block[1]) << 8) | u32::from(block[2]);
        for index in 0..4 {
            if index <= group.len() {
                let position = usize::try_from((packed >> (18 - 6 * index)) & 0x3f)
                    .expect("six bits index the alphabet");
                out.push(char::from(ALPHABET[position]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The census
// ---------------------------------------------------------------------------

/// Nothing in the section may go untested, and every case name has to be one
/// this file rebuilds.
#[test]
fn the_robot_parse_section_is_exactly_the_cases_this_file_covers() {
    assert_eq!(
        ROBOT_LINES,
        ROBOT_CONSTS + ROBOT_VERDICTS,
        "the parts of the census no longer add up to the whole"
    );

    let cases = recorded_cases();
    let lines = section(&cases, "robot_parse");
    assert_eq!(
        lines.len(),
        ROBOT_LINES,
        "the robot_parse section is no longer {ROBOT_LINES} lines"
    );

    let constants: BTreeSet<&str> = lines
        .iter()
        .filter(|case| case.kind() == "const")
        .map(|case| case.get("name"))
        .collect();
    assert_eq!(
        constants,
        BTreeSet::from([
            "required_claims",
            "optional_claims",
            "parse_layout",
            "jwt_module",
            "robot_module",
        ]),
        "the constants the section records are not the ones this file reads"
    );

    let verdicts: BTreeSet<&str> = lines
        .iter()
        .filter(|case| case.kind() == "verdict")
        .map(|case| case.get("name"))
        .collect();
    assert_eq!(
        verdicts.len(),
        ROBOT_VERDICTS,
        "the section no longer holds {ROBOT_VERDICTS} distinct verdicts"
    );
    let rebuilt: BTreeSet<&str> = crafted_tokens().keys().copied().collect();
    assert_eq!(
        verdicts, rebuilt,
        "a recorded case is not rebuilt here, or a rebuilt one is not recorded"
    );

    // The three constants that describe the acceptor, against the acceptor.
    let value = |name: &str| -> String {
        lines
            .iter()
            .find(|case| case.kind() == "const" && case.get("name") == name)
            .unwrap_or_else(|| panic!("the section records no {name}"))
            .output
            .clone()
    };
    assert_eq!(
        value("required_claims"),
        REQUIRED_CLAIMS.join(","),
        "the required claims, and the order identity/token.go reads them in"
    );
    assert_eq!(
        value("optional_claims"),
        "permissions",
        "identity/token.go:153 reads exactly one optional claim"
    );
    assert_eq!(
        value("parse_layout"),
        "2006-01-02T15:04:05Z07:00",
        "identity/token.go:126 parses with time.RFC3339, not RFC3339Nano"
    );
    assert!(
        value("jwt_module").starts_with("github.com/golang-jwt/jwt "),
        "the recording no longer names the module the probe ran"
    );
    assert!(
        value("robot_module").starts_with("github.com/dgrijalva/jwt-go "),
        "the recording no longer names the module the robot builds against"
    );
}

// ---------------------------------------------------------------------------
// The acceptor against the recording
// ---------------------------------------------------------------------------

/// Every crafted token, rebuilt here and run through the Rust acceptor, has to
/// reach the verdict Go reached.
///
/// The tokens are rebuilt rather than recorded because a recorded token would
/// only prove that this file can copy a string. Rebuilding them means the
/// payload bytes come from [`go_marshal`] and the segments from
/// [`encode_segment`], so the assembly is under test as well as the reader.
///
/// The four cases in [`DISCLOSED_DIVERGENCES`] are held to the verdict that
/// table names instead, and to the recorded verdict not being it.
#[test]
fn the_acceptor_agrees_with_every_recorded_verdict() {
    let cases = recorded_cases();
    let lines = section(&cases, "robot_parse");
    let tokens = crafted_tokens();

    let mut checked = 0usize;
    let mut diverged = 0usize;
    for case in lines.iter().filter(|case| case.kind() == "verdict") {
        let name = case.get("name");
        let token = tokens
            .get(name)
            .unwrap_or_else(|| panic!("line {}: no token is rebuilt for {name}", case.number));
        match DISCLOSED_DIVERGENCES
            .iter()
            .find(|(disclosed, _)| *disclosed == name)
        {
            Some((_, verdict)) => {
                assert_eq!(
                    accept(token).text(),
                    verdict.text(),
                    "line {}: {name}: a disclosed divergence no longer answers \
                     what the module doc says it answers",
                    case.number
                );
                assert_ne!(
                    verdict.text(),
                    case.output,
                    "line {}: {name}: Go now agrees with this acceptor, so the \
                     disclosure in the module doc is stale",
                    case.number
                );
                diverged += 1;
            }
            None => assert_eq!(
                accept(token).text(),
                case.output,
                "line {}: {name}",
                case.number
            ),
        }
        checked += 1;
    }
    assert_eq!(checked, ROBOT_VERDICTS);
    assert_eq!(
        diverged,
        DISCLOSED_DIVERGENCES.len(),
        "a disclosed divergence names a case the recording no longer holds"
    );
}

/// A timestamp carrying a multibyte character is refused, not a panic.
///
/// [`parses_as_rfc3339`] reads its fields by byte index, and the seconds field
/// is the one where a character can straddle the end of the slice: a two-byte
/// character starting at byte eighteen leaves byte nineteen inside it, so
/// `&text[17..19]` would split it. The claim is a string out of a token
/// anybody can craft, so the guard is what stands between a malformed token
/// and a panic where a refusal belongs.
#[test]
fn a_timestamp_whose_seconds_field_holds_a_multibyte_character_is_refused() {
    // 'é' is two bytes, and "2026-09-09T12:34:5" is eighteen, so it occupies
    // bytes eighteen and nineteen.
    let straddling = "2026-09-09T12:34:5é";
    assert_eq!(
        straddling.len(),
        20,
        "the character is not where it belongs"
    );
    assert!(!straddling.is_char_boundary(19), "byte 19 is inside it");

    let token = assemble(
        HEADER,
        &payload(|claims| {
            claims.insert("iat".to_owned(), Value::from(straddling));
        }),
    );
    assert_eq!(accept(&token), Verdict::TimeParse);

    // And in the other timestamp, which is read by the same function one claim
    // later.
    let token = assemble(
        HEADER,
        &payload(|claims| {
            claims.insert("expires".to_owned(), Value::from(straddling));
        }),
    );
    assert_eq!(accept(&token), Verdict::TimeParse);
}

// ---------------------------------------------------------------------------
// The acceptor against what this port issues
// ---------------------------------------------------------------------------

/// A deterministic draw, so a failure is reproducible and a green run is not a
/// coincidence that will un-green itself on the next commit.
struct Draw(u64);

impl Draw {
    fn bits(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.bits() % bound
    }
}

/// Every token this port issues is one the robot accepts: the recorded claim
/// sets, and two hundred drawn ones on top of them.
///
/// The drawn instants span a century, the drawn offsets span the range a zone
/// can have, and the drawn serials are built from an alphabet that includes
/// every character `encoding/json` escapes plus a quote and a backslash, so a
/// serial that broke out of its JSON string would produce a payload the
/// acceptor could not decode.
#[test]
fn every_issued_token_is_one_the_robot_accepts() {
    // The recorded claim sets first, rebuilt from the `claims_matrix`
    // fixtures. The claim set is assembled field by field rather than through
    // `Claims::new` so this file needs no zone of its own; that the assembly is
    // the right one is asserted against the recorded payload immediately
    // below, and `tests/jwt_matrix.rs` is what drives `Claims::new` itself.
    let cases = recorded_cases();
    let lines = section(&cases, "claims_matrix");

    let mut requestors: BTreeMap<&str, String> = BTreeMap::new();
    for case in lines.iter().filter(|case| case.kind() == "requestor_hex") {
        let bytes = from_hex(&case.output, case.number);
        let id = String::from_utf8(bytes)
            .unwrap_or_else(|error| panic!("line {}: {error}", case.number));
        requestors.insert(case.get("name"), id);
    }
    let mut token_ids: BTreeMap<&str, String> = BTreeMap::new();
    for case in lines.iter().filter(|case| case.kind() == "token_id") {
        token_ids.insert(case.get("name"), case.output.clone());
    }
    let mut expires: BTreeMap<&str, String> = BTreeMap::new();
    for case in lines.iter().filter(|case| case.kind() == "expires") {
        expires.insert(case.get("case"), case.output.clone());
    }

    let mut recorded = 0usize;
    for case in lines.iter().filter(|case| case.kind() == "payload") {
        let name = case.get("case");
        let claims = Claims {
            expires: expires[name].clone(),
            iat: case.get("iat").to_owned(),
            permissions: (),
            requestor_id: requestors[case.get("requestor")].clone(),
            token_id: token_ids[case.get("token_id")].clone(),
            token_type: TOKEN_TYPE,
            user_id: USER_ID,
        };
        assert_eq!(
            String::from_utf8(marshal_claims(&claims)).expect("the payload is UTF-8"),
            case.output,
            "line {}: {name}: the claim set rebuilt here is not the recorded one",
            case.number
        );
        let token = issue_token(&claims).expect("the OS random source refused");
        assert_eq!(
            accept(&token),
            Verdict::Ok,
            "line {}: {name}: the robot would refuse a token this server issues",
            case.number
        );
        recorded += 1;
    }
    assert!(recorded > 0, "no recorded claim set was driven");

    // And two hundred drawn ones.
    const SERIAL_PIECES: [&str; 12] = [
        "0", "9", "a", "F", "<", ">", "&", "\"", "\\", "\u{2028}", "\u{2029}", "~",
    ];
    // 2000-01-01 through 2100-01-01, so no year is outside the four digits
    // `time.RFC3339Nano` writes and no instant is before the epoch.
    const FIRST: i64 = 946_684_800;
    const LAST: i64 = 4_102_444_800;

    let mut draw = Draw(0x2026_0909_1234_5678);
    for index in 0..RANDOM_DRAWS {
        let unix_secs = FIRST
            + i64::try_from(draw.below(u64::try_from(LAST - FIRST).expect("a century of seconds")))
                .expect("the span fits in an i64");
        let nanos = u32::try_from(draw.below(1_000_000_000)).expect("a fraction fits in a u32");
        // A quarter hour at a time, across the range a real zone can have.
        let steps = i32::try_from(draw.below(105)).expect("a small count fits in an i32");
        let offset_secs = (steps - 48) * 900;

        let requestor = if draw.below(4) == 0 {
            Requestor::Unknown
        } else {
            let length = draw.below(9);
            let mut serial = String::new();
            for _ in 0..length {
                let piece = usize::try_from(
                    draw.below(u64::try_from(SERIAL_PIECES.len()).expect("twelve fits in a u64")),
                )
                .expect("an index fits in a usize");
                serial.push_str(SERIAL_PIECES[piece]);
            }
            Requestor::Robot(serial)
        };

        let instant = WallTime::new(unix_secs, nanos);
        let clock = FixedWallClock::new(instant, offset_secs);
        let mut token_bytes = [0u8; 16];
        for byte in &mut token_bytes {
            *byte = u8::try_from(draw.below(256)).expect("a byte fits in a u8");
        }
        let claims = Claims::new(
            &requestor,
            wirepod_core::token::jwt::uuid_v4(&token_bytes),
            &clock,
        );
        assert_eq!(
            claims.iat,
            rfc3339_nano(instant, offset_secs),
            "draw {index}: the iat claim is not the instant it was built from"
        );

        let token = issue_token(&claims).expect("the OS random source refused");
        assert_eq!(
            accept(&token),
            Verdict::Ok,
            "draw {index}: the robot would refuse a token this server issues: \
             iat {}, expires {}, requestor {:?}",
            claims.iat,
            claims.expires,
            claims.requestor_id
        );
    }
}

/// The signature slot can be empty and the robot still parses the token, which
/// is the executable form of the argument deviation 28 rests on.
///
/// `ParseUnverified` splits the three segments and decodes the first two
/// (`parser.go:97-136`); nothing ever looks at the third, and no verifier
/// exists on either side of this wire. A slot of drawn bytes is therefore no
/// weaker than one holding a signature nobody checks, and an empty slot is the
/// limiting case that proves it.
#[test]
fn an_empty_signature_segment_still_parses_which_is_why_deviation_28_is_safe() {
    let claims = payload(|_| {});
    let headless = format!("{}.", signing_input(HEADER, &claims));
    assert_eq!(accept(&headless), Verdict::Ok);

    // And with drawn bytes in it, which is what this port actually writes.
    let signature = random_signature().expect("the OS random source refused");
    let drawn = format!(
        "{}.{}",
        signing_input(HEADER, &claims),
        encode_segment(&signature)
    );
    assert_eq!(accept(&drawn), Verdict::Ok);

    // The recording says the same, on a token the probe crafted.
    let cases = recorded_cases();
    let recorded = section(&cases, "robot_parse")
        .into_iter()
        .find(|case| case.kind() == "verdict" && case.get("name") == "empty_signature_segment")
        .expect("the section records no empty_signature_segment case");
    assert_eq!(recorded.output, "ok", "line {}", recorded.number);
}

/// `parser.go:97-100` counts the dot-separated parts before anything else, so
/// a token missing its signature slot entirely is refused where one holding
/// nothing is not.
#[test]
fn a_token_with_two_segments_is_rejected() {
    let claims = payload(|_| {});
    assert_eq!(accept(&signing_input(HEADER, &claims)), Verdict::Segments);

    let cases = recorded_cases();
    let recorded = section(&cases, "robot_parse")
        .into_iter()
        .find(|case| case.kind() == "verdict" && case.get("name") == "two_segments")
        .expect("the section records no two_segments case");
    assert_eq!(recorded.output, "segments", "line {}", recorded.number);
}

/// An empty `user_id` parses, which is exactly why it must never be written.
///
/// `FromJwtToken` is happy with it: the claim is a string, and the recording
/// says `ok`. The cost is one level up, at
/// `vector-cloud/internal/token/identity/identity.go:141-145`, where the robot
/// deletes its own token file at boot when the token it read back carries an
/// empty `user_id`. A server that wrote one would hand out tokens that work
/// until the robot restarts and then vanish.
#[test]
fn the_user_id_claim_is_never_empty_because_an_empty_one_deletes_the_robots_token_file() {
    assert!(
        !USER_ID.is_empty(),
        "token.go:31's UserId is empty, which identity.go:141-145 treats as a token to delete"
    );
    assert_eq!(USER_ID, "wirepod", "token.go:31");

    // Nothing can build a claim set with anything else in it: the field is a
    // `&'static str` fixed at [`USER_ID`].
    let clock = FixedWallClock::new(WallTime::new(1_788_980_400, 0), -7 * 3600);
    let claims = Claims::new(&Requestor::Unknown, "id", &clock);
    assert_eq!(claims.user_id, USER_ID, "token.go:264");
    let token = issue_token(&claims).expect("the OS random source refused");
    assert_eq!(accept(&token), Verdict::Ok);

    // The recorded case, so the fact that an empty one would still parse is on
    // the record rather than assumed.
    let cases = recorded_cases();
    let recorded = section(&cases, "robot_parse")
        .into_iter()
        .find(|case| case.kind() == "verdict" && case.get("name") == "empty_user_id")
        .expect("the section records no empty_user_id case");
    assert_eq!(
        recorded.output, "ok",
        "line {}: an empty user_id no longer parses, so the deletion at \
         identity.go:141-145 is no longer the only thing that catches it",
        recorded.number
    );
}
