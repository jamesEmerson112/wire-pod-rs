//! Go float formatting, table-driven from the recorded probe output.
//!
//! `docs/phases/P4-sdk-app/gofmt-probe/expected.txt` is the stdout of the Go
//! program committed beside it, so the expectations are recorded rather than
//! hand-written. Each line is `section\tinput\toutput`. An input literal this
//! file does not recognize fails the test, so a new probe line cannot be
//! silently skipped.

use wirepod_core::{GoJsonError, go_format_f32, go_json_f64, go_json_f64_raw};

const EXPECTED: &str = include_str!("../../../docs/phases/P4-sdk-app/gofmt-probe/expected.txt");

/// Parses a `math.Float32frombits(0x...)` or `math.Float64frombits(0x...)`
/// literal into its raw bits.
///
/// The probe writes its exact halfway ties that way on purpose: a decimal
/// literal for one of them would beg the question the case exists to settle,
/// because the literal would have to be written in one of the two spellings
/// under test. Parsing the form generically also means a new tie case in the
/// probe needs no new arm below.
fn from_bits_literal(literal: &str, prefix: &str) -> Option<u64> {
    let hex = literal.strip_prefix(prefix)?.strip_suffix(')')?;
    u64::from_str_radix(hex, 16).ok()
}

fn f32_input(literal: &str) -> Option<f32> {
    if let Some(bits) = from_bits_literal(literal, "math.Float32frombits(0x") {
        return Some(f32::from_bits(u32::try_from(bits).ok()?));
    }
    Some(match literal {
        "float32(0)" => 0.0,
        "float32(math.Copysign(0, -1))" => -0.0,
        "float32(1)" => 1.0,
        "float32(0.1)" => 0.1,
        "float32(0.75)" => 0.75,
        "float32(0.5325)" => 0.5325,
        "float32(5e-05)" => 5e-05,
        "float32(0.0001)" => 0.0001,
        "float32(0.001)" => 0.001,
        "float32(1.5)" => 1.5,
        "float32(2.5)" => 2.5,
        "float32(-0.25)" => -0.25,
        "float32(100000)" => 100_000.0,
        "float32(999999)" => 999_999.0,
        "float32(1e6)" => 1e6,
        "float32(1.5e6)" => 1.5e6,
        "float32(1234567)" => 1_234_567.0,
        "float32(123456789)" => 123_456_789.0,
        "float32(1e20)" => 1e20,
        "float32(1e21)" => 1e21,
        "float32(1e22)" => 1e22,
        "float32(math.MaxFloat32)" => f32::MAX,
        // The smallest positive subnormal, which is Go's SmallestNonzeroFloat32.
        "float32(math.SmallestNonzeroFloat32)" => f32::from_bits(1),
        "float32(math.Inf(1))" => f32::INFINITY,
        "float32(math.Inf(-1))" => f32::NEG_INFINITY,
        "float32(math.NaN())" => f32::NAN,
        _ => return None,
    })
}

fn f64_input(literal: &str) -> Option<f64> {
    if let Some(bits) = from_bits_literal(literal, "math.Float64frombits(0x") {
        return Some(f64::from_bits(bits));
    }
    Some(match literal {
        "float64(0)" => 0.0,
        "float64(math.Copysign(0, -1))" => -0.0,
        "float64(13)" => 13.0,
        "float64(13.482)" => 13.482,
        "float64(14.0)" => 14.0,
        "float64(0.1)" => 0.1,
        "float64(-0.5)" => -0.5,
        "float64(0.000001)" => 0.000001,
        "float64(1e-6)" => 1e-6,
        "float64(1e-7)" => 1e-7,
        "float64(1e17)" => 1e17,
        "float64(1e20)" => 1e20,
        "float64(1e21)" => 1e21,
        "float64(123456789012345678)" => 123_456_789_012_345_678.0,
        "float64(1.5e300)" => 1.5e300,
        "float64(5e-324)" => 5e-324,
        "float64(math.MaxFloat64)" => f64::MAX,
        "float64(1.0000000000000002)" => 1.0000000000000002,
        "NaN" => f64::NAN,
        "+Inf" => f64::INFINITY,
        "-Inf" => f64::NEG_INFINITY,
        _ => return None,
    })
}

#[test]
fn go_formatting_matches_the_recorded_probe() {
    let mut f32_cases = 0usize;
    let mut f64_cases = 0usize;

    for (index, raw) in EXPECTED.lines().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            continue;
        }
        let number = index + 1;
        let mut columns = line.split('\t');
        let section = columns.next().unwrap_or_default();
        let literal = columns
            .next()
            .unwrap_or_else(|| panic!("line {number}: missing the input column"));
        let want = columns
            .next()
            .unwrap_or_else(|| panic!("line {number}: missing the output column"));
        assert!(
            columns.next().is_none(),
            "line {number}: more than three columns"
        );

        match section {
            "f32v" => {
                let value = f32_input(literal).unwrap_or_else(|| {
                    panic!("line {number}: unrecognized f32 literal {literal}; add it to f32_input")
                });
                assert_eq!(go_format_f32(value), want, "line {number}: {literal}");
                f32_cases += 1;
            }
            "f64json" => {
                let value = f64_input(literal).unwrap_or_else(|| {
                    panic!("line {number}: unrecognized f64 literal {literal}; add it to f64_input")
                });
                let got = match go_json_f64(value) {
                    Ok(text) => text,
                    Err(error) => format!("ERROR: {error}"),
                };
                assert_eq!(got, want, "line {number}: {literal}");
                f64_cases += 1;
            }
            other => panic!("line {number}: unknown section {other}"),
        }
    }

    assert!(f32_cases > 0, "the probe file recorded no f32 cases");
    assert!(f64_cases > 0, "the probe file recorded no f64 cases");
}

/// The `RawValue` form must be the string form, unchanged, and it must survive
/// being serialized inside a struct.
///
/// This is the whole reason the helper exists: `serde_json` serializing an
/// `f64` writes `0.0` where Go writes `0`, so `/api-sdk/net_probe`'s `rttMs`
/// has to reach the wire as digits rather than as a number the encoder
/// re-renders.
#[test]
fn the_raw_form_carries_the_go_digits_through_a_serializer() {
    /// The shape `/api-sdk/net_probe` serializes, cut down to the one field
    /// that cannot go through `serde_json`'s own number writer.
    #[derive(serde::Serialize)]
    struct Body<'a> {
        #[serde(rename = "rttMs")]
        rtt_ms: &'a serde_json::value::RawValue,
    }

    for value in [
        0.0f64,
        13.482,
        1.0,
        0.001,
        -0.25,
        1e21,
        1e-7,
        f64::MIN_POSITIVE,
        f64::MAX,
    ] {
        let text = go_json_f64(value).expect("a finite value marshals");
        let raw = go_json_f64_raw(value).expect("a finite value marshals");
        assert_eq!(
            raw.get(),
            text,
            "the raw form is the string form for {value}"
        );

        // Inside a document, which is where it is actually used.
        let document =
            serde_json::to_string(&Body { rtt_ms: &raw }).expect("a raw number serializes");
        assert_eq!(document, format!(r#"{{"rttMs":{text}}}"#));
    }

    // The two spellings the encoder would get wrong, spelled out.
    assert_eq!(go_json_f64_raw(0.0).expect("zero marshals").get(), "0");
    assert_eq!(
        go_json_f64_raw(13.482)
            .expect("a round trip marshals")
            .get(),
        "13.482"
    );
}

#[test]
fn the_raw_form_refuses_what_go_refuses() {
    assert_eq!(
        go_json_f64_raw(f64::NAN).expect_err("NaN is not marshalable"),
        GoJsonError::Nan
    );
    assert_eq!(
        go_json_f64_raw(f64::INFINITY).expect_err("+Inf is not marshalable"),
        GoJsonError::PosInf
    );
    assert_eq!(
        go_json_f64_raw(f64::NEG_INFINITY).expect_err("-Inf is not marshalable"),
        GoJsonError::NegInf
    );
}
