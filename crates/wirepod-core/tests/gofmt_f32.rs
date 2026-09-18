//! Go's `float32` JSON rendering, table-driven from the recorded probe output.
//!
//! `docs/phases/P1-robot-connect-auth/go-probe/expected.txt` is the stdout of
//! the Go program committed beside it, so the expectations here are recorded
//! rather than hand-written. Its `f32json` section marshals a struct with one
//! `float32` field, which is the shape `pkg/vars/config.go:38-39` has, and
//! records both the whole document and the number on its own.
//!
//! The file's format is documented in the phase README. Lines whose first byte
//! is `#` are comments; every other line is three tab-separated fields, being
//! the section name, the input as space-separated `key=value` pairs whose first
//! pair is `kind=`, and the output as a Go `%q` quoted string literal. The
//! parser below refuses anything it does not recognize within the section it
//! was asked for, so neither a new probe line nor an unfamiliar escape can be
//! silently skipped, and it leaves every other section's bytes alone so that a
//! later section cannot fail a `float32` test for a reason of its own.
//!
//! `crates/wirepod-core/tests/gofmt.rs` reads the P4 recording, which predates
//! this format and has an unquoted output column and a bare Go expression for
//! its input, so the two parsers stay separate rather than one growing a mode
//! switch.

use std::collections::BTreeMap;

use wirepod_core::{GoJsonError, go_json_f32, go_json_f32_raw, go_json_f64};

const EXPECTED: &str =
    include_str!("../../../docs/phases/P1-robot-connect-auth/go-probe/expected.txt");

/// One parsed line: the raw input column, its pairs in order, the unquoted
/// output and the line number every failure message names.
struct Case {
    input: &'static str,
    pairs: Vec<(&'static str, &'static str)>,
    want: String,
    line: usize,
}

impl Case {
    /// The value of one input key, or a failure naming the line.
    fn pair(&self, key: &str) -> &str {
        self.pairs
            .iter()
            .find(|(name, _)| *name == key)
            .unwrap_or_else(|| panic!("line {}: no {key}= pair in {}", self.line, self.input))
            .1
    }

    /// The authoritative `v=0x........` bit pattern, as an `f32`.
    ///
    /// The README says to parse the bits and read `expr=` as documentation, and
    /// that is load-bearing here: several inputs are `math.Nextafter32` results
    /// one ulp off a cutoff, which have no short decimal spelling, and the NaN
    /// and infinity inputs are written as bit patterns so the recording does
    /// not depend on how a `float64` NaN narrows on the probe host.
    fn value(&self) -> f32 {
        let hex = self
            .pair("v")
            .strip_prefix("0x")
            .unwrap_or_else(|| panic!("line {}: v= is not a 0x literal", self.line));
        let raw = u32::from_str_radix(hex, 16)
            .unwrap_or_else(|_| panic!("line {}: v=0x{hex} is not 32 bits of hex", self.line));
        f32::from_bits(raw)
    }
}

/// Undoes Go's `%q` quoting.
///
/// Only the five escapes the recording can produce are accepted. Anything else
/// panics rather than being passed through, so a probe change that starts
/// emitting some other escape fails the test instead of comparing against a Go
/// source literal that no longer means the same bytes.
fn unquote(quoted: &str, line: usize) -> String {
    let body = quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("line {line}: the output column is not a quoted literal"));
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let escape = chars
            .next()
            .unwrap_or_else(|| panic!("line {line}: the output column ends in a lone backslash"));
        out.push(match escape {
            '\\' => '\\',
            '"' => '"',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            other => panic!("line {line}: unsupported escape in the output column: {other}"),
        });
    }
    out
}

/// Every case in one section of the recording, with the comment lines dropped.
///
/// Only the named section's lines are split into pairs and unquoted. The whole
/// file is still walked far enough to hold the README's promise that the
/// (section, input) pair is unique, because that promise is what makes it safe
/// to key anything on the pair, but a section this test does not assert is left
/// as bytes: a later probe section that emits an escape outside the five
/// [`unquote`] accepts is that section's own test to fix, not a `float32`
/// failure.
fn cases_in(wanted: &str) -> Vec<Case> {
    let mut parsed = Vec::new();
    let mut seen: BTreeMap<(&str, &str), usize> = BTreeMap::new();

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
        let section = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: missing the section column"));
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

        // The README promises the pair is unique, and keying anything on it is
        // only safe while that holds.
        if let Some(first) = seen.insert((section, input), line) {
            panic!("line {line}: {section} / {input} already appeared on line {first}");
        }

        if section != wanted {
            continue;
        }

        let pairs: Vec<(&str, &str)> = input
            .split(' ')
            .map(|piece| {
                piece
                    .split_once('=')
                    .unwrap_or_else(|| panic!("line {line}: {piece} is not a key=value pair"))
            })
            .collect();
        assert_eq!(
            pairs.first().map(|(key, _)| *key),
            Some("kind"),
            "line {line}: the first input pair is not kind="
        );

        parsed.push(Case {
            input,
            pairs,
            want: unquote(output, line),
            line,
        });
    }

    assert!(
        !parsed.is_empty(),
        "the recording has no {wanted} section at all"
    );
    parsed
}

/// The `float32` field of `pkg/vars/config.go:38-39`, which is the shape the
/// probe marshals. `top_p` and `temp` reach `apiConfig.json` through exactly
/// this.
#[derive(serde::Serialize)]
struct Doc<'a> {
    v: &'a serde_json::value::RawValue,
}

#[test]
fn go_float32_json_matches_the_recorded_probe() {
    let mut numbers = 0usize;
    let mut structs = 0usize;
    let mut errors = 0usize;

    for case in cases_in("f32json") {
        let value = case.value();
        let (line, input) = (case.line, case.input);

        match case.pair("kind") {
            "number" => {
                let got = go_json_f32(value).unwrap_or_else(|error| {
                    panic!("line {line}: {input} did not marshal: {error}")
                });
                assert_eq!(got, case.want, "line {line}: {input}");
                numbers += 1;
            }
            "struct" => {
                // Through the serializer, because that is how the config file
                // is written: the raw form has to survive being a struct field.
                let raw = go_json_f32_raw(value).unwrap_or_else(|error| {
                    panic!("line {line}: {input} did not marshal: {error}")
                });
                let got = serde_json::to_string(&Doc { v: &raw })
                    .expect("a raw number serializes inside a struct");
                assert_eq!(got, case.want, "line {line}: {input}");
                structs += 1;
            }
            "error" => {
                let error = match go_json_f32(value) {
                    Ok(text) => panic!("line {line}: {input} marshalled as {text}"),
                    Err(error) => error,
                };
                assert_eq!(error.to_string(), case.want, "line {line}: {input}");
                // The raw form refuses the same values with the same error.
                let raw = match go_json_f32_raw(value) {
                    Ok(_) => panic!("line {line}: {input} produced a raw value"),
                    Err(error) => error,
                };
                assert_eq!(raw, error, "line {line}: {input}");
                errors += 1;
            }
            other => panic!("line {line}: unknown kind {other} in the f32json section"),
        }
    }

    // Every finite value is recorded twice, once whole and once as the bare
    // number, and the three values Go refuses are recorded once each. The
    // counts are exact so that deleting a recorded line fails here instead of
    // quietly shrinking the coverage; a struct and its number can otherwise be
    // removed together without disturbing any other assertion. They are the
    // recording's own numbers, recounted whenever the probe changes, so adding
    // a case to `f32JSONSection` means editing these three lines in the same
    // commit that regenerates `expected.txt`.
    assert_eq!(
        numbers, 35,
        "the f32json section records 35 finite values as bare numbers"
    );
    assert_eq!(
        structs, 35,
        "the f32json section records the same 35 values as whole documents"
    );
    assert_eq!(errors, 3, "the probe records NaN, +Inf and -Inf as errors");
}

/// Go breaks an exact decimal tie to even, and the recording carries one.
///
/// `0x3ee90000` is `0.455078125` exactly. That is nine significant digits and
/// eight of them round-trip, so the shortest form sits exactly halfway between
/// `0.45507812` and `0.45507813`, and both are legal shortest digits. `strconv`
/// takes the even one (`strconv/ftoaryu.go:412`, the round-up flag at
/// `:456-461`, which rounds an exact half up only away from an odd truncation)
/// where Rust's own shortest digits round the half up unconditionally.
///
/// Every `float32` tie has a magnitude between `2^-12` and `2^22`, so a tie
/// always reaches `encoding/json` through the plain form and the exponent form
/// can never carry one. This is the only shape the rule is observable in, which
/// is why the recording needs exactly one such case and why it sits in the
/// plain-form group.
#[test]
fn an_exact_decimal_tie_is_broken_the_way_go_breaks_it() {
    let case = cases_in("f32json")
        .into_iter()
        .find(|case| case.pair("kind") == "number" && case.pair("v") == "0x3ee90000")
        .expect("the f32json section carries the exact-tie case");
    let value = case.value();
    assert_eq!(
        f64::from(value),
        0.455078125,
        "0x3ee90000 is 0.455078125 exactly"
    );

    assert_eq!(go_json_f32(value).expect("finite"), case.want);
    // The tie is the whole point: Rust's shortest digits are the other
    // candidate, so passing them straight through would ship the wrong last
    // digit into apiConfig.json.
    assert_ne!(
        format!("{value}"),
        case.want,
        "Rust's own shortest digits break this tie the other way"
    );
}

/// A tail that merely starts with a 5 is not a tie, and must not be broken.
///
/// This is the other half of the rule above, and the recording carries an input
/// for it. `0x00000060` is `96 * 2^-149`, whose exact expansion is
/// `1.345246525751824388086780399958e-43`: the digit after the shortest three
/// is a 5, and the digits after that one are not all zeros, so the value is
/// strictly nearer `1.35e-43` than `1.34e-43`. `ryuDigits32` rounds up on any
/// non-zero tail without consulting parity (`strconv/ftoaryu.go:456-461`), and
/// Go writes `1.35e-43`.
///
/// A tie breaker that tested only the 5 would see a tie here, break it to the
/// even `1.34e-43`, and still pass
/// [`an_exact_decimal_tie_is_broken_the_way_go_breaks_it`], because there the
/// tail really is a lone 5. The two cases only mean something together.
#[test]
fn a_tail_that_only_begins_with_a_five_is_not_a_tie() {
    let case = cases_in("f32json")
        .into_iter()
        .find(|case| case.pair("kind") == "number" && case.pair("v") == "0x00000060")
        .expect("the f32json section carries the near-tie case");
    let value = case.value();

    assert_eq!(case.want, "1.35e-43", "this is what Go recorded");
    assert_eq!(go_json_f32(value).expect("finite"), case.want);
    // Unlike the exact tie, Rust's own shortest digits are already right here,
    // because neither formatter is breaking anything: the value is nearer one
    // candidate than the other and both round to it.
    assert_eq!(
        format!("{value:e}"),
        "1.35e-43",
        "the shortest digits of a near tie are the same on both sides"
    );
}

/// The correction this function exists for: Go formats at 32 bits, so the
/// `float64` renderer is the wrong answer for a `float32` field.
///
/// `0.7` is the shipped `top_p` default. Its exact value widened to a `float64`
/// renders as `0.699999988079071`, which is what would land in the config file
/// if the value were widened before formatting. `encode.go:557` and
/// `encode.go:561` both run at 32 bits instead, so `0.7` stays `0.7`. Every
/// boot rewrites the config file unconditionally (`pkg/vars/config.go:154-155`),
/// so a widening port would corrupt both fields on the first boot after cutover
/// and the Go server would read the drifted numbers back on rollback.
#[test]
fn a_float32_field_is_not_widened_before_formatting() {
    assert_eq!(go_json_f32(0.7).expect("a finite value marshals"), "0.7");
    assert_eq!(
        go_json_f64(f64::from(0.7f32)).expect("a finite value marshals"),
        "0.699999988079071"
    );
    // The other shipped default, which the two widths happen to agree on.
    assert_eq!(go_json_f32(1.0).expect("a finite value marshals"), "1");
}

/// The raw form is the string form, and it survives a serializer.
///
/// This is the whole reason the raw helper exists. `serde_json` picks the same
/// shortest `f32` digits Go does, so it gets `0.7` right, but it lays them out
/// differently in two ways that both reach `apiConfig.json`: it writes a
/// decimal point on every rendering it does not put in exponent form, and it
/// switches to exponent form far below Go's `1e21`.
#[test]
fn the_raw_form_carries_the_go_digits_through_a_serializer() {
    for value in [
        0.0f32,
        -0.0,
        0.7,
        1.0,
        0.05,
        1e-7,
        1e20,
        1e21,
        f32::MIN_POSITIVE,
        f32::MAX,
    ] {
        let text = go_json_f32(value).expect("a finite value marshals");
        let raw = go_json_f32_raw(value).expect("a finite value marshals");
        assert_eq!(
            raw.get(),
            text,
            "the raw form is the string form for {value}"
        );

        let document = serde_json::to_string(&Doc { v: &raw }).expect("a raw number serializes");
        assert_eq!(document, format!(r#"{{"v":{text}}}"#));
    }

    // What serde_json would have written instead, spelled out. A `temp` of 1
    // is the first case and the shipped default, and 1e20 is the second.
    assert_eq!(
        serde_json::to_string(&1.0f32).expect("an f32 serializes"),
        "1.0"
    );
    assert_eq!(
        serde_json::to_string(&0.0f32).expect("an f32 serializes"),
        "0.0"
    );
    assert_eq!(
        serde_json::to_string(&1e20f32).expect("an f32 serializes"),
        "1e+20"
    );
    // The digits themselves are not the problem: serde_json gets 0.7 right,
    // which is why the widening a float64 renderer would do is a separate bug
    // from the layout this helper exists to fix.
    assert_eq!(
        serde_json::to_string(&0.7f32).expect("an f32 serializes"),
        "0.7"
    );

    // The doc comments say serde_json writes a decimal point on every rendering
    // it does not put in exponent form, which is a claim no probe records.
    // Check it over every finite value the recording carries rather than
    // leaving it as an assertion about a library.
    for case in cases_in("f32json") {
        if case.pair("kind") != "number" {
            continue;
        }
        let text = serde_json::to_string(&case.value()).expect("an f32 serializes");
        assert!(
            text.contains('e') || text.contains('.'),
            "line {}: serde_json wrote {text} with neither an exponent nor a decimal point",
            case.line
        );
    }
}

#[test]
fn the_raw_form_refuses_what_go_refuses() {
    assert_eq!(
        go_json_f32_raw(f32::NAN).expect_err("NaN is not marshalable"),
        GoJsonError::Nan
    );
    assert_eq!(
        go_json_f32_raw(f32::INFINITY).expect_err("+Inf is not marshalable"),
        GoJsonError::PosInf
    );
    assert_eq!(
        go_json_f32_raw(f32::NEG_INFINITY).expect_err("-Inf is not marshalable"),
        GoJsonError::NegInf
    );
}

/// The cutoffs are `f32` comparisons, and the recording pins them one ulp
/// either side. This test names the bit patterns, so a reader can see where the
/// boundary sits without decoding the probe's `math.Nextafter32` inputs.
///
/// `encode.go:557` is exclusive below and inclusive above: the `f32` nearest
/// `1e-6` takes the plain form and the ulp below it takes the exponent form,
/// while at the top the `f32` nearest `1e21` takes the exponent form and the
/// ulp below it does not. A cutoff compared as a `float64` instead, or moved by
/// one ulp, changes exactly one of these four.
#[test]
fn the_cutoffs_sit_where_the_recording_puts_them() {
    let small = f32::from_bits(0x3586_37bd);
    assert_eq!(small, 1e-6f32, "0x358637bd is the f32 nearest 1e-6");
    assert_eq!(go_json_f32(small).expect("finite"), "0.000001");
    assert_eq!(
        go_json_f32(f32::from_bits(0x3586_37bc)).expect("finite"),
        "9.999999e-7"
    );

    let large = f32::from_bits(0x6258_d727);
    assert_eq!(large, 1e21f32, "0x6258d727 is the f32 nearest 1e21");
    assert_eq!(go_json_f32(large).expect("finite"), "1e+21");
    assert_eq!(
        go_json_f32(f32::from_bits(0x6258_d726)).expect("finite"),
        "999999950000000000000"
    );
}

/// The exponent cleanup rewrites `e-09` to `e-9` and nothing else.
///
/// `encode.go:562-569` tests the last four bytes for `e`, `-`, `0`, so a
/// two-digit magnitude and a positive exponent both keep the padding `strconv`
/// gave them. Dropping the cleanup breaks only the single-digit negative case,
/// which is why all four shapes are asserted together.
#[test]
fn only_a_single_digit_negative_exponent_is_cleaned_up() {
    assert_eq!(go_json_f32(1e-9).expect("finite"), "1e-9");
    assert_eq!(go_json_f32(1e-10).expect("finite"), "1e-10");
    assert_eq!(go_json_f32(1e21).expect("finite"), "1e+21");
    assert_eq!(go_json_f32(f32::from_bits(1)).expect("finite"), "1e-45");
}
