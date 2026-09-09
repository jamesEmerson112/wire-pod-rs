//! Go-compatible float formatting.
//!
//! Two Go behaviors reach the wire and have to be reproduced byte for byte:
//! `fmt.Sprintf("%v", x)` on a `float32`, which is what `get_stim_status`
//! prints when it passes `stimState`'s `float32` to `fmt.Fprint`
//! (`server.go:512`, `robot.go:276`), and `encoding/json`'s `float64`
//! encoding, which carries `net_probe`'s `rttMs` (`server.go:48`,
//! `server.go:117`). The `custom_eye_color` echo is not one of them: it writes
//! the raw `hue` and `sat` form strings back out (`server.go:163`) and never
//! parses a float at all.
//!
//! Rust's own `Display` agrees with neither: it writes `14` as `14`, but also
//! `1e6` as `1000000` where Go writes `1e+06`, and `serde_json` always emits a
//! decimal point. Rust's shortest digits also break an exact tie the other way
//! from Go, which [`Decimal::break_tie_to_even`] undoes.
//!
//! Both functions are pinned by `docs/phases/P4-sdk-app/gofmt-probe/expected.txt`,
//! which is the recorded stdout of the Go probe program committed beside it.

use std::fmt;

use serde_json::value::RawValue;

/// The values `encoding/json` refuses to marshal.
///
/// `Display` reproduces Go's error text exactly, because it is the string the
/// server would log or return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoJsonError {
    /// Not a number.
    Nan,
    /// Positive infinity.
    PosInf,
    /// Negative infinity.
    NegInf,
}

impl fmt::Display for GoJsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Nan => "NaN",
            Self::PosInf => "+Inf",
            Self::NegInf => "-Inf",
        };
        write!(f, "json: unsupported value: {value}")
    }
}

impl std::error::Error for GoJsonError {}

/// Formats `x` the way Go's `fmt.Sprintf("%v", x)` formats a `float32`.
///
/// That is `strconv.FormatFloat(v, 'g', -1, 32)`: shortest round-trip digits,
/// exponent form when the decimal exponent is below -4 or at least 6, and the
/// spellings `+Inf`, `-Inf` and `NaN`.
pub fn go_format_f32(x: f32) -> String {
    if x.is_nan() {
        return "NaN".to_owned();
    }
    if x.is_infinite() {
        return if x.is_sign_positive() { "+Inf" } else { "-Inf" }.to_owned();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    let decimal = Decimal::shortest_f32(x);
    // strconv's 'g' with the shortest precision decides on eprec = 6, so the
    // switch is `exp < -4 || exp >= 6` (strconv/ftoa.go, the `if shortest`
    // arm of the 'g' case).
    if decimal.exp < -4 || decimal.exp >= 6 {
        decimal.exponent_form()
    } else {
        decimal.plain_form()
    }
}

/// Formats `x` the way Go's `encoding/json` marshals a `float64`.
///
/// Plain form when the value is zero or its magnitude is in `[1e-6, 1e21)`, and
/// exponent form otherwise, both with shortest round-trip digits and no decimal
/// point on a whole number. NaN and the infinities are errors, as they are in
/// Go.
pub fn go_json_f64(x: f64) -> Result<String, GoJsonError> {
    if x.is_nan() {
        return Err(GoJsonError::Nan);
    }
    if x.is_infinite() {
        return Err(if x.is_sign_positive() {
            GoJsonError::PosInf
        } else {
            GoJsonError::NegInf
        });
    }
    if x == 0.0 {
        return Ok(if x.is_sign_negative() { "-0" } else { "0" }.to_owned());
    }
    let decimal = Decimal::shortest_f64(x);
    // encoding/json writes exponent form outside [1e-6, 1e21) and plain form
    // inside it (encoding/json/encode.go, floatEncoder).
    if !(1e-6..1e21).contains(&x.abs()) {
        let mut out = decimal.exponent_form();
        // encoding/json rewrites a two-digit negative exponent down to one
        // digit, "clean up e-09 to e-9" (encoding/json/encode.go, floatEncoder).
        // A positive exponent keeps its padding, so `1e+21` stays as it is.
        let bytes = out.as_bytes();
        let n = bytes.len();
        if n >= 4 && bytes[n - 4] == b'e' && bytes[n - 3] == b'-' && bytes[n - 2] == b'0' {
            out.remove(n - 2);
        }
        Ok(out)
    } else {
        Ok(decimal.plain_form())
    }
}

/// The same rendering as [`go_json_f64`], as a value that can be spliced into a
/// serialized struct without being re-encoded.
///
/// `serde_json`'s own `f64` writer always emits a decimal point, so a field
/// typed as an `f64` reaches the wire as `0.0` where Go writes `0`, and as
/// `1e21` where Go writes `1e+21`. A [`RawValue`] field carries these digits
/// through the serializer untouched, which is what makes
/// `/api-sdk/net_probe`'s `rttMs` byte-exact while the rest of the body is
/// still built by `serde`.
///
/// # Panics
///
/// Never. Every string [`go_json_f64`] returns is a JSON number literal, which
/// is what [`RawValue::from_string`] is checking for; the `expect` is that
/// invariant written down rather than a case with a behavior of its own.
pub fn go_json_f64_raw(x: f64) -> Result<Box<RawValue>, GoJsonError> {
    let rendered = go_json_f64(x)?;
    Ok(RawValue::from_string(rendered).expect("every go_json_f64 rendering is a JSON number"))
}

/// A finite, non-zero float split into sign, shortest round-trip digits and the
/// decimal exponent, so that `value = 0.d0 d1 ... * 10^(exp + 1)`.
struct Decimal {
    negative: bool,
    digits: String,
    exp: i32,
}

impl Decimal {
    /// The shortest Go digits for a finite, non-zero `f32`.
    ///
    /// The exact expansion of an `f32` never runs past 105 significant digits:
    /// the widest is the smallest subnormal, `2^-149`, which is
    /// `5^149 * 10^-149`, and `5^149` has 105 digits. Asking for 151 digits
    /// therefore always covers the whole expansion, and every digit after it is
    /// a zero the formatter padded on.
    fn shortest_f32(x: f32) -> Self {
        let mut decimal = Self::of(&format!("{x:e}"));
        decimal.break_tie_to_even(|| Self::of(&format!("{x:.150e}")));
        debug_assert_eq!(
            decimal
                .exponent_form()
                .parse::<f32>()
                .expect("the digits are a valid float literal")
                .to_bits(),
            x.to_bits(),
            "the tie-broken digits must round-trip to the same f32"
        );
        decimal
    }

    /// The shortest Go digits for a finite, non-zero `f64`.
    ///
    /// The exact expansion of an `f64` never runs past 751 significant digits:
    /// the widest is the smallest subnormal, `2^-1074`, which is
    /// `5^1074 * 10^-1074`, and `5^1074` has 751 digits. Asking for 801 digits
    /// therefore always covers the whole expansion, with room to spare against
    /// the 767 digits usually quoted as the worst case.
    fn shortest_f64(x: f64) -> Self {
        let mut decimal = Self::of(&format!("{x:e}"));
        decimal.break_tie_to_even(|| Self::of(&format!("{x:.800e}")));
        debug_assert_eq!(
            decimal
                .exponent_form()
                .parse::<f64>()
                .expect("the digits are a valid float literal")
                .to_bits(),
            x.to_bits(),
            "the tie-broken digits must round-trip to the same f64"
        );
        decimal
    }

    /// Splits the output of Rust's `{:e}`, which is the shortest round-trip
    /// digit string at the value's own width plus a bare decimal exponent, or
    /// of `{:.Ne}`, which is the same shape at a fixed width.
    /// Only ever called with a finite, non-zero value.
    fn of(lower_exp: &str) -> Self {
        let (negative, rest) = match lower_exp.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, lower_exp),
        };
        let (mantissa, exp) = rest
            .split_once('e')
            .expect("LowerExp always writes an exponent");
        Self {
            negative,
            digits: mantissa.chars().filter(|c| *c != '.').collect(),
            exp: exp
                .parse()
                .expect("LowerExp always writes a decimal exponent"),
        }
    }

    /// Rewrites the shortest digits when the value sits exactly halfway between
    /// the two candidates of that width, which Go and Rust break differently.
    ///
    /// Both candidates round-trip, so both are legal shortest forms. Go picks
    /// the one whose last digit is even: in `ryuDigits32`
    /// (`strconv/ftoaryu.go`) the round-up flag is
    /// `cNextDigit > 5 || (cNextDigit == 5 && !c0) || (cNextDigit == 5 && c0 &&
    /// central&1 == 1)`, where `c0` means every trimmed digit after the 5 was a
    /// zero, so an exact half rounds up only away from an odd truncation. Rust
    /// rounds an exact half up unconditionally.
    ///
    /// `exact` produces the same value written at a precision no expansion can
    /// reach, so its digit string is the exact decimal expansion followed by
    /// padding zeros. It is a closure because the exact expansion costs far
    /// more than the shortest one and is only ever needed on the odd branch:
    /// the two candidates always have opposite parity, so digits already ending
    /// in an even digit are the ones Go would have picked anyway.
    fn break_tie_to_even<F: FnOnce() -> Self>(&mut self, exact: F) {
        let width = self.digits.len();
        if self.digits.as_bytes()[width - 1].is_multiple_of(2) {
            return;
        }
        let exact = exact();
        // A carry out of the shortest digits moves the exponent, and then the
        // truncation below is not the neighbour of these digits at all.
        if exact.exp != self.exp || exact.digits.len() <= width {
            return;
        }
        let (low, tail) = exact.digits.split_at(width);
        // Anything other than a lone 5 is not a tie: the value is strictly
        // nearer one candidate, and both formatters already agree on it.
        if !tail.starts_with('5') || tail[1..].bytes().any(|byte| byte != b'0') {
            return;
        }
        let Some(high) = increment(low) else {
            return;
        };
        if self.digits != low && self.digits != high {
            return;
        }
        // Incrementing without a carry out flips the last digit's parity, so
        // exactly one of the two candidates ends in an even digit.
        self.digits = if low.as_bytes()[width - 1].is_multiple_of(2) {
            low.to_owned()
        } else {
            high
        };
    }

    /// Go's `fmtF`: no exponent, and no decimal point on a whole number.
    fn plain_form(&self) -> String {
        let sign = if self.negative { "-" } else { "" };
        let nd = self.digits.len();
        // The position of the decimal point within the digit string.
        let dp = self.exp + 1;
        if dp <= 0 {
            let zeros = "0".repeat(dp.unsigned_abs() as usize);
            format!("{sign}0.{zeros}{}", self.digits)
        } else if dp as usize >= nd {
            let zeros = "0".repeat(dp as usize - nd);
            format!("{sign}{}{zeros}", self.digits)
        } else {
            let cut = dp as usize;
            format!("{sign}{}.{}", &self.digits[..cut], &self.digits[cut..])
        }
    }

    /// Go's `fmtE`: `d.ddde±dd`, with a sign and at least two exponent digits.
    fn exponent_form(&self) -> String {
        let sign = if self.negative { "-" } else { "" };
        let (lead, rest) = self.digits.split_at(1);
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let exp_sign = if self.exp < 0 { '-' } else { '+' };
        let magnitude = self.exp.unsigned_abs();
        let pad = if magnitude < 10 { "0" } else { "" };
        format!("{sign}{lead}{fraction}e{exp_sign}{pad}{magnitude}")
    }
}

/// The next digit string of the same width, or `None` when the increment
/// carries past the leading digit and so would change the decimal exponent.
fn increment(digits: &str) -> Option<String> {
    let mut bytes = digits.as_bytes().to_vec();
    for byte in bytes.iter_mut().rev() {
        if *byte == b'9' {
            *byte = b'0';
        } else {
            *byte += 1;
            return Some(String::from_utf8(bytes).expect("digits stay ASCII"));
        }
    }
    None
}
