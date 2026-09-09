//! Robot identity: the normalized serial number and the ownership generation.

use std::fmt;

/// A robot serial number in canonical form: trimmed and ASCII-lowercased.
///
/// Go stores `strings.TrimSpace(strings.ToLower(serial))` and then compares
/// every lookup with `strings.EqualFold` (`robot.go:335`, `robot.go:414`).
/// Normalizing once on construction gives `Eq` and `Hash` the same behavior,
/// so an ESN-keyed map matches the Go lookups without a custom comparator.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Esn(String);

impl Esn {
    /// Normalizes `raw` into an `Esn`.
    pub fn new(raw: &str) -> Self {
        Self(raw.trim().to_ascii_lowercase())
    }

    /// The normalized serial.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the normalized serial is empty, which is how a request with no
    /// `serial` form value arrives.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Esn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Esn {
    fn from(raw: &str) -> Self {
        Self::new(raw)
    }
}

impl From<String> for Esn {
    fn from(raw: String) -> Self {
        Self::new(&raw)
    }
}

/// An opaque, monotone ownership generation.
///
/// Go increments a plain `uint64` that starts at zero and hands out the value
/// after the increment (`robot.go:104-117`), so the first issued generation is
/// 1 and 0 is never a valid owner. [`Generation::UNCLAIMED`] is that zero.
///
/// Only equality and ordering are meaningful; the counter is not exposed. The
/// type is `Generation` and every field and local is `generation`, because Go's
/// `gen` is a reserved keyword in edition 2024.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Generation(u64);

impl Generation {
    /// The generation that means "never claimed". Never handed to an owner.
    pub const UNCLAIMED: Self = Self(0);

    /// The first generation an owner can be issued.
    pub const fn first() -> Self {
        Self(1)
    }

    /// The generation issued after this one.
    ///
    /// The increment wraps, because Go's `camGen++` on a `uint64` wraps rather
    /// than panicking and nothing in the port should differ from it. Reaching
    /// the wrap needs 2^64 claims, so the only real effect is that a debug
    /// build cannot panic here where a release build would not.
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    /// True for any generation that has actually been issued.
    pub const fn is_claimed(self) -> bool {
        self.0 != 0
    }
}
