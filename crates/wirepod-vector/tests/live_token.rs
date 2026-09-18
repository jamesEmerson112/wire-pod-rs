//! One live `RefreshToken` against the Go server running on this machine.
//!
//! This is the only test in the port that speaks to the production server.
//! It opens one TLS connection to `https://127.0.0.1:443`, calls
//! `tokenpb.Token/RefreshToken` exactly once, and compares the shape of the
//! bundle that comes back with the bundle
//! [`wirepod_core::token::jwt`] builds locally. Nothing else is dialled and
//! no other method is called.
//!
//! # Why one call is safe
//!
//! `RefreshToken` is `token.go:291-296`, which is three lines: one debug log
//! and `CreateJWT(ctx, false, false)`. Inside `CreateJWT`, the peer address is
//! split at the first colon (`token.go:201-202`) and handed to
//! `GetEsnFromTarget` (`token.go:58-74`), which reads `botSdkInfo.json` and
//! compares the address against each stored `ip_address` for exact string
//! equality. The robot on this network is stored under its LAN address, so a
//! call from `127.0.0.1` misses and `err` is non-nil.
//!
//! That miss is what makes the call harmless. The disk-writing arm is
//! `token.go:221-230`, reached only when the lookup hits: it is the arm that
//! calls `WriteTokenHash`, `SetBotGUID` and `ChangeGUIDInIni`, so it is the
//! arm that rewrites `jdocs.json`, `botSdkInfo.json` and `sdk_config.ini`.
//! A miss takes the else arm at `token.go:231-239` instead, which appends one
//! entry to the in-memory `TokenHashStore` (`token.go:37`, `token.go:236`)
//! and touches no file. The literal IPv4 address is used rather than
//! `localhost`, whose IPv6 form would make `token.go:202` key that entry with
//! the string `[`.
//!
//! # The exact side effects of one run
//!
//! One entry `{"127.0.0.1", guid, hash}` appended to `TokenHashStore`
//! (`token.go:236`). It lives in process memory, a restart clears it, and its
//! only other reader is the jdocs server's fallback at
//! `jdocs/server.go:91-94`. Six `logger.Debug` lines: `token.go:292`,
//! `token.go:197`, `token.go:198`, `token.go:232`, `token.go:234` and
//! `token.go:251`. One read of `botSdkInfo.json` at `token.go:59`. No write
//! to any file, which is what the modification-time assertion below pins.
//!
//! # What this test must never do
//!
//! It must never construct an `AssociatePrimaryUserRequest` or call
//! `AssociatePrimaryUser`, `AssociateSecondaryClient`,
//! `ReassociatePrimaryUser`, `ListRevokedTokens` or `RevokeTokens`. The first
//! of those dereferences `pem.Decode`'s result unchecked at
//! `token.go:274-275`, and the Go server installs no recovery interceptor, so
//! a request with an empty certificate field would take the whole production
//! process down. That paragraph is the only place the word appears in this
//! file.
//!
//! Nothing from the response is ever printed. Not the token, not any of its
//! three segments, not `client_token`, and nothing decoded out of the claims
//! except the fixed literals this port defines for itself. Every assertion
//! message describes a length, a count or a shape, so a failure says what was
//! wrong without saying what the value was. That is why the assertions below
//! are `assert!` with a written message rather than `assert_eq!`, whose
//! failure output would print both sides.
//!
//! # Guards
//!
//! Both the live test and the helpers it calls are behind `#[ignore]`, and
//! the test panics immediately unless `WIREPOD_LIVE_GO=1` is set, so
//! `cargo test -- --ignored` on a machine with no Go server fails on the
//! first line instead of hanging on a connect. The RPC itself runs under a
//! ten second `tokio::time::timeout`.
//!
//! A full ignored run of this file makes exactly one RPC, because the robot's
//! acceptor is a plain function called on the one response rather than a
//! second test.
//!
//! Run it deliberately, and with `--nocapture`, because the skip lines and
//! the ALPN outcome go to stderr:
//!
//! ```text
//! cargo test -p wirepod-vector --test live_token -- --ignored --nocapture
//! ```
//!
//! # ALPN
//!
//! The Go listener is `tls.Listen` with a `tls.Config` that sets only
//! `Certificates` and a nil `CipherSuites`
//! (`initwirepod/startserver.go:184-187`), so it advertises no ALPN protocol
//! and leaves cmux to sniff the HTTP/2 preface afterwards. rustls only fails
//! a handshake when the server selects a protocol the client did not offer,
//! not when it selects none, so this port's `h2` offer
//! (`crates/wirepod-vector/src/tls.rs:144`) should be accepted as a silent
//! no-op. Should the dial fail anyway, [`one_refresh_token`] retries once
//! with a clone of the configuration whose `alpn_protocols` is empty and says
//! so on stderr. The retry is unconditional on a connect failure because
//! `tonic::transport::Error` does not distinguish a handshake alert from an
//! unreachable port, so a server that is simply down is retried once and then
//! reported as unreachable.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tonic::transport::Endpoint;
use wirepod_core::paths::{DataDir, sdk_ini_dir};
use wirepod_core::store::sdk_ini::sdk_config_path;
use wirepod_core::timefmt::rfc3339_nano;
use wirepod_core::token::jwt::{
    ALG, DEFAULT_REQUESTOR_ID, HEADER, SIGNATURE_LEN, TOKEN_TYPE, USER_ID, encode_segment,
};
use wirepod_core::wallclock::{SystemWallClock, WallTime};
use wirepod_core::{Claims, GUID_B64_LEN, Requestor, generate_token_id, issue_token};
use wirepod_proto::tokenpb::RefreshTokenRequest;
use wirepod_proto::tokenpb::token_client::TokenClient;
use wirepod_vector::{InsecureTlsConnector, insecure_client_config};

// ---------------------------------------------------------------------------
// The constants this file pins against
// ---------------------------------------------------------------------------

/// The environment variable that arms the live test.
const LIVE_GATE: &str = "WIREPOD_LIVE_GO";

/// The one target this file is ever allowed to dial.
const TARGET: &str = "https://127.0.0.1:443";

/// The ceiling the one RPC runs under.
const RPC_CEILING: Duration = Duration::from_secs(10);

/// The claim keys, in the byte order `encoding/json`'s map encoder writes
/// them, which is the order [`Claims`] declares its fields in.
const CLAIM_ORDER: [&str; 7] = [
    "expires",
    "iat",
    "permissions",
    "requestor_id",
    "token_id",
    "token_type",
    "user_id",
];

/// The six claims the robot's `FromJwtToken` requires to be present and to be
/// strings, in the order it checks them
/// (`vector-cloud/internal/token/identity/token.go:101-137`).
const ROBOT_REQUIRED_CLAIMS: [&str; 6] = [
    "token_id",
    "token_type",
    "user_id",
    "requestor_id",
    "iat",
    "expires",
];

/// Every `alg` name `golang-jwt/jwt` v3.2.2 registers in its package `init`
/// functions, which is the set `GetSigningMethod` (`signing_method.go:27-35`)
/// can answer for and therefore the set `ParseUnverified` accepts
/// (`parser.go:138-145`). Gathered from the `RegisterSigningMethod` calls in
/// `ecdsa.go:35`, `:41`, `:47`, `ed25519.go:24`, `hmac.go:27`, `:33`, `:39`,
/// `none.go:18`, `rsa.go:26`, `:32`, `:38` and `rsa_pss.go:43`, `:60`, `:77`.
const REGISTERED_ALGS: [&str; 14] = [
    "ES256", "ES384", "ES512", "EdDSA", "HS256", "HS384", "HS512", "none", "PS256", "PS384",
    "PS512", "RS256", "RS384", "RS512",
];

/// The character length of a RawURL segment carrying [`SIGNATURE_LEN`] bytes.
///
/// 128 bytes is 42 whole three-byte groups plus two bytes, so it encodes to
/// 42 four-character groups plus three characters.
const SIGNATURE_SEGMENT_LEN: usize = 171;

/// The length of a UUID in its hyphenated form.
const UUID_LEN: usize = 36;

// ---------------------------------------------------------------------------
// The two guards
// ---------------------------------------------------------------------------

/// Panics unless the operator armed the live test.
///
/// Without this an `--ignored` run on a machine with no Go server would sit on
/// a TCP connect until the ceiling, and the failure would read like a timeout
/// rather than like a machine that was never meant to run this.
fn require_the_live_gate() {
    assert!(
        std::env::var_os(LIVE_GATE).is_some_and(|value| value == "1"),
        "set {LIVE_GATE}=1 to run this test: it dials the production Go server on this machine"
    );
}

// ---------------------------------------------------------------------------
// base64url, written here because this crate has no base64 dependency
// ---------------------------------------------------------------------------

/// The six-bit value a RawURL character carries, or `None` for a character the
/// alphabet does not have.
///
/// `+`, `/` and `=` fall through to `None`, which is what makes this a decoder
/// for `base64.RawURLEncoding` (`golang-jwt/jwt@v3.2.2/token.go:102-104`) and
/// not for the standard alphabet or for a padded segment.
fn raw_url_sextet(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

/// Decodes one unpadded base64url segment.
///
/// A length one past a four-character group cannot be produced by any input,
/// so it is refused. Leftover bits at the end of the last partial group are
/// dropped rather than required to be zero, which is what Go's decoder does
/// too; nothing this file asserts depends on them.
fn decode_raw_url(segment: &str) -> Result<Vec<u8>, &'static str> {
    if segment.len() % 4 == 1 {
        return Err("a base64url segment cannot be one character past a group");
    }
    let mut out = Vec::with_capacity(segment.len() * 3 / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for byte in segment.bytes() {
        let sextet = raw_url_sextet(byte)
            .ok_or("a segment carries a character the RawURL alphabet does not have")?;
        accumulator = (accumulator << 6) | sextet;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            let whole = (accumulator >> bits) & 0xff;
            out.push(u8::try_from(whole).expect("masked to one byte"));
        }
    }
    Ok(out)
}

/// Whether a character belongs to the standard base64 alphabet, which is what
/// `CreateTokenAndHashedToken` encodes a GUID with
/// (`servers/token/hashing.go:54`).
fn is_standard_base64_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/'
}

// ---------------------------------------------------------------------------
// A JSON object scanner, written here for the same reason
// ---------------------------------------------------------------------------

/// One top-level member of a JSON object: its decoded key and the raw text of
/// its value.
struct Member {
    key: String,
    value: String,
}

impl Member {
    /// The member's value when it is a JSON string, decoded.
    fn string_value(&self) -> Result<String, &'static str> {
        let mut json = Json::new(self.value.as_bytes());
        let decoded = json.string()?;
        if json.at == json.src.len() {
            Ok(decoded)
        } else {
            Err("a member's value is not a single JSON string")
        }
    }

    /// Whether the member's value is the literal `null`, which is what Go's
    /// untyped nil `permissions` marshals to (`token.go:257`).
    fn is_null(&self) -> bool {
        self.value == "null"
    }
}

/// A cursor over JSON text.
///
/// This is deliberately small: it walks one object, decodes strings, and skips
/// past every other value without interpreting it. `wirepod-vector` depends on
/// neither `serde_json` nor `base64` and this commit is not allowed to add a
/// dependency to its manifest, so both of this file's decoders are written out
/// rather than pulled in.
struct Json<'a> {
    src: &'a [u8],
    at: usize,
}

impl<'a> Json<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self { src, at: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn take(&mut self, byte: u8) -> Result<(), &'static str> {
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err("the JSON text is not the shape this scanner accepts")
        }
    }

    /// Reads four hexadecimal digits as one code unit.
    fn hex4(&mut self) -> Result<u32, &'static str> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.peek().ok_or("a \\u escape is truncated")?;
            self.at += 1;
            let digit = char::from(byte)
                .to_digit(16)
                .ok_or("a \\u escape carries a non-hexadecimal digit")?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    /// Reads the body of a `\u` escape, joining a surrogate pair when it finds
    /// one.
    fn unicode_escape(&mut self) -> Result<char, &'static str> {
        let first = self.hex4()?;
        if (0xd800..0xdc00).contains(&first) {
            self.take(b'\\')?;
            self.take(b'u')?;
            let second = self.hex4()?;
            if !(0xdc00..0xe000).contains(&second) {
                return Err("a \\u escape is a high surrogate with no low surrogate");
            }
            let joined = 0x1_0000 + ((first - 0xd800) << 10) + (second - 0xdc00);
            return char::from_u32(joined).ok_or("a surrogate pair is not a scalar value");
        }
        char::from_u32(first).ok_or("a \\u escape is not a scalar value")
    }

    /// Reads one JSON string and returns its contents with escapes resolved.
    fn string(&mut self) -> Result<String, &'static str> {
        self.take(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = self.peek().ok_or("a JSON string is unterminated")?;
            self.at += 1;
            match byte {
                b'"' => {
                    return String::from_utf8(out).map_err(|_| "a JSON string is not UTF-8");
                }
                b'\\' => {
                    let escape = self.peek().ok_or("a JSON string ends in a backslash")?;
                    self.at += 1;
                    let resolved = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return Err("a JSON string carries an escape this scanner rejects"),
                    };
                    let mut buffer = [0u8; 4];
                    out.extend_from_slice(resolved.encode_utf8(&mut buffer).as_bytes());
                }
                other => out.push(other),
            }
        }
    }

    /// Walks past an already-opened container up to and including its closing
    /// bracket, respecting strings so a bracket inside one does not count.
    fn skip_container(&mut self, open: u8, close: u8) -> Result<(), &'static str> {
        let mut depth = 1usize;
        while depth > 0 {
            match self.peek() {
                None => return Err("a JSON container is unterminated"),
                Some(b'"') => {
                    self.string()?;
                }
                Some(byte) => {
                    self.at += 1;
                    if byte == open {
                        depth += 1;
                    } else if byte == close {
                        depth -= 1;
                    }
                }
            }
        }
        Ok(())
    }

    /// Walks past one value of any kind.
    fn skip_value(&mut self) -> Result<(), &'static str> {
        match self.peek().ok_or("a JSON value is missing")? {
            b'"' => {
                self.string()?;
                Ok(())
            }
            b'{' => {
                self.at += 1;
                self.skip_container(b'{', b'}')
            }
            b'[' => {
                self.at += 1;
                self.skip_container(b'[', b']')
            }
            first if first == b'-' || first.is_ascii_alphanumeric() => {
                while let Some(byte) = self.peek() {
                    if byte == b'-' || byte == b'+' || byte == b'.' || byte.is_ascii_alphanumeric()
                    {
                        self.at += 1;
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            _ => Err("a JSON value starts with a byte this scanner rejects"),
        }
    }

    /// Walks past one value and returns the raw text it spanned.
    fn value(&mut self) -> Result<&'a str, &'static str> {
        let src = self.src;
        let start = self.at;
        self.skip_value()?;
        std::str::from_utf8(&src[start..self.at]).map_err(|_| "a JSON value is not UTF-8")
    }
}

/// The top-level members of a JSON object, in the order they appear.
///
/// The order is the whole point: `encoding/json` writes a `map[string]…` with
/// its keys sorted by `strings.Compare`, so the byte order of the claim keys
/// is part of the payload contract and not an accident of how they are read
/// back.
fn object_members(text: &[u8]) -> Result<Vec<Member>, &'static str> {
    let mut json = Json::new(text);
    json.skip_space();
    json.take(b'{')?;
    let mut members = Vec::new();
    json.skip_space();
    if json.peek() == Some(b'}') {
        json.at += 1;
    } else {
        loop {
            json.skip_space();
            let key = json.string()?;
            json.skip_space();
            json.take(b':')?;
            json.skip_space();
            let value = json.value()?.to_owned();
            members.push(Member { key, value });
            json.skip_space();
            match json.peek() {
                Some(b',') => json.at += 1,
                Some(b'}') => {
                    json.at += 1;
                    break;
                }
                _ => return Err("a JSON object member is followed by neither a comma nor a brace"),
            }
        }
    }
    json.skip_space();
    if json.at == json.src.len() {
        Ok(members)
    } else {
        Err("the JSON text carries trailing bytes after its object")
    }
}

/// One named member, or a panic naming the key this file was looking for.
fn member<'m>(members: &'m [Member], key: &str) -> &'m Member {
    members
        .iter()
        .find(|candidate| candidate.key == key)
        .unwrap_or_else(|| panic!("the object has no {key} member"))
}

// ---------------------------------------------------------------------------
// The shape checks
// ---------------------------------------------------------------------------

/// Whether a string is shaped like a version 4, variant RFC 4122 UUID, which
/// is what `uuid.New().String()` produces (`token.go:180-183`).
fn is_uuid_v4(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != UUID_LEN {
        return false;
    }
    bytes.iter().copied().enumerate().all(|(index, byte)| {
        match index {
            8 | 13 | 18 | 23 => byte == b'-',
            // The version nibble.
            14 => byte == b'4',
            // The variant nibble: `10xx`, so one of 8, 9, a, b.
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
        }
    })
}

/// The calendar date part of a timestamp, which is all this file compares.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CalendarDate {
    year: i32,
    month: u32,
    day: u32,
}

/// Parses the shape Go's `time.RFC3339` layout accepts and returns the
/// calendar date.
///
/// That layout is `2006-01-02T15:04:05Z07:00`, and Go's parser accepts an
/// optional fractional second after the seconds field even though the layout
/// names none, which is why `time.RFC3339Nano` output parses back through it.
/// The zone is either the literal `Z` or a sign with `HH:MM`. This is the
/// check the robot performs on both claims
/// (`vector-cloud/internal/token/identity/token.go:126-137`).
fn parse_rfc3339_date(text: &str) -> Result<CalendarDate, &'static str> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return Err("a timestamp is shorter than a bare RFC 3339 instant plus a zone");
    }
    for index in [0usize, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
        if !bytes[index].is_ascii_digit() {
            return Err("a timestamp has a non-digit where RFC 3339 wants one");
        }
    }
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return Err("a timestamp is missing one of its date separators");
    }
    if bytes[10] != b'T' {
        return Err("a timestamp is missing the T between its date and its time");
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return Err("a timestamp is missing one of its time separators");
    }

    let mut at = 19;
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while matches!(bytes.get(at), Some(byte) if byte.is_ascii_digit()) {
            at += 1;
        }
        if at == start {
            return Err("a timestamp has a fraction point with no digits after it");
        }
    }

    match bytes.get(at).copied() {
        Some(b'Z') => at += 1,
        Some(b'+' | b'-') => {
            if bytes.len() < at + 6 {
                return Err("a timestamp's numeric zone is truncated");
            }
            if bytes[at + 3] != b':' {
                return Err("a timestamp's numeric zone is missing its colon");
            }
            for index in [at + 1, at + 2, at + 4, at + 5] {
                if !bytes[index].is_ascii_digit() {
                    return Err("a timestamp's numeric zone has a non-digit in it");
                }
            }
            at += 6;
        }
        _ => return Err("a timestamp carries neither Z nor a signed numeric zone"),
    }
    if at != bytes.len() {
        return Err("a timestamp carries trailing bytes after its zone");
    }

    Ok(CalendarDate {
        year: text[0..4].parse().expect("four validated digits"),
        month: text[5..7].parse().expect("two validated digits"),
        day: text[8..10].parse().expect("two validated digits"),
    })
}

/// Whether `expires` is the calendar date one month after `iat`, which is what
/// Go's `AddDate(0, 1, 0)` produces (`token.go:196`).
///
/// A day of month that does not exist in the following month is not treated as
/// a match. Go would normalise it forward, so a token issued on the 31st of a
/// month with a 30-day successor legitimately lands on the 1st; this returns
/// false there and the caller's message says so, because the alternative is an
/// assertion that cannot tell that case from a broken one.
fn is_one_calendar_month_later(iat: CalendarDate, expires: CalendarDate) -> bool {
    let (year, month) = if iat.month == 12 {
        (iat.year + 1, 1)
    } else {
        (iat.year, iat.month + 1)
    };
    expires.year == year && expires.month == month && expires.day == iat.day
}

// ---------------------------------------------------------------------------
// The robot's own acceptor
// ---------------------------------------------------------------------------

/// The checks the robot runs over a token it is handed, reproduced here.
///
/// The robot parses with `new(jwt.Parser).ParseUnverified`
/// (`vector-cloud/internal/token/identity/identity.go:158`), which is
/// `golang-jwt/jwt@v3.2.2/parser.go:96-148`: exactly three dot-separated
/// segments, segments one and two decoded with `base64.RawURLEncoding`
/// (`golang-jwt/jwt@v3.2.2/token.go:102-104`), both unmarshalled as JSON
/// objects, and a string `alg` in the header that `GetSigningMethod`
/// (`golang-jwt/jwt@v3.2.2/signing_method.go:27-35`) can resolve
/// (`parser.go:138-145`). It then runs `FromJwtToken`
/// (`vector-cloud/internal/token/identity/token.go:96-161`), which requires
/// six string claims (`token.go:101-137`) and parses `iat` and `expires`
/// with `time.RFC3339`. `permissions` is optional there: it is read only when
/// it is an object (`token.go:153-156`), so a null passes through as an
/// absent value.
///
/// This is a plain function rather than a second `#[tokio::test]` so that one
/// ignored run of this file makes one RPC.
fn the_live_token_is_one_the_robot_parser_would_accept(token: &str) {
    let segments: Vec<&str> = token.split('.').collect();
    assert!(
        segments.len() == 3,
        "the robot's parser wants three dot-separated segments and this token has {}",
        segments.len()
    );

    let header = decode_raw_url(segments[0]).expect("the header segment is RawURL base64");
    let header_members = object_members(&header).expect("the header segment is a JSON object");
    let alg = member(&header_members, "alg")
        .string_value()
        .expect("the header's alg member is a JSON string");
    assert!(
        REGISTERED_ALGS.contains(&alg.as_str()),
        "the header names an alg golang-jwt v3.2.2 does not register, which is the \
         unverifiable error at parser.go:140-141"
    );

    let payload = decode_raw_url(segments[1]).expect("the claim segment is RawURL base64");
    let members = object_members(&payload).expect("the claim segment is a JSON object");
    for key in ROBOT_REQUIRED_CLAIMS {
        member(&members, key)
            .string_value()
            .unwrap_or_else(|_| panic!("the {key} claim is not a JSON string"));
    }

    for key in ["iat", "expires"] {
        let stamp = member(&members, key)
            .string_value()
            .expect("the claim is a JSON string");
        parse_rfc3339_date(&stamp)
            .unwrap_or_else(|reason| panic!("the {key} claim is not RFC 3339: {reason}"));
    }
}

// ---------------------------------------------------------------------------
// The live files the call must not touch
// ---------------------------------------------------------------------------

/// The user's home directory, which is where Go builds `SDKIniPath` from
/// (`vars.go:207-209`).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// The files the disk-writing arm of `CreateJWT` would rewrite, as many of the
/// three as this machine actually has.
///
/// `jdocs.json` is what `WriteTokenHash` rewrites (`token.go:226`),
/// `botSdkInfo.json` is what `SetBotGUID` rewrites (`token.go:227`) and
/// `sdk_config.ini` is what `ChangeGUIDInIni` rewrites (`token.go:228`,
/// `token.go:177`). None of the three is ever opened by this test: only
/// `fs::metadata` is called on them.
///
/// A file that is absent is skipped one at a time, with its own printed line,
/// rather than abandoning the whole check: a machine that has never taken the
/// disk-writing arm has no `~/.anki_vector/sdk_config.ini` at all, and
/// dropping the two jdocs files along with it would leave the assertion
/// vacuous on exactly the machine it was written for. An unset `APPDATA` skips
/// all three, because then none of them has a path.
fn watched_files() -> Vec<PathBuf> {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        eprintln!("SKIP: APPDATA is unset, so there is no live data directory to watch");
        return Vec::new();
    };
    let data_dir = DataDir::packaged(Path::new(&appdata));
    let mut candidates = vec![
        PathBuf::from(data_dir.jdocs_path()),
        PathBuf::from(data_dir.bot_info_path()),
    ];
    match home_dir() {
        Some(home) => candidates.push(PathBuf::from(sdk_config_path(&sdk_ini_dir(&home)))),
        None => {
            eprintln!("SKIP: neither USERPROFILE nor HOME is set, so sdk_config.ini has no path")
        }
    }
    candidates
        .into_iter()
        .filter(|path| {
            let present = path.is_file();
            if !present {
                eprintln!(
                    "SKIP: {} is missing, so its mtime is not watched",
                    path.display()
                );
            }
            present
        })
        .collect()
}

/// The modification time of each watched file. Metadata only; nothing here
/// opens or reads a byte of any of them.
fn modification_times(paths: &[PathBuf]) -> Vec<SystemTime> {
    paths
        .iter()
        .map(|path| {
            fs::metadata(path)
                .unwrap_or_else(|_| panic!("{} has readable metadata", path.display()))
                .modified()
                .unwrap_or_else(|_| panic!("{} reports a modification time", path.display()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The one call
// ---------------------------------------------------------------------------

/// What one call handed back. Neither field is ever printed.
struct LiveBundle {
    token: String,
    client_token: String,
}

/// Opens one TLS channel to the live server and makes exactly one
/// `RefreshToken` call over it.
///
/// The channel is built the way `factory.rs:46-50` and
/// `factory.rs:144-148` build a robot's: `Endpoint::from_shared` then
/// `connect_with_connector` over
/// [`InsecureTlsConnector`], which is what lets the port dial TLS without
/// tonic's own `tls` feature. The resulting `Channel` goes straight to the
/// generated token client rather than into a `TonicRobotConn`, because the
/// robot connection attaches an SDK bearer credential this server neither
/// wants nor reads.
async fn one_refresh_token() -> LiveBundle {
    let channel = match Endpoint::from_shared(TARGET.to_owned())
        .expect("the live target is a valid URI")
        .connect_with_connector(InsecureTlsConnector::new())
        .await
    {
        Ok(channel) => {
            eprintln!("ALPN: the h2 offer was accepted, so no retry was needed");
            channel
        }
        Err(_) => {
            eprintln!("ALPN: the first dial failed, retrying once with an empty ALPN offer");
            let mut config = insecure_client_config();
            config.alpn_protocols.clear();
            Endpoint::from_shared(TARGET.to_owned())
                .expect("the live target is a valid URI")
                .connect_with_connector(InsecureTlsConnector::with_config(Arc::new(config)))
                .await
                .expect("the live Go server answers TLS on 127.0.0.1:443")
        }
    };

    let request = RefreshTokenRequest {
        refresh_jwt_tokens: true,
        ..Default::default()
    };
    let response = tokio::time::timeout(
        RPC_CEILING,
        TokenClient::new(channel).refresh_token(request),
    )
    .await
    .expect("the live RefreshToken call answered within the ceiling")
    .unwrap_or_else(|status| {
        panic!(
            "the live RefreshToken call failed with gRPC code {}",
            status.code()
        )
    });

    let data = response
        .into_inner()
        .data
        .expect("the live RefreshTokenResponse carries a bundle");
    LiveBundle {
        token: data.token,
        client_token: data.client_token,
    }
}

/// One live `RefreshToken`, compared with the bundle this port builds.
///
/// Every assertion below is about a length, a count, a key name this file
/// declares or a fixed literal the port defines. Nothing decoded out of the
/// response reaches the output on any path.
#[tokio::test]
#[ignore = "dials the production Go server on this machine, which only that machine has"]
async fn a_live_refresh_token_returns_the_bundle_this_port_would_build() {
    require_the_live_gate();

    let watched = watched_files();
    let before = modification_times(&watched);

    let live = one_refresh_token().await;

    // --- the three segments
    let segments: Vec<&str> = live.token.split('.').collect();
    assert!(
        segments.len() == 3,
        "the live token has {} dot-separated segments and not three",
        segments.len()
    );

    // --- segment one is the header literal, byte for byte
    let header = decode_raw_url(segments[0]).expect("the live header segment is RawURL base64");
    assert!(
        header == HEADER,
        "the live header segment does not decode to the {} byte header literal this port writes",
        HEADER.len()
    );

    // --- segment two's key set and key order
    let payload = decode_raw_url(segments[1]).expect("the live claim segment is RawURL base64");
    let members = object_members(&payload).expect("the live claim segment is a JSON object");
    let keys: Vec<&str> = members.iter().map(|one| one.key.as_str()).collect();
    assert!(
        keys == CLAIM_ORDER,
        "the live claim payload's {} top-level keys are not the {} this port writes, in the \
         byte order it writes them",
        keys.len(),
        CLAIM_ORDER.len()
    );

    // --- the three claims whose value is a literal this port defines
    for (key, expected) in [
        ("requestor_id", DEFAULT_REQUESTOR_ID),
        ("token_type", TOKEN_TYPE),
        ("user_id", USER_ID),
    ] {
        let value = member(&members, key)
            .string_value()
            .unwrap_or_else(|_| panic!("the live {key} claim is a JSON string"));
        assert!(
            value == expected,
            "the live {key} claim is not the literal this port writes for it"
        );
    }
    assert!(
        member(&members, "permissions").is_null(),
        "the live permissions claim is not JSON null"
    );

    // --- the token id
    let token_id = member(&members, "token_id")
        .string_value()
        .expect("the live token_id claim is a JSON string");
    assert!(
        is_uuid_v4(&token_id),
        "the live token_id claim is not shaped like a {UUID_LEN} character version 4 UUID"
    );

    // --- the two timestamps and the month between them
    let iat = parse_rfc3339_date(
        &member(&members, "iat")
            .string_value()
            .expect("the live iat claim is a JSON string"),
    )
    .unwrap_or_else(|reason| panic!("the live iat claim is not RFC 3339: {reason}"));
    let expires = parse_rfc3339_date(
        &member(&members, "expires")
            .string_value()
            .expect("the live expires claim is a JSON string"),
    )
    .unwrap_or_else(|reason| panic!("the live expires claim is not RFC 3339: {reason}"));
    assert!(
        is_one_calendar_month_later(iat, expires),
        "the live expires date is not the same day of the month one month after the live iat \
         date; a token issued on a day number the next month does not have reaches here too, \
         because Go normalises that forward"
    );

    // --- segment three
    assert!(
        segments[2].len() == SIGNATURE_SEGMENT_LEN,
        "the live signature segment is {} characters and not {SIGNATURE_SEGMENT_LEN}",
        segments[2].len()
    );
    assert!(
        !segments[2].contains(['+', '/', '=']),
        "the live signature segment carries a character the RawURL alphabet does not have"
    );
    let signature =
        decode_raw_url(segments[2]).expect("the live signature segment is RawURL base64");
    assert!(
        signature.len() == SIGNATURE_LEN,
        "the live signature segment decodes to {} bytes and not {SIGNATURE_LEN}",
        signature.len()
    );

    // --- the client token
    let client_token = live.client_token.as_bytes();
    assert!(
        client_token.len() == GUID_B64_LEN,
        "the live client_token is {} characters and not {GUID_B64_LEN}",
        client_token.len()
    );
    assert!(
        client_token[GUID_B64_LEN - 2..] == *b"==",
        "the live client_token does not end in the two padding characters a {} byte value \
         carries in standard base64",
        wirepod_core::TOKEN_SIZE
    );
    assert!(
        client_token[..GUID_B64_LEN - 2]
            .iter()
            .copied()
            .all(is_standard_base64_char),
        "the live client_token carries a character the standard base64 alphabet does not have"
    );

    // --- the robot would take it
    the_live_token_is_one_the_robot_parser_would_accept(&live.token);

    // --- and the port builds the same shape
    let ours = issue_token(&Claims::new(
        &Requestor::Unknown,
        generate_token_id().expect("the OS random source answers"),
        &SystemWallClock::new(),
    ))
    .expect("the OS random source answers");
    let our_segments: Vec<&str> = ours.split('.').collect();
    assert!(
        our_segments.len() == segments.len(),
        "the port's token has {} segments and the live one has {}",
        our_segments.len(),
        segments.len()
    );
    assert!(
        our_segments[0] == segments[0],
        "the port's header segment is not character for character the live one"
    );
    assert!(
        our_segments[2].len() == segments[2].len(),
        "the port's signature segment is {} characters and the live one is {}",
        our_segments[2].len(),
        segments[2].len()
    );
    let our_payload =
        decode_raw_url(our_segments[1]).expect("the port's claim segment is RawURL base64");
    let our_members = object_members(&our_payload).expect("the port's claim segment is an object");
    let our_keys: Vec<&str> = our_members.iter().map(|one| one.key.as_str()).collect();
    assert!(
        our_keys == keys,
        "the port's {} claim keys are not the live ones, in the same byte order",
        our_keys.len()
    );
    for key in ["requestor_id", "token_type", "user_id"] {
        let ours = member(&our_members, key)
            .string_value()
            .unwrap_or_else(|_| panic!("the port's {key} claim is a JSON string"));
        let theirs = member(&members, key)
            .string_value()
            .unwrap_or_else(|_| panic!("the live {key} claim is a JSON string"));
        assert!(
            ours == theirs,
            "the port's {key} claim is not the live {key} claim"
        );
    }
    assert!(
        member(&our_members, "permissions").is_null(),
        "the port's permissions claim is not JSON null"
    );

    // --- and nothing on disk moved
    if watched.is_empty() {
        eprintln!("SKIP: no live file was watched, so nothing pinned the call as write-free");
    } else {
        let after = modification_times(&watched);
        assert!(
            after == before,
            "the call changed the modification time of one of the {} live files the \
             write-free arm of CreateJWT must never touch",
            watched.len()
        );
    }
}

// ---------------------------------------------------------------------------
// The helpers, under the normal gate
// ---------------------------------------------------------------------------

/// The two decoders agree with the port's own encoder and formatter.
///
/// Every input here is fixed and nothing is dialled, so this runs in the
/// ordinary `cargo test` gate and keeps the live test's helpers from rotting
/// between deliberate runs.
#[test]
fn the_decoders_agree_with_the_ports_own_encoder_and_formatter() {
    // `encode_segment` is `base64.RawURLEncoding.EncodeToString`, so a round
    // trip through it is the contract this decoder has to meet.
    let segment = encode_segment(HEADER);
    assert!(!segment.contains(['+', '/', '=']));
    assert_eq!(
        decode_raw_url(&segment).expect("the port's own segment decodes"),
        HEADER
    );
    // The two characters the URL alphabet substitutes, forced by input.
    assert_eq!(encode_segment(&[0xfb, 0xef, 0xbe]), "----");
    assert_eq!(encode_segment(&[0xff, 0xff, 0xff]), "____");
    assert_eq!(
        decode_raw_url("----").expect("the substituted 62 decodes"),
        vec![0xfb, 0xef, 0xbe]
    );
    assert_eq!(
        decode_raw_url("____").expect("the substituted 63 decodes"),
        vec![0xff, 0xff, 0xff]
    );
    // Padding, the standard alphabet and an impossible length are refused.
    assert!(decode_raw_url("QQ==").is_err());
    assert!(decode_raw_url("a+b/").is_err());
    assert!(decode_raw_url("A").is_err());

    // The alg this port signs with is one the robot's library registers.
    assert!(REGISTERED_ALGS.contains(&ALG));

    // `rfc3339_nano` at fixed instants, parsed back.
    let epoch = rfc3339_nano(WallTime::new(0, 0), 0);
    assert_eq!(epoch, "1970-01-01T00:00:00Z");
    let epoch_date = parse_rfc3339_date(&epoch).expect("the UTC form parses");
    assert_eq!(
        (epoch_date.year, epoch_date.month, epoch_date.day),
        (1970, 1, 1)
    );
    // A fraction, which `time.RFC3339` accepts even though its layout names
    // none.
    let fraction = rfc3339_nano(WallTime::new(0, 500_000_000), 0);
    assert_eq!(fraction, "1970-01-01T00:00:00.5Z");
    assert!(parse_rfc3339_date(&fraction).is_ok());
    // A signed numeric zone, which is the other half of the `Z07:00` verb.
    let western = rfc3339_nano(WallTime::new(0, 0), -5 * 3600);
    assert_eq!(western, "1969-12-31T19:00:00-05:00");
    let western_date = parse_rfc3339_date(&western).expect("the offset form parses");
    assert_eq!(
        (western_date.year, western_date.month, western_date.day),
        (1969, 12, 31)
    );
    // And the shapes it must refuse.
    assert!(parse_rfc3339_date("1970-01-01T00:00:00").is_err());
    assert!(parse_rfc3339_date("1970-01-01 00:00:00Z").is_err());
    assert!(parse_rfc3339_date("1970-01-01T00:00:00.Z").is_err());
    assert!(parse_rfc3339_date("1970-01-01T00:00:00-0500").is_err());
    assert!(parse_rfc3339_date("1970-01-01T00:00:00Zextra").is_err());

    // The month rule, over dates the same formatter produced. 31 days after
    // the epoch is 1 February, 334 days is 1 December and 365 days is
    // 1 January of the next year, none of them leap-affected.
    let day = 86_400i64;
    let february = parse_rfc3339_date(&rfc3339_nano(WallTime::new(31 * day, 0), 0))
        .expect("a formatted instant parses");
    let december = parse_rfc3339_date(&rfc3339_nano(WallTime::new(334 * day, 0), 0))
        .expect("a formatted instant parses");
    let next_january = parse_rfc3339_date(&rfc3339_nano(WallTime::new(365 * day, 0), 0))
        .expect("a formatted instant parses");
    let march = parse_rfc3339_date(&rfc3339_nano(WallTime::new(59 * day, 0), 0))
        .expect("a formatted instant parses");
    assert!(is_one_calendar_month_later(epoch_date, february));
    assert!(is_one_calendar_month_later(december, next_january));
    assert!(!is_one_calendar_month_later(epoch_date, march));
    assert!(!is_one_calendar_month_later(epoch_date, epoch_date));
}

/// The scanner and the robot's acceptor, driven over a token this port issues.
///
/// This is the live test's whole assertion set minus the network: if the
/// acceptor or the object scanner is wrong, it fails here rather than on a
/// deliberate run against the production server.
#[test]
fn the_ports_own_token_passes_the_scanner_and_the_robots_acceptor() {
    let token = issue_token(&Claims::new(
        &Requestor::Unknown,
        generate_token_id().expect("the OS random source answers"),
        &SystemWallClock::new(),
    ))
    .expect("the OS random source answers");

    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(segments.len(), 3);
    assert_eq!(segments[2].len(), SIGNATURE_SEGMENT_LEN);
    assert_eq!(
        decode_raw_url(segments[2])
            .expect("the signature segment decodes")
            .len(),
        SIGNATURE_LEN
    );

    let payload = decode_raw_url(segments[1]).expect("the claim segment decodes");
    let members = object_members(&payload).expect("the claim segment is an object");
    let keys: Vec<&str> = members.iter().map(|one| one.key.as_str()).collect();
    assert_eq!(keys, CLAIM_ORDER);
    assert!(member(&members, "permissions").is_null());
    assert_eq!(
        member(&members, "requestor_id")
            .string_value()
            .expect("a string"),
        DEFAULT_REQUESTOR_ID
    );
    assert!(is_uuid_v4(
        &member(&members, "token_id")
            .string_value()
            .expect("a string")
    ));

    the_live_token_is_one_the_robot_parser_would_accept(&token);
}

/// The scanner reads the shapes a claim payload can actually carry.
///
/// A nested object, a nested array, a brace inside a string, a number with a
/// sign and an exponent, and a bare keyword all have to be walked past without
/// being interpreted, because only the top-level keys and three of the
/// top-level values are ever read.
#[test]
fn the_object_scanner_keeps_order_and_walks_past_every_value_kind() {
    let text = br#"{"a":"x","b":null,"c":{"d":[1,2,{"e":"}"}]},"f":-1.5e3,"g":true}"#;
    let members = object_members(text).expect("a well formed object");
    let keys: Vec<&str> = members.iter().map(|one| one.key.as_str()).collect();
    assert_eq!(keys, ["a", "b", "c", "f", "g"]);
    assert_eq!(members[0].string_value().expect("a string"), "x");
    assert!(members[1].is_null());
    assert!(!members[2].is_null());
    assert!(members[2].string_value().is_err());

    // Escapes, including the `\u` form Go's HTML escaping produces for the
    // three characters `encoding/json` always escapes, and a surrogate pair.
    let escaped = br#"{"k":"<&> \"q\" \\ \u00e9 \ud83d\ude00\tz"}"#;
    let one = object_members(escaped).expect("a well formed object");
    assert_eq!(
        one[0].string_value().expect("a string"),
        "<&> \"q\" \\ \u{e9} \u{1f600}\tz"
    );

    // And the shapes it refuses.
    assert!(object_members(br#"{"a":1}trailing"#).is_err());
    assert!(object_members(br#"{"a"1}"#).is_err());
    assert!(object_members(br#"{"a":1"#).is_err());
    assert!(object_members(br#"["a"]"#).is_err());
    assert!(object_members(br#"{"a":"\ud83d"}"#).is_err());
    assert!(
        object_members(br#"{}"#)
            .expect("an empty object")
            .is_empty()
    );
}
