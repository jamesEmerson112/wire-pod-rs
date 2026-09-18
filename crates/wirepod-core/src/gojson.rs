//! `encoding/json`, in the two halves the port has to reproduce: the
//! marshaller every state file is written through, and the decoder every state
//! file is read back through.
//!
//! Nothing here knows which file it is serving. A module brings its structs,
//! its tag tables and its own root; this module brings Go's behaviour. The rule
//! the port holds to is that every on-disk JSON file the server writes goes
//! through [`go_marshal`] and every one it reads goes through the [`GoObject`]
//! decoder, because every one of those files is also read and written by the Go
//! server: a rewrite has to be the bytes Go would have written, and a
//! hand-edited file has to be read the way Go would read it. `apiConfig.json`
//! ([`crate::config`]) was the first, `jdocs.json`
//! ([`crate::store::jdocs`]) the second.
//!
//! **Encoding.** `serde_json` and `encoding/json` agree on everything but the
//! escaping, and `json.Marshal` calls `appendString` with `escapeHTML` true.
//! That escapes `<`, `>` and `&`, which the `htmlSafeSet` test at
//! `encode.go:984` sends to the `\u` branch "because they can lead to security
//! holes when user-controlled strings are rendered into JSON and served to some
//! browsers" (`encode.go:1005-1007`), and U+2028 and U+2029 unconditionally,
//! because they are line terminators in JavaScript (`encode.go:1030-1043`).
//! `serde_json` escapes none of the five, and every file the port writes
//! carries text somebody typed, so the difference is ordinary rather than
//! exotic: an operator's prompt in `apiConfig.json`, a robot setting inside a
//! jdoc's `json_doc`, a log message on its way to the web UI.
//! [`GoFormatter`] is the difference and [`go_marshal`] is the entry point.
//!
//! **Decoding.** `encoding/json`'s object loop (`decode.go:661-827`) is four
//! rules that `serde`'s derive does not have, and all four are visible in a
//! hand-edited file:
//!
//! 1. A key matches a tag exactly, or, failing that, by fold: Go looks the key
//!    up in `byExactName` and then in `byFoldedName` (`decode.go:694-697`), and
//!    the fold is `fold.go`'s, an ASCII upper-casing plus the two runes whose
//!    simple fold set reaches ASCII ([`fold_name`]).
//! 2. A duplicate key is applied on top of what the earlier one left, because
//!    Go decodes into a `subv` that points into the struct rather than into a
//!    fresh value (`decode.go:762`). The last occurrence of a scalar wins, and
//!    a second nested object is merged into the first rather than replacing it.
//! 3. A JSON `null` leaves a non-pointer field exactly as it was
//!    (`decode.go:899-903`), so it can neither clear a field nor raise a fault.
//! 4. A value whose type does not fit the field its key matched records the
//!    first such fault and decoding carries on through every other field
//!    (`decode.go:243-247`). The caller therefore gets a half-filled value
//!    *and* a fault, which is why [`Decoded`] hands back both and why each
//!    module's error type carries whatever decoded beside its [`DecodeFault`].
//!
//! **Forward compatibility.** Go's decoder drops any key its struct does not
//! name, so a fork-only or newer-version key is lost the moment the Go server
//! rewrites the file. Every object the port declares carries an [`Extra`] map
//! instead, filled by [`GoObject::unknown`], so an unknown key survives a
//! read-modify-write. That is what makes rolling back to the Go server safe in
//! one direction and forward in the other, and it is a deliberate difference
//! from Go rather than a shortfall.

use std::collections::BTreeMap;
use std::fmt;
use std::io;

use serde::de::Error as _;
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

/// The keys one JSON object carried that the struct beside it does not name.
///
/// Go has no counterpart: `encoding/json` drops an unknown key silently, so a
/// Go rewrite erases anything a fork or a later version wrote. Keeping them is
/// what lets this server and the Go one take turns over the same file without
/// either erasing the other's additions.
///
/// A [`BTreeMap`] rather than an order-preserving map because the position is
/// not recoverable anyway: `serde`'s flatten hands every surviving key to the
/// map at the position the `extra` field is declared at, which is last in every
/// struct that carries one. Sorted order at least makes the rewrite
/// deterministic, so two boots over the same file produce the same bytes.
pub type Extra = BTreeMap<String, Value>;

// ---------------------------------------------------------------------------
// Marshalling the way Go marshals
// ---------------------------------------------------------------------------

/// `encoding/json`'s string escaping, which `serde_json`'s default formatter
/// does not reproduce.
///
/// Go's `appendString` is called with `escapeHTML` true from `json.Marshal`,
/// and escapes five characters that are legal unescaped JSON:
/// `<`, `>` and `&`, which the `htmlSafeSet` test at
/// `encoding/json/encode.go:984` sends to the `\u` branch "because they can
/// lead to security holes when user-controlled strings are rendered into JSON
/// and served to some browsers" (`encode.go:1005-1007`), and U+2028 and U+2029
/// unconditionally, because they are line terminators in JavaScript
/// (`encode.go:1030-1043`). `serde_json` escapes none of the five.
///
/// This matters wherever a file carries free text somebody typed, which is
/// every file the port writes: `knowledge.openai_prompt` and
/// `knowledge.robotName` reach `apiConfig.json` exactly as the web UI's form
/// submitted them, a jdoc's `json_doc` is a JSON *string* holding a robot
/// setting's JSON *text*, and a log entry's `msg` carries whatever a robot, an
/// operator or an LLM put in it. Without this the first Rust rewrite of such a
/// file would change bytes the Go server would change back.
///
/// Everything else the two encoders do agrees, which the escape tables make
/// checkable rather than assumed: Go writes `\"`, `\\`, `\b`, `\f`, `\n`, `\r`
/// and `\t` as short forms and every other byte below 0x20 as `\u00XX`
/// (`encode.go:989-1008`), and `serde_json`'s `ESCAPE` table and
/// `write_char_escape` name exactly the same set. Neither escapes `/`, and
/// neither escapes non-ASCII. Go's one remaining case, invalid UTF-8 becoming
/// `\ufffd` (`encode.go:1022-1028`), cannot arise: a Rust [`String`] is UTF-8
/// by construction.
#[derive(Clone, Copy, Debug, Default)]
pub struct GoFormatter;

impl serde_json::ser::Formatter for GoFormatter {
    /// Escapes the five characters Go escapes and `serde_json` does not.
    ///
    /// A fragment is a run `serde_json` already decided needs no escaping of
    /// its own, so nothing here can collide with an escape the caller will
    /// write: the quote and the backslash never reach this function.
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        // The run is copied out of the byte slice rather than out of the `str`
        // because every bound here is already a character boundary that
        // `char_indices` handed over, so re-checking one costs without
        // deciding anything.
        let bytes = fragment.as_bytes();
        let mut written = 0;
        for (index, character) in fragment.char_indices() {
            let escape = match character {
                '<' => r"\u003c",
                '>' => r"\u003e",
                '&' => r"\u0026",
                '\u{2028}' => r"\u2028",
                '\u{2029}' => r"\u2029",
                _ => continue,
            };
            writer.write_all(&bytes[written..index])?;
            writer.write_all(escape.as_bytes())?;
            written = index + character.len_utf8();
        }
        writer.write_all(&bytes[written..])
    }
}

/// Marshals `value` the way Go's `json.Marshal` would: compact, in declaration
/// order, with [`GoFormatter`]'s string escaping and no trailing newline.
///
/// Every state file the port writes has to match Go's marshaller byte for byte
/// rather than merely produce the same JSON, which is why this lives here
/// rather than beside any one of them.
///
/// # Errors
///
/// Only what the value's own [`Serialize`] reports. Writing into a [`Vec`]
/// cannot fail, so for a type built out of integers, strings, bools and
/// [`RawValue`]s rendered from finite floats there is no failure left.
pub fn go_marshal<T>(value: &T) -> serde_json::Result<Vec<u8>>
where
    T: ?Sized + Serialize,
{
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, GoFormatter);
    value.serialize(&mut serializer)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Decoding the way `encoding/json` decodes
// ---------------------------------------------------------------------------

/// What `encoding/json` found where a value was expected.
///
/// `literalStore` switches on the first byte of a literal (`decode.go:891`) and
/// the parser sends objects and arrays to `object` and `array` instead, so
/// these six are the whole vocabulary a type error is reported in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// `decode.go:892`.
    Null,
    /// `decode.go:904`.
    Bool,
    /// `decode.go:929`.
    String,
    /// `decode.go:966`.
    Number,
    /// `decode.go:650`.
    Object,
    /// `decode.go:529`.
    Array,
}

impl Kind {
    /// The word Go's `UnmarshalTypeError` carries for it (`decode.go:869-875`,
    /// `:917`, `:939`, `:984`, `:650`, `:529`).
    fn as_str(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "bool",
            Self::String => "string",
            Self::Number => "number",
            Self::Object => "object",
            Self::Array => "array",
        }
    }
}

/// The text of a captured value, without the whitespace around it.
///
/// `serde_json` hands a [`RawValue`] the exact bytes of the value it scanned,
/// so the trim has nothing to do on any document this reads; it is here so that
/// [`kind`] can index the first byte without depending on that. A field writer
/// a module brings of its own wants it too, to read the literal it is about to
/// parse.
pub fn text(raw: &RawValue) -> &str {
    raw.get().trim()
}

/// What `raw` is, which is what Go dispatches on (`decode.go:891`).
pub fn kind(raw: &RawValue) -> Kind {
    match text(raw).as_bytes().first() {
        Some(b'n') => Kind::Null,
        Some(b't' | b'f') => Kind::Bool,
        Some(b'"') => Kind::String,
        Some(b'{') => Kind::Object,
        Some(b'[') => Kind::Array,
        // A captured value is never empty, so the remaining head bytes are a
        // digit or a minus sign.
        _ => Kind::Number,
    }
}

/// Go's `foldName` for one rune (`fold.go:39-48`): the smallest rune its simple
/// fold set contains.
///
/// Only the two runes below can matter. A folded key matches a folded tag, and
/// every tag the port declares is ASCII, so a key rune that does not fold to
/// ASCII can never be part of a match; sweeping the whole of Unicode against
/// Go's own `unicode.SimpleFold` finds exactly two non-ASCII runes that do,
/// U+017F LATIN SMALL LETTER LONG S and U+212A KELVIN SIGN. Every other
/// non-ASCII rune is left alone, which is not always its fold but is always a
/// rune that cannot match.
fn fold_rune(character: char) -> char {
    match character {
        '\u{017f}' => 'S',
        '\u{212a}' => 'K',
        other => other,
    }
}

/// Go's `foldName` (`fold.go:20-37`): ASCII lower case becomes upper case and
/// everything else goes through `fold_rune`.
pub fn fold_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii() {
                character.to_ascii_uppercase()
            } else {
                fold_rune(character)
            }
        })
        .collect()
}

/// Go's field lookup (`decode.go:694-697`): the exact tag if there is one, and
/// otherwise the first tag that folds to the same name.
///
/// Go breaks a tie between two fields that fold together by taking the first in
/// name order and says it does so "for historical reasons"
/// (`encode.go:1301-1304`). No two tags in any table the port declares fold
/// together, which `config`'s `no_two_tags_fold_together` test pins, so the
/// tie-break is unobservable and declaration order stands in for it.
pub fn resolve(tags: &[&'static str], key: &str) -> Option<&'static str> {
    if let Some(tag) = tags.iter().find(|tag| **tag == key) {
        return Some(tag);
    }
    let folded = fold_name(key);
    tags.iter().find(|tag| fold_name(tag) == folded).copied()
}

/// Why a document did not decode cleanly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeFault {
    /// The bytes are not one JSON value. Go finds this in `checkValid` before
    /// it decodes anything (`decode.go:98-105`), which is why a document with
    /// this fault leaves the target at its zero value where one with a
    /// [`DecodeFault::Type`] does not.
    Malformed(String),
    /// A value's type did not fit the field its key matched. Go records the
    /// first of these, carries on to the end of the document, and returns it
    /// (`decode.go:243-247`, `:182`).
    Type {
        /// The tag path from the document root, `knowledge.top_p` say, and the
        /// empty string for the document as a whole.
        path: String,
        /// What was there, in `encoding/json`'s vocabulary, with the literal
        /// appended for a number that does not fit the field, which is the one
        /// case Go's own message quotes (`decode.go:1000`, `:1016`). A string
        /// is never quoted, in Go or here, so no key or prompt can reach a log
        /// line through this.
        found: String,
        /// The Go type the field is declared as: a builtin the field writer
        /// names, or the nested object's own [`GoObject::GO_TYPE`].
        want: &'static str,
    },
}

impl fmt::Display for DecodeFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(message) => formatter.write_str(message),
            Self::Type { path, found, want } if path.is_empty() => {
                write!(formatter, "cannot unmarshal {found} into {want}")
            }
            Self::Type { path, found, want } => {
                write!(
                    formatter,
                    "cannot unmarshal {found} into {path} of type {want}"
                )
            }
        }
    }
}

impl std::error::Error for DecodeFault {}

/// The first fault one decode recorded, and nothing after it
/// (`decode.go:241-247`).
#[derive(Debug, Default)]
pub struct Faults {
    /// The first fault, which is the only one Go keeps.
    pub first: Option<DecodeFault>,
}

impl Faults {
    /// Records a type mismatch against the field `prefix` + `tag`.
    pub fn save(&mut self, prefix: &str, tag: &str, found: Kind, want: &'static str) {
        self.save_text(format!("{prefix}{tag}"), found.as_str().to_owned(), want);
    }

    /// The same for a number whose literal Go puts in the message
    /// (`decode.go:1000`, `:1016`).
    pub fn save_number(&mut self, prefix: &str, tag: &str, literal: &str, want: &'static str) {
        self.save_text(format!("{prefix}{tag}"), format!("number {literal}"), want);
    }

    /// The same against the document as a whole (`decode.go:650`).
    pub fn save_root(&mut self, found: Kind, want: &'static str) {
        self.save_text(String::new(), found.as_str().to_owned(), want);
    }

    /// `decode.go:243-246`: the first call wins and every later one is
    /// discarded.
    fn save_text(&mut self, path: String, found: String, want: &'static str) {
        if self.first.is_none() {
            self.first = Some(DecodeFault::Type { path, found, want });
        }
    }
}

/// One Go struct, seen the way `encoding/json` sees it.
///
/// The trait is what lets one object loop serve them all, which is the point:
/// the loop is Go's and the structs only say which tags they have and what to
/// do with a value once a key has been matched to one. [`crate::config`]
/// implements it for the six structs `apiConfig.json` is made of and
/// [`crate::store::jdocs`] for the two `jdocs.json` is made of.
pub trait GoObject: Default {
    /// The tags a key is matched against, in Go's declaration order
    /// (`decode.go:694-697`).
    const TAGS: &'static [&'static str];
    /// The path this object's fields hang off, `"knowledge."` say, and the
    /// empty string at the root.
    const PREFIX: &'static str;
    /// What a fault raised against the object itself calls it.
    const GO_TYPE: &'static str;

    /// Stores one value into the field `tag` names, the way `d.value(subv)`
    /// does (`decode.go:762`): `subv` points into the struct, so a second
    /// occurrence of a key is applied on top of what the first one left rather
    /// than into a fresh value.
    ///
    /// # Errors
    ///
    /// Only what a nested object's own parse reports, which is a key or a value
    /// `serde_json` refuses to represent and never the shape of the document.
    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()>;

    /// The keys Go drops (`decode.go:734-736`) and this keeps.
    fn unknown(&mut self) -> &mut Extra;
}

/// Go's object loop (`decode.go:661-827`), over a struct that already exists.
fn merge_object<'de, T, A>(target: &mut T, mut map: A, faults: &mut Faults) -> Result<(), A::Error>
where
    T: GoObject,
    A: MapAccess<'de>,
{
    while let Some(key) = map.next_key::<String>()? {
        let raw: Box<RawValue> = map.next_value()?;
        match resolve(T::TAGS, &key) {
            // `decode.go:698-733`.
            Some(tag) => target.store(tag, &raw, faults).map_err(A::Error::custom)?,
            // `decode.go:734-736`, except that Go drops the key and this keeps
            // it. A value `serde_json` cannot represent, a number outside
            // `f64` say, is dropped rather than reported, which is what Go does
            // with every unknown key.
            None => {
                if let Ok(value) = serde_json::from_str::<Value>(text(&raw)) {
                    target.unknown().insert(key, value);
                }
            }
        }
    }
    Ok(())
}

/// Go's `d.object(v)` for a struct that already exists (`decode.go:599`).
struct MergeVisitor<'a, T> {
    target: &'a mut T,
    faults: &'a mut Faults,
}

impl<'de, T: GoObject> Visitor<'de> for MergeVisitor<'_, T> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        merge_object(self.target, map, self.faults)
    }
}

/// Runs the object loop over `raw`, which the caller has established is an
/// object.
fn merge_into<T: GoObject>(
    target: &mut T,
    raw: &RawValue,
    faults: &mut Faults,
) -> serde_json::Result<()> {
    let mut deserializer = serde_json::Deserializer::from_str(text(raw));
    serde::Deserializer::deserialize_map(&mut deserializer, MergeVisitor { target, faults })
}

/// Go's `d.value(rv)` at the top of an unmarshal (`decode.go:178`).
///
/// An object is decoded; a `null` changes nothing and reports nothing
/// (`decode.go:899-903`); anything else is a type error against the struct
/// itself and leaves it at its zero value (`decode.go:650-652`).
struct RootVisitor<'a, T> {
    target: &'a mut T,
    faults: &'a mut Faults,
}

impl<'de, T: GoObject> Visitor<'de> for RootVisitor<'_, T> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        merge_object(self.target, map, self.faults)
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::Bool, T::GO_TYPE);
        Ok(())
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::Number, T::GO_TYPE);
        Ok(())
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::Number, T::GO_TYPE);
        Ok(())
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::Number, T::GO_TYPE);
        Ok(())
    }

    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
        self.faults.save_root(Kind::String, T::GO_TYPE);
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        // Go's `array` skips the whole value before it returns
        // (`decode.go:529-530`), and `serde` needs the same: an element left
        // unread is a parse still in the middle of the array.
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        self.faults.save_root(Kind::Array, T::GO_TYPE);
        Ok(())
    }
}

/// One `json.Unmarshal` into a fresh value whose root is an object: what
/// decoded, and the first fault.
///
/// It exists because a [`Deserialize`] impl can return a value or an error and
/// Go's decoder produces both at once. A module's own [`Deserialize`] impls are
/// this one with the fault dropped, and its entry point
/// ([`crate::config::ApiConfig::from_json_bytes`], say) is the one that keeps
/// it. A file whose root is not an object brings a root of its own instead;
/// [`crate::store::jdocs`] is the one that does.
pub struct Decoded<T> {
    /// Every field that decoded, the ones before the fault and the ones after
    /// it alike.
    pub value: T,
    /// The first fault, if the document had one.
    pub fault: Option<DecodeFault>,
}

impl<'de, T: GoObject> Deserialize<'de> for Decoded<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = T::default();
        let mut faults = Faults::default();
        deserializer.deserialize_any(RootVisitor {
            target: &mut value,
            faults: &mut faults,
        })?;
        Ok(Self {
            value,
            fault: faults.first,
        })
    }
}

// The field writers every file's fields are made of. Each is one arm of Go's
// `literalStore`, and each leaves the field alone for a `null` the way Go does
// (`decode.go:899-903`). A module whose field has no arm here writes its own
// against [`kind`], [`text`] and [`Faults`]; [`crate::config`] has two, for the
// two shapes only `apiConfig.json` has.

/// `decode.go:904-927`.
pub fn store_bool(
    slot: &mut bool,
    raw: &RawValue,
    prefix: &str,
    tag: &'static str,
    faults: &mut Faults,
) {
    match kind(raw) {
        Kind::Null => {}
        Kind::Bool => *slot = text(raw) == "true",
        found => faults.save(prefix, tag, found, "bool"),
    }
}

/// `decode.go:929-964`.
pub fn store_string(
    slot: &mut String,
    raw: &RawValue,
    prefix: &str,
    tag: &'static str,
    faults: &mut Faults,
) {
    match kind(raw) {
        Kind::Null => {}
        Kind::String => match serde_json::from_str::<String>(text(raw)) {
            Ok(value) => *slot = value,
            // Go's `unquoteBytes` replaces an escape it cannot decode, a lone
            // surrogate say, with U+FFFD (`decode.go:930`); `serde_json`
            // refuses it, so the field keeps what it had and the fault stands
            // in for Go's replacement.
            Err(_) => faults.save(prefix, tag, Kind::String, "string"),
        },
        found => faults.save(prefix, tag, found, "string"),
    }
}

/// `decode.go:1005-1011`.
///
/// Go's `strconv.ParseUint(item, 10, 64)` refuses a sign, a fraction, an
/// exponent and anything past `u64::MAX`, and `u64`'s own `FromStr` refuses the
/// same set: the one spelling they disagree about, a leading `+`, is not a JSON
/// number, so a [`RawValue`] can never carry it here. Go appends the literal to
/// the message in this branch, as it does for the integer and float ones and
/// unlike every other arm (`decode.go:1008`).
///
/// [`crate::store::jdocs`] is the caller, for `doc_version` and `fmt_version`
/// (`vars.go:131-132`).
pub fn store_u64(
    slot: &mut u64,
    raw: &RawValue,
    prefix: &str,
    tag: &'static str,
    faults: &mut Faults,
) {
    match kind(raw) {
        Kind::Null => {}
        Kind::Number => {
            let literal = text(raw);
            match literal.parse::<u64>() {
                Ok(value) => *slot = value,
                Err(_) => faults.save_number(prefix, tag, literal, "uint64"),
            }
        }
        found => faults.save(prefix, tag, found, "uint64"),
    }
}

/// One nested object, which Go merges into whatever the field already holds
/// rather than replacing (`decode.go:599-827` writes through a `subv` that
/// points into the struct).
///
/// # Errors
///
/// Only what the nested object's own parse reports.
pub fn store_object<T: GoObject>(
    target: &mut T,
    raw: &RawValue,
    prefix: &str,
    tag: &'static str,
    faults: &mut Faults,
) -> serde_json::Result<()> {
    match kind(raw) {
        Kind::Null => Ok(()),
        Kind::Object => merge_into(target, raw, faults),
        found => {
            faults.save(prefix, tag, found, T::GO_TYPE);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go's fold is an ASCII upper-casing plus the two runes whose simple fold
    /// set reaches ASCII, and nothing else may move: a fold that also
    /// lower-cased, or that normalised an accent, would match keys Go leaves
    /// alone.
    #[test]
    fn the_fold_is_gos() {
        assert_eq!(fold_name("robotName"), "ROBOTNAME");
        assert_eq!(fold_name("top_p"), "TOP_P");
        assert_eq!(fold_name("STT"), "STT");
        // `fold.go:23-30`: only a-z moves in the ASCII range.
        assert_eq!(fold_name("a_1-Z{"), "A_1-Z{");
        // `fold.go:39-48`, and the sweep of Unicode that found exactly these.
        assert_eq!(fold_name("\u{017f}tt"), "STT");
        assert_eq!(fold_name("\u{212a}ey"), "KEY");
        // Every other non-ASCII rune is left as it is, which cannot match an
        // ASCII tag either way.
        assert_eq!(fold_name("\u{00e9}"), "\u{00e9}");
    }
}
