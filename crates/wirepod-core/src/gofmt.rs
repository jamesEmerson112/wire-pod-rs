//! Go-compatible float formatting.
//!
//! Two Go behaviors reach the wire and have to be reproduced byte for byte:
//! `fmt.Sprintf("%v", x)` on a `float32`, which `get_stim_status` and the
//! `custom_eye_color` echo emit, and `encoding/json`'s `float64` encoding,
//! which carries `net_probe`'s `rttMs`. Rust's own `Display` agrees with
//! neither: it writes `14` as `14`, but also `1e6` as `1000000` where Go writes
//! `1e+06`, and `serde_json` always emits a decimal point.
//!
//! Both functions are pinned by `docs/phases/P4-sdk-app/gofmt-probe/expected.txt`,
//! which is the recorded stdout of the Go probe program committed beside it.

use std::fmt;

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
    let decimal = Decimal::of(&format!("{x:e}"));
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
    let decimal = Decimal::of(&format!("{x:e}"));
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

/// A finite, non-zero float split into sign, shortest round-trip digits and the
/// decimal exponent, so that `value = 0.d0 d1 ... * 10^(exp + 1)`.
struct Decimal {
    negative: bool,
    digits: String,
    exp: i32,
}

impl Decimal {
    /// Splits the output of Rust's `{:e}`, which is the shortest round-trip
    /// digit string at the value's own width plus a bare decimal exponent.
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
