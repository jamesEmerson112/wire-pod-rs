//! Token hashing and GUID generation, table-driven from the recorded probe
//! output.
//!
//! `docs/phases/P1-robot-connect-auth/go-probe/expected.txt` is the stdout of
//! the Go program committed beside it, so every expectation below is a value Go
//! printed rather than one written by hand. Each line is
//! `section\tinput\toutput`: the input is space-separated `key=value` pairs
//! whose first pair is always `kind=`, and the output is always a Go `%q`
//! quoted literal. A `kind=` this file does not recognize fails the test, so a
//! new probe line cannot be silently skipped, and an escape the unquoter does
//! not know panics rather than being guessed at.
//!
//! The last test is `#[ignore]`d and checks the live association on this
//! machine. It is the reason the rest of the file exists: the hash in the live
//! `vic.AppTokens` jdoc was produced by the Go server, and if this port does
//! not reproduce it the robot's association does not survive a cutover.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use wirepod_core::{
    GUID_B64_LEN, HASH_SIZE, HASHED_B64_LEN, HASHED_RAW_LEN, SALT_SIZE, TOKEN_SIZE, TokenHashError,
    compare_hash_and_token, create_token_and_hashed_token, encode_token_and_hash, hash_token,
    new_from_hash,
};

const EXPECTED: &str =
    include_str!("../../../docs/phases/P1-robot-connect-auth/go-probe/expected.txt");

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
        .unwrap_or_else(|| panic!("line {line}: the output column is not a quoted literal"));

    let mut out = String::with_capacity(body.len());
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        let escape = characters
            .next()
            .unwrap_or_else(|| panic!("line {line}: the output column ends in a backslash"));
        out.push(match escape {
            '\\' => '\\',
            '"' => '"',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            other => panic!(
                "line {line}: unsupported escape \\{other}; teach unquote about it before \
                 trusting this recording"
            ),
        });
    }
    out
}

/// Parses the whole recording into cases, comments dropped.
fn cases() -> Vec<Case<'static>> {
    let mut parsed = Vec::new();
    for (index, raw) in EXPECTED.lines().enumerate() {
        let line = index + 1;
        let text = raw.strip_suffix('\r').unwrap_or(raw);
        if text.starts_with('#') {
            continue;
        }
        assert!(
            !text.is_empty(),
            "line {line}: the recording has no blank lines"
        );

        let mut columns = text.split('\t');
        let section = columns.next().unwrap_or_default();
        let input = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: missing the input column"));
        let output = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: missing the output column"));
        assert!(
            columns.next().is_none(),
            "line {line}: more than three columns"
        );

        parsed.push(Case {
            line,
            section,
            input: input
                .split(' ')
                .map(|pair| match pair.split_once('=') {
                    Some(split) => split,
                    None => panic!("line {line}: input piece {pair} is not key=value"),
                })
                .collect(),
            output: unquote(output, line),
        });
    }
    parsed
}

/// Parses a lowercase hex string from the probe's input column.
fn hex_bytes(text: &str, line: usize) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2),
        "line {line}: hex input has an odd length"
    );
    (0..text.len() / 2)
        .map(|index| {
            u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .unwrap_or_else(|_| panic!("line {line}: input is not lowercase hex"))
        })
        .collect()
}

/// The spelling the probe's `split_hash` and `split_salt` cases record.
fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        text.push_str(&format!("{byte:02x}"));
        text
    })
}

/// Narrows a decoded probe input to the fixed-size array the API takes.
fn fixed<const N: usize>(bytes: &[u8], line: usize) -> [u8; N] {
    <[u8; N]>::try_from(bytes).unwrap_or_else(|_| panic!("line {line}: expected {N} input bytes"))
}

/// Renders a result the way the probe records a Go `error`: `"ok"` for nil.
fn outcome(result: Result<(), TokenHashError>) -> String {
    match result {
        Ok(()) => "ok".to_owned(),
        Err(error) => error.to_string(),
    }
}

/// Decodes a recorded stored hash, which every split case is keyed on.
fn decode_hashed(text: &str, line: usize) -> Vec<u8> {
    STANDARD
        .decode(text)
        .unwrap_or_else(|_| panic!("line {line}: the recorded hash is not standard base64"))
}

#[test]
fn the_hash_section_matches_the_recorded_probe() {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();

    for case in cases() {
        // The other sections belong to the commits that port their subjects.
        if case.section != "hash" {
            continue;
        }
        let line = case.line;
        let kind = case.get("kind");
        *seen.entry(kind).or_default() += 1;

        match kind {
            // The GUID is the token bytes alone, so the salt cannot reach it.
            "guid" => {
                let token = fixed::<TOKEN_SIZE>(&hex_bytes(case.get("token"), line), line);
                let pair = encode_token_and_hash(&token, &[0u8; SALT_SIZE]);
                assert_eq!(pair.guid, case.output, "line {line}: guid");
                assert_eq!(pair.guid.len(), GUID_B64_LEN, "line {line}: guid length");
            }
            "hash" => {
                let token = fixed::<TOKEN_SIZE>(&hex_bytes(case.get("token"), line), line);
                let salt = fixed::<SALT_SIZE>(&hex_bytes(case.get("salt"), line), line);
                let pair = encode_token_and_hash(&token, &salt);
                assert_eq!(pair.guid_hash, case.output, "line {line}: hash");
                assert_eq!(
                    pair.guid_hash.len(),
                    HASHED_B64_LEN,
                    "line {line}: hash length"
                );

                // The same bytes reached through the primitive, which is what
                // pins the single SHA-256 pass over token and then salt.
                let mut raw = Vec::with_capacity(HASHED_RAW_LEN);
                raw.extend_from_slice(&hash_token(&token, &salt));
                raw.extend_from_slice(&salt);
                assert_eq!(STANDARD.encode(&raw), case.output, "line {line}: primitive");
            }
            "const" => {
                let name = case.get("name");
                let got = match name {
                    "tokenSize" => TOKEN_SIZE.to_string(),
                    "saltSize" => SALT_SIZE.to_string(),
                    "hashSize" => HASH_SIZE.to_string(),
                    "hashed_raw_len" => HASHED_RAW_LEN.to_string(),
                    "guid_b64_len" => GUID_B64_LEN.to_string(),
                    "hashed_b64_len" => HASHED_B64_LEN.to_string(),
                    "errMismatchedTokenAndHash" => TokenHashError::Mismatch.to_string(),
                    "errHashTooLong" => TokenHashError::HashTooLong.to_string(),
                    "errHashTooShort" => TokenHashError::HashTooShort.to_string(),
                    "errTokenTooLong" => TokenHashError::TokenTooLong.to_string(),
                    "errTokenTooShort" => TokenHashError::TokenTooShort.to_string(),
                    other => panic!("line {line}: unknown recorded constant {other}"),
                };
                assert_eq!(got, case.output, "line {line}: const {name}");
            }
            "decoded_len" => {
                let raw = decode_hashed(case.get("hashed"), line);
                assert_eq!(raw.len().to_string(), case.output, "line {line}: decoded");
                assert_eq!(raw.len(), HASHED_RAW_LEN, "line {line}: decoded constant");
            }
            "split_at" => {
                let raw = decode_hashed(case.get("hashed"), line);
                let hashed = new_from_hash(&raw).expect("a recorded hash is the right length");
                assert_eq!(
                    hashed.hash.len().to_string(),
                    case.output,
                    "line {line}: split point"
                );
                assert_eq!(
                    HASH_SIZE.to_string(),
                    case.output,
                    "line {line}: split constant"
                );
            }
            "split_hash" => {
                let raw = decode_hashed(case.get("hashed"), line);
                let hashed = new_from_hash(&raw).expect("a recorded hash is the right length");
                assert_eq!(hex_string(hashed.hash), case.output, "line {line}: digest");
            }
            "split_salt" => {
                let raw = decode_hashed(case.get("hashed"), line);
                let hashed = new_from_hash(&raw).expect("a recorded hash is the right length");
                assert_eq!(hex_string(hashed.salt), case.output, "line {line}: salt");
            }
            "split_hash_len" => {
                let raw = decode_hashed(case.get("hashed"), line);
                let hashed = new_from_hash(&raw).expect("a recorded hash is the right length");
                assert_eq!(
                    hashed.hash.len().to_string(),
                    case.output,
                    "line {line}: digest length"
                );
            }
            "split_salt_len" => {
                let raw = decode_hashed(case.get("hashed"), line);
                let hashed = new_from_hash(&raw).expect("a recorded hash is the right length");
                assert_eq!(
                    hashed.salt.len().to_string(),
                    case.output,
                    "line {line}: salt length"
                );
            }
            // The recorded lengths straddle the boundary at 47, 48 and 49, so
            // the two errors and the accepting case are all pinned. The bytes
            // themselves do not matter to a length check.
            "newfromhash" => {
                let length: usize = case
                    .get("len")
                    .parse()
                    .unwrap_or_else(|_| panic!("line {line}: len= is not a number"));
                let bytes = vec![0u8; length];
                assert_eq!(
                    outcome(new_from_hash(&bytes).map(|_| ())),
                    case.output,
                    "line {line}: newFromHash at {length}"
                );
            }
            "compare" => {
                assert_eq!(
                    outcome(compare_hash_and_token(
                        case.get("hashed"),
                        case.get("token")
                    )),
                    case.output,
                    "line {line}: compare"
                );
            }
            other => panic!("line {line}: unknown hash-section kind {other}"),
        }
    }

    for kind in [
        "guid",
        "hash",
        "const",
        "decoded_len",
        "split_at",
        "split_hash",
        "split_salt",
        "split_hash_len",
        "split_salt_len",
        "newfromhash",
        "compare",
    ] {
        assert!(
            seen.get(kind).copied().unwrap_or_default() > 0,
            "the recording holds no {kind} cases, so nothing pinned that behavior"
        );
    }
}

/// A freshly drawn pair verifies against itself, which is the round trip the
/// robot performs on every association.
///
/// Sixteen draws rather than one, because a bug that depends on the random
/// bytes, such as a salt whose high byte is dropped, would show up
/// intermittently. Nothing here prints token material: a failure reports only
/// lengths and the error.
#[test]
fn a_fresh_pair_verifies_against_itself() {
    for _ in 0..16 {
        let pair = create_token_and_hashed_token().expect("the OS random source answers");
        assert_eq!(pair.guid.len(), GUID_B64_LEN, "the GUID is 24 characters");
        assert_eq!(
            pair.guid_hash.len(),
            HASHED_B64_LEN,
            "the stored hash is 64 characters"
        );
        compare_hash_and_token(&pair.guid_hash, &pair.guid)
            .expect("a freshly drawn pair verifies against itself");
    }
}

/// Two generations differ, and neither verifies against the other's hash.
///
/// This is what says the salt is redrawn per pair rather than fixed, and it is
/// the test a hardcoded or zeroed random source fails. The comparisons use
/// `assert!` rather than `assert_ne!` so that a failure cannot print the token
/// material it compared.
#[test]
fn two_generations_differ_and_do_not_cross_verify() {
    let first = create_token_and_hashed_token().expect("the OS random source answers");
    let second = create_token_and_hashed_token().expect("the OS random source answers");

    assert!(
        first.guid != second.guid,
        "two generations drew the same GUID"
    );
    assert!(
        first.guid_hash != second.guid_hash,
        "two generations produced the same stored hash"
    );
    assert_eq!(
        compare_hash_and_token(&first.guid_hash, &second.guid),
        Err(TokenHashError::Mismatch),
        "one generation's GUID verified against another's hash"
    );
}

/// The live association on this machine still verifies under the port.
///
/// This is the phase's critical gate rather than a convenience: the hash in
/// `%APPDATA%\wire-pod\jdocs\jdocs.json` was written by the Go server, and the
/// robot holds the matching GUID. Nothing re-issues either at cutover, so a
/// port that cannot verify the pair loses the association.
///
/// It is `#[ignore]`d because it reads a live, machine-specific data directory
/// that no CI runner has, and because the material it loads must never be
/// printed. Run it deliberately with `cargo test -- --ignored`. It asserts
/// nothing about the values themselves, reports nothing derived from them, and
/// its parse failures carry fixed messages so that no fragment of the files can
/// reach the output. When `APPDATA` is unset or either file is missing it
/// prints a skip line and passes, because the absence of the live server is not
/// a failure of this code.
#[test]
#[ignore = "reads the live wire-pod data directory, which only the server's own machine has"]
fn the_live_stored_hash_verifies_against_the_live_guid() {
    use std::fs;
    use std::path::PathBuf;

    use serde::Deserialize;
    use wirepod_core::{BotInfo, Esn};

    /// One element of the jdocs file, Go's `botjdoc` (`vars.go:137-141`).
    #[derive(Deserialize)]
    struct LiveEntry {
        #[serde(default)]
        thing: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        jdoc: LiveJdoc,
    }

    /// The one field of Go's `AJdoc` this test reads (`vars.go:130-135`).
    #[derive(Default, Deserialize)]
    struct LiveJdoc {
        #[serde(default)]
        json_doc: String,
    }

    /// The document `vic.AppTokens` carries, Go's `ClientTokenManager`
    /// (`hashing.go:43-45`).
    #[derive(Deserialize)]
    struct LiveTokens {
        #[serde(default)]
        client_tokens: Vec<LiveToken>,
    }

    /// Go's `ClientToken` (`hashing.go:36-41`), cut to the stored hash.
    #[derive(Deserialize)]
    struct LiveToken {
        #[serde(default)]
        hash: String,
    }

    let Some(appdata) = std::env::var_os("APPDATA") else {
        eprintln!("SKIP: APPDATA is unset, so there is no live wire-pod data directory");
        return;
    };
    let jdocs_dir = PathBuf::from(appdata).join("wire-pod").join("jdocs");
    let jdocs_path = jdocs_dir.join("jdocs.json");
    let bot_info_path = jdocs_dir.join("botSdkInfo.json");
    for path in [&jdocs_path, &bot_info_path] {
        if !path.is_file() {
            eprintln!(
                "SKIP: {} is missing, so there is no live association to check",
                path.display()
            );
            return;
        }
    }

    let jdocs_text = fs::read_to_string(&jdocs_path).expect("the live jdocs file is readable");
    let entries: Vec<LiveEntry> = serde_json::from_str(&jdocs_text)
        .unwrap_or_else(|_| panic!("the live jdocs file is not the shape Go writes"));
    let info_text = fs::read_to_string(&bot_info_path).expect("the live bot-info file is readable");
    let info: BotInfo = serde_json::from_str(&info_text)
        .unwrap_or_else(|_| panic!("the live bot-info file is not the shape Go writes"));

    let mut robots_checked = 0usize;
    for entry in &entries {
        if entry.name != "vic.AppTokens" {
            continue;
        }
        // Go stores this jdoc under `vic:<esn>` (`token.go:125`).
        let Some(esn) = entry.thing.strip_prefix("vic:") else {
            continue;
        };
        let Some(target) = info.resolve(&Esn::new(esn)) else {
            eprintln!("SKIP: a robot has a stored token but no bot-info entry, so it has no GUID");
            continue;
        };
        let tokens: LiveTokens = serde_json::from_str(&entry.jdoc.json_doc)
            .unwrap_or_else(|_| panic!("a live vic.AppTokens jdoc is not a client-token document"));

        // Go appends a client token per association and the current GUID
        // matches the newest one, so one match is the assertion, not all.
        let matched = tokens
            .client_tokens
            .iter()
            .filter(|token| compare_hash_and_token(&token.hash, &target.guid).is_ok())
            .count();
        assert!(
            matched > 0,
            "no stored token in a live vic.AppTokens jdoc verifies against that robot's GUID, \
             so this port would break the association at cutover"
        );
        robots_checked += 1;
    }

    if robots_checked == 0 {
        eprintln!("SKIP: the live jdocs file holds no vic.AppTokens document to check");
    }
}
