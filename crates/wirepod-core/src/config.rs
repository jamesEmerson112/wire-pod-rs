//! `apiConfig.json`: the one settings file the web UI edits and the server
//! rewrites at every boot.
//!
//! Go keeps it as a package-level `apiConfig` value with three writers
//! (`config.go:61-65`, `:67-100`, `:111-158`) and one reader per consumer, all
//! reaching the same global. Here it is a value the boot path produces and
//! hands to `AppState`, so a test can point a whole configuration cycle at a
//! temporary directory.
//!
//! The file is a contract in three separate ways, and each of them is a
//! separate mechanism below.
//!
//! **Byte layout.** Go writes `json.Marshal` output straight into the file with
//! no indentation and no trailing newline (`config.go:63-64`, `:98-99`,
//! `:154-155`), so the port's rewrite has to reproduce Go's marshaller and not
//! merely produce equivalent JSON. Three things separate `serde_json`'s output
//! from Go's, and all three are handled: field order and tags come from the
//! struct declarations, which are `config.go:17-59` field for field; the two
//! `float32` fields are carried as pre-rendered [`RawValue`]s so a `top_p` of
//! `0.7` stays `0.7` and a `temp` of `1` stays `1`
//! ([`crate::gofmt::go_json_f32_raw`]); and every string is written through
//! [`GoFormatter`], which reproduces `encoding/json`'s HTML escaping.
//!
//! **Decoding.** The file is also read back, and a hand-edited one is on-disk
//! state like any other, so the decoder is Go's rather than `serde`'s.
//! [`ApiConfig`] and the five nested structs carry hand-written
//! [`Deserialize`] impls that reproduce `encoding/json`'s object loop
//! (`decode.go:661-827`): a key matches a tag exactly or, failing that,
//! case insensitively; a duplicate key is applied on top of what the earlier
//! one left; a JSON `null` leaves a non-pointer field alone; and a value whose
//! type does not fit its field records the first such fault and decoding
//! carries on through every other field (`decode.go:243-247`). That last rule
//! is why [`ApiConfig::from_json_bytes`]'s error carries a configuration:
//! Go's `ReadConfig` leaves the partially decoded global in place for the rest
//! of the process, and `vars.go:234` reads `APIConfig.STT.Service` out of it
//! immediately afterwards.
//!
//! **Forward compatibility.** Go's decoder drops any key its struct does not
//! name, so a fork-only or newer-version key is lost the moment the Go server
//! rewrites the file. Every object here carries a `#[serde(flatten)]` map
//! instead, so an unknown key survives a read-modify-write. That is what makes
//! rolling back to the Go server safe in one direction and forward in the
//! other, and it is a deliberate difference from Go rather than a shortfall.
//! A key that folds to a tag is not unknown: it fills the field, as it does in
//! Go, and the rewrite spells it Go's way.
//!
//! **Boot side effects.** [`read_config`] is not a loader. Go's `ReadConfig`
//! rewrites the file on every successful boot and can change four things on
//! the way past: the STT provider, the two setup flags, the Together model
//! name and the go-home percent. All four are reproduced in Go's order and
//! each is cited at its call site.
//!
//! Nothing here reads the process environment or decides a path on its own.
//! [`Env`] is the twelve variables `config.go` calls `os.Getenv` for, and
//! [`DataDir`] is the file's location, both injected. Wiring this into the
//! binary is the boot commit's job, not this module's.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::PathBuf;

use serde::de::Error as _;
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::gofmt::{GoJsonError, go_json_f32_raw};
use crate::paths::DataDir;
use crate::persist::write_atomic;

/// The permission bit set Go hands `os.WriteFile` at all three config write
/// sites (`config.go:64`, `:99`, `:155`).
///
/// It is the same at each of them, unlike the bot-info and session-certificate
/// files whose writers disagree, so one constant covers the file.
pub const CONFIG_FILE_MODE: u32 = 0o644;

/// The go-home battery percent Go forces when the file carries none
/// (`config.go:93`, `:150`).
///
/// Go's comment at `config.go:53-55` is the whole meaning of the field: nil is
/// this default and zero is "disabled", which is why the field is a pointer in
/// Go and an [`Option`] here rather than a plain number with a sentinel.
pub const DEFAULT_GOHOME_PERCENT: i32 = 25;

/// The Together model name `ReadConfig` rewrites away (`config.go:144`).
pub const LLAMA2_MODEL: &str = "meta-llama/Llama-2-70b-chat-hf";

/// What it is rewritten to (`config.go:146`).
pub const LLAMA3_MODEL: &str = "meta-llama/Llama-3-70b-chat-hf";

/// The value `CreateConfigFromEnv` tests two environment variables against
/// (`config.go:69`, `:77`). Anything else, the empty string included, is off.
const ENABLED: &str = "true";

/// The knowledge provider that alone makes `KNOWLEDGE_ID` meaningful
/// (`config.go:80`).
const HOUNDIFY: &str = "houndify";

/// The two STT services that alone make `STT_LANGUAGE` meaningful
/// (`config.go:106`).
const LANGUAGE_AWARE_STT: [&str; 2] = ["vosk", "whisper.cpp"];

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
/// struct here. Sorted order at least makes the rewrite deterministic, so two
/// boots over the same file produce the same bytes.
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
/// This matters here because `apiConfig.json` carries free text that an
/// operator types: `knowledge.openai_prompt` and `knowledge.robotName` reach
/// the file exactly as the web UI's form submitted them, and a prompt
/// containing `&` or an angle bracket is ordinary. Without this the first Rust
/// rewrite of such a file would change bytes the Go server would change back.
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
/// It lives in this module because the config file is the first thing in the
/// port that has to match Go's marshaller byte for byte rather than merely
/// produce the same JSON. Every other state file the port writes has the same
/// requirement, so a later commit may well lift it somewhere more central.
///
/// # Errors
///
/// Only what the value's own [`Serialize`] reports. Writing into a [`Vec`]
/// cannot fail, so for a type built out of the ones in this module there is no
/// failure left.
pub fn go_marshal<T>(value: &T) -> serde_json::Result<Vec<u8>>
where
    T: ?Sized + Serialize,
{
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, GoFormatter);
    value.serialize(&mut serializer)?;
    Ok(out)
}

/// The rendering of a zero `float32`, which is the Go zero value of `top_p` and
/// `temp` and therefore what a missing one falls back to.
fn zero_f32() -> Box<RawValue> {
    go_json_f32_raw(0.0).expect("zero is a finite float32")
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
enum Kind {
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
/// [`kind`] can index the first byte without depending on that.
fn text(raw: &RawValue) -> &str {
    raw.get().trim()
}

/// What `raw` is, which is what Go dispatches on (`decode.go:891`).
fn kind(raw: &RawValue) -> Kind {
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
/// every tag in this file is ASCII, so a key rune that does not fold to ASCII
/// can never be part of a match; sweeping the whole of Unicode against Go's own
/// `unicode.SimpleFold` finds exactly two non-ASCII runes that do, U+017F LATIN
/// SMALL LETTER LONG S and U+212A KELVIN SIGN. Every other non-ASCII rune is
/// left alone, which is not always its fold but is always a rune that cannot
/// match.
fn fold_rune(character: char) -> char {
    match character {
        '\u{017f}' => 'S',
        '\u{212a}' => 'K',
        other => other,
    }
}

/// Go's `foldName` (`fold.go:20-37`): ASCII lower case becomes upper case and
/// everything else goes through [`fold_rune`].
fn fold_name(name: &str) -> String {
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
/// (`encode.go:1301-1304`). No two tags in this file fold together, which
/// [`tests::no_two_tags_fold_together`] pins, so the tie-break is unobservable
/// here and declaration order stands in for it.
fn resolve(tags: &[&'static str], key: &str) -> Option<&'static str> {
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
    /// this fault leaves the configuration at its zero value where one with a
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
        /// The Go type the field is declared as (`config.go:17-59`), with
        /// `struct` standing in for the five anonymous ones.
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

/// What `json.Unmarshal` left behind when it reported an error
/// (`decode.go:182`).
///
/// Go has no such type: it fills the package global as it goes and returns the
/// error separately, so the caller reads a half-filled configuration out of the
/// global whether it looks at the error or not. `ReadConfig` does exactly that
/// (`config.go:125-132`), and `vars.go:234` reads `APIConfig.STT.Service` out
/// of it on the next line, so the half-filled value is part of the contract and
/// the error carries it here.
#[derive(Debug)]
pub struct DecodeError {
    /// Every field that decoded, the ones before the fault and the ones after
    /// it alike.
    pub config: ApiConfig,
    /// The first fault, which is the only one Go keeps.
    pub fault: DecodeFault,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fault.fmt(formatter)
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.fault)
    }
}

/// The first fault one decode recorded, and nothing after it
/// (`decode.go:241-247`).
#[derive(Debug, Default)]
struct Faults {
    first: Option<DecodeFault>,
}

impl Faults {
    /// Records a type mismatch against the field `prefix` + `tag`.
    fn save(&mut self, prefix: &str, tag: &str, found: Kind, want: &'static str) {
        self.save_text(format!("{prefix}{tag}"), found.as_str().to_owned(), want);
    }

    /// The same for a number whose literal Go puts in the message
    /// (`decode.go:1000`, `:1016`).
    fn save_number(&mut self, prefix: &str, tag: &str, literal: &str, want: &'static str) {
        self.save_text(format!("{prefix}{tag}"), format!("number {literal}"), want);
    }

    /// The same against the document as a whole (`decode.go:650`).
    fn save_root(&mut self, found: Kind, want: &'static str) {
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

/// One of Go's six configuration structs, seen the way `encoding/json` sees it.
///
/// The trait is what lets one object loop serve all six, which is the point:
/// the loop is Go's and the structs only say which tags they have and what to
/// do with a value once a key has been matched to one.
trait GoObject: Default {
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

/// One `json.Unmarshal` into a fresh value: what decoded, and the first fault.
///
/// It exists because a [`Deserialize`] impl can return a value or an error and
/// Go's decoder produces both at once. The public impls below are this one with
/// the fault dropped, and [`ApiConfig::from_json_bytes`] is the entry point that
/// keeps it.
struct Decoded<T> {
    value: T,
    fault: Option<DecodeFault>,
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

// The five field writers. Each is one arm of Go's `literalStore`, and each
// leaves the field alone for a `null` the way Go does (`decode.go:899-903`).

/// `decode.go:904-927`.
fn store_bool(
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
fn store_string(
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

/// `decode.go:1013-1019`.
///
/// The literal is parsed once, directly to an [`f32`], which is what
/// `strconv.ParseFloat(s, 32)` is: going through an [`f64`] first would round
/// twice and put a literal with sixteen or more significant digits one unit in
/// the last place away from Go's answer. The rendering happens here rather than
/// on the way out so the field always holds Go's spelling of some `float32`,
/// whatever spelling the file used, which is Go's behaviour too: a hand-edited
/// `0.70` comes back as `0.7` and a `1.0` comes back as `1`.
fn store_f32(
    slot: &mut Box<RawValue>,
    raw: &RawValue,
    prefix: &str,
    tag: &'static str,
    faults: &mut Faults,
) {
    match kind(raw) {
        Kind::Null => {}
        Kind::Number => {
            let literal = text(raw);
            match literal.parse::<f32>() {
                // Rust answers an infinity where `strconv.ParseFloat` answers
                // one and an `ErrRange` beside it, and Go turns that range
                // error into the same type error every other misfit gets.
                Ok(value) if value.is_finite() => {
                    *slot = go_json_f32_raw(value).expect("a finite float32 renders");
                }
                _ => faults.save_number(prefix, tag, literal, "float32"),
            }
        }
        found => faults.save(prefix, tag, found, "float32"),
    }
}

/// `decode.go:997-1003`, reached through the pointer `indirect` walks.
///
/// The pointer is the reason this is not [`store_bool`]'s shape. `indirect`
/// allocates every pointer on the way to the value it is about to store
/// (`decode.go:476-478`), so a field that then takes a type error is left
/// pointing at a zero rather than at nothing; for a `null` it stops at the
/// pointer instead (`decode.go:465-467`) and `literalStore` sets it back to nil
/// (`decode.go:899-901`), whatever the field held before.
fn store_gohome(
    slot: &mut Option<i32>,
    raw: &RawValue,
    prefix: &str,
    tag: &'static str,
    faults: &mut Faults,
) {
    match kind(raw) {
        Kind::Null => *slot = None,
        Kind::Number => {
            let literal = text(raw);
            // `strconv.ParseInt(item, 10, 64)`, which refuses `25.0` and `1e2`
            // as firmly as it refuses a word.
            match literal
                .parse::<i64>()
                .ok()
                .and_then(|value| i32::try_from(value).ok())
            {
                Some(value) => *slot = Some(value),
                None => {
                    allocate(slot);
                    faults.save_number(prefix, tag, literal, "int");
                }
            }
        }
        found => {
            allocate(slot);
            faults.save(prefix, tag, found, "int");
        }
    }
}

/// `decode.go:476-478`: the pointer a value is about to be stored through is
/// allocated before the store is attempted, and a failed store leaves it.
fn allocate(slot: &mut Option<i32>) {
    if slot.is_none() {
        *slot = Some(0);
    }
}

/// One nested object, which Go merges into whatever the field already holds
/// rather than replacing (`decode.go:599-827` writes through a `subv` that
/// points into the struct).
fn store_object<T: GoObject>(
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

// ---------------------------------------------------------------------------
// The struct, field for field
// ---------------------------------------------------------------------------

/// Go's `apiConfig` (`config.go:17-59`).
///
/// Field order is Go's declaration order, which is the order `encoding/json`
/// marshals in and therefore the order of the file on disk. The names are Go's
/// tags. Both are a contract: the web UI reads this document, and so does the
/// Go server after a rollback.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ApiConfig {
    /// `config.go:18-23`.
    pub weather: WeatherConfig,
    /// `config.go:24-42`.
    pub knowledge: KnowledgeConfig,
    /// `config.go:43-46`. The tag is upper case where every other one is lower
    /// case, which is Go's inconsistency and not a typo here.
    #[serde(rename = "STT")]
    pub stt: SttConfig,
    /// `config.go:47-51`.
    pub server: ServerConfig,
    /// `config.go:52-56`.
    pub battery: BatteryConfig,
    /// Go's `HasReadFromEnv` (`config.go:57`). Set once, either by
    /// [`ApiConfig::from_env`] seeding a fresh file (`config.go:97`) or by the
    /// port-change rule in [`read_config`] (`config.go:137-142`).
    #[serde(rename = "hasreadfromenv")]
    pub has_read_from_env: bool,
    /// Go's `PastInitialSetup` (`config.go:58`), which the web UI reads to
    /// decide whether to show the first-run wizard.
    #[serde(rename = "pastinitialsetup")]
    pub past_initial_setup: bool,
    /// Top-level keys this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Go's anonymous weather struct (`config.go:18-23`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WeatherConfig {
    /// `config.go:19`.
    pub enable: bool,
    /// `config.go:20`.
    pub provider: String,
    /// `config.go:21`. An API key: never log this, and never put it in a test
    /// fixture.
    pub key: String,
    /// `config.go:22`.
    pub unit: String,
    /// Keys inside `weather` this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Go's anonymous knowledge struct (`config.go:24-42`).
///
/// The two `float32` fields are private because their invariant is a byte
/// layout rather than a value: whatever is in them is always the bytes
/// `encoding/json` would write for some `float32`. [`KnowledgeConfig::top_p`]
/// and [`KnowledgeConfig::temp`] read them back as numbers and
/// [`KnowledgeConfig::set_top_p`] and [`KnowledgeConfig::set_temp`] are the
/// only way to change them.
#[derive(Clone, Debug, Serialize)]
pub struct KnowledgeConfig {
    /// `config.go:25`.
    pub enable: bool,
    /// `config.go:26`.
    pub provider: String,
    /// `config.go:27`. The LLM API key: never log this, and never put it in a
    /// test fixture.
    pub key: String,
    /// `config.go:28`, the Houndify client ID, which only a `houndify`
    /// provider ever fills (`config.go:80-82`).
    pub id: String,
    /// `config.go:29`. [`read_config`] rewrites one value of this field
    /// (`config.go:144-147`).
    pub model: String,
    /// `config.go:30`.
    pub intentgraph: bool,
    /// `config.go:31`. The one camel-case tag in the file.
    #[serde(rename = "robotName")]
    pub robot_name: String,
    /// `config.go:32`. Free text an operator types, which is the field that
    /// makes [`GoFormatter`]'s HTML escaping load-bearing.
    pub openai_prompt: String,
    /// `config.go:33`.
    pub openai_voice: String,
    /// `config.go:34`.
    pub openai_voice_with_english: bool,
    /// `config.go:35`.
    pub save_chat: bool,
    /// `config.go:36`.
    pub commands_enable: bool,
    /// `config.go:37`.
    pub endpoint: String,
    /// Go's `TopP float32` (`config.go:38`).
    top_p: Box<RawValue>,
    /// Go's `Temperature float32` (`config.go:39`), tagged `temp`.
    temp: Box<RawValue>,
    /// `config.go:41`. Go's comment above it is the whole vocabulary:
    /// "for gpt-5*/o* reasoning models: none|low|medium|high|xhigh
    /// (`""` = medium)" (`config.go:40`).
    pub reasoning_effort: String,
    /// Keys inside `knowledge` this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for KnowledgeConfig {
    /// Go's zero value, which is every string empty, every bool false and both
    /// `float32`s zero. It is spelled out rather than derived only because
    /// `Box<RawValue>` has no [`Default`] of its own.
    fn default() -> Self {
        Self {
            enable: false,
            provider: String::new(),
            key: String::new(),
            id: String::new(),
            model: String::new(),
            intentgraph: false,
            robot_name: String::new(),
            openai_prompt: String::new(),
            openai_voice: String::new(),
            openai_voice_with_english: false,
            save_chat: false,
            commands_enable: false,
            endpoint: String::new(),
            top_p: zero_f32(),
            temp: zero_f32(),
            reasoning_effort: String::new(),
            extra: Extra::new(),
        }
    }
}

impl PartialEq for KnowledgeConfig {
    /// Two knowledge blocks are equal when they marshal to the same bytes.
    ///
    /// That is the only equality this file has: the struct exists to be
    /// written, and the two [`RawValue`] fields hold bytes rather than numbers,
    /// so a field-by-field comparison would have to compare them as bytes
    /// anyway. Going through the marshaller instead cannot go stale when a
    /// field is added.
    fn eq(&self, other: &Self) -> bool {
        go_marshal(self).ok() == go_marshal(other).ok()
    }
}

impl KnowledgeConfig {
    /// Go's `TopP` as a number (`config.go:38`).
    ///
    /// # Panics
    ///
    /// Never. Every path that fills the field renders it from an [`f32`]
    /// through [`crate::gofmt::go_json_f32`]: [`Default`], [`go_f32`] on the way in from a
    /// file, and [`KnowledgeConfig::set_top_p`]. The field is private so there
    /// is no fourth.
    pub fn top_p(&self) -> f32 {
        parse_rendered(&self.top_p)
    }

    /// Go's `Temperature` as a number (`config.go:39`).
    ///
    /// # Panics
    ///
    /// Never, for the reason [`KnowledgeConfig::top_p`] gives.
    pub fn temp(&self) -> f32 {
        parse_rendered(&self.temp)
    }

    /// Replaces `top_p`, rendering it as `encoding/json` would.
    ///
    /// # Errors
    ///
    /// [`GoJsonError`] for NaN and the infinities, which is what Go's
    /// marshaller answers for them rather than writing a value JSON cannot
    /// spell.
    pub fn set_top_p(&mut self, value: f32) -> Result<(), GoJsonError> {
        self.top_p = go_json_f32_raw(value)?;
        Ok(())
    }

    /// Replaces `temp`, rendering it as `encoding/json` would.
    ///
    /// # Errors
    ///
    /// As [`KnowledgeConfig::set_top_p`].
    pub fn set_temp(&mut self, value: f32) -> Result<(), GoJsonError> {
        self.temp = go_json_f32_raw(value)?;
        Ok(())
    }
}

/// Reads back a rendering [`crate::gofmt::go_json_f32`] produced.
fn parse_rendered(raw: &RawValue) -> f32 {
    raw.get()
        .parse()
        .expect("the field only ever holds a go_json_f32 rendering")
}

/// Go's anonymous STT struct (`config.go:43-46`).
///
/// Go's field is `Service` and its tag is `provider`; the tag wins here,
/// because the tag is what the file and the web UI use.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SttConfig {
    /// Go's `Service` (`config.go:44`). The one setting the shell still
    /// controls, through `STT_SERVICE` (`config.go:133-136`).
    pub provider: String,
    /// `config.go:45`, filled only for the two services that have models per
    /// language (`config.go:106-108`).
    pub language: String,
    /// Keys inside `STT` this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Go's anonymous server struct (`config.go:47-51`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ServerConfig {
    /// `config.go:49`, with Go's comment at `:48`: "false for ip, true for
    /// escape pod".
    pub epconfig: bool,
    /// `config.go:50`. A string, not a number, because that is how Go declares
    /// it and how it is compared against `DDL_RPC_PORT` (`config.go:138`).
    pub port: String,
    /// Keys inside `server` this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Go's anonymous battery struct (`config.go:52-56`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BatteryConfig {
    /// Go's `GoHomePercent *int` (`config.go:55`), with no `omitempty`, so an
    /// absent value is written as `null` rather than dropped.
    ///
    /// The pointer is the whole point of the field: Go's comment at
    /// `config.go:53-54` reads "battery percent at/below which a bot is sent to
    /// its charger" and "nil = default (25), 0 = disabled", so nil and zero
    /// mean different things and a plain number could not tell them apart.
    ///
    /// An [`i32`] where Go's `int` is 64 bits on every platform this runs on. A
    /// percentage does not need the range, and the narrowing is visible only
    /// for a value outside it: one already in the file is the type error Go
    /// gives a value outside `int64` instead, and one in
    /// `GOHOME_BATTERY_PERCENT` is ignored the way an unparseable one is
    /// (`config.go:87-91`), leaving the default.
    pub gohome_percent: Option<i32>,
    /// Keys inside `battery` this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
}

// ---------------------------------------------------------------------------
// Go's object loop, struct by struct
// ---------------------------------------------------------------------------

// Each pair below is one struct's half of `encoding/json`'s decoder: the tags
// it answers to, and what one value does to one field. The loop itself is
// `merge_object`, which is Go's and is shared.
//
// Every one of the six [`Deserialize`] impls is [`Decoded`] with the fault
// dropped, because a `Deserialize` impl can hand back a value or an error and
// Go's decoder produces both at once. [`ApiConfig::from_json_bytes`] is the
// entry point that keeps the fault, and it is the one the boot path uses.
//
// All six read each value as a [`RawValue`], which only `serde_json` can
// produce, so all six decode `apiConfig.json` and nothing else.

impl GoObject for ApiConfig {
    const TAGS: &'static [&'static str] = &[
        "weather",
        "knowledge",
        "STT",
        "server",
        "battery",
        "hasreadfromenv",
        "pastinitialsetup",
    ];
    const PREFIX: &'static str = "";
    const GO_TYPE: &'static str = "apiConfig";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        match tag {
            "weather" => store_object(&mut self.weather, raw, Self::PREFIX, tag, faults),
            "knowledge" => store_object(&mut self.knowledge, raw, Self::PREFIX, tag, faults),
            "STT" => store_object(&mut self.stt, raw, Self::PREFIX, tag, faults),
            "server" => store_object(&mut self.server, raw, Self::PREFIX, tag, faults),
            "battery" => store_object(&mut self.battery, raw, Self::PREFIX, tag, faults),
            "hasreadfromenv" => {
                store_bool(&mut self.has_read_from_env, raw, Self::PREFIX, tag, faults);
                Ok(())
            }
            "pastinitialsetup" => {
                store_bool(&mut self.past_initial_setup, raw, Self::PREFIX, tag, faults);
                Ok(())
            }
            // Unreachable: `resolve` only ever answers with a tag out of
            // `TAGS`. A tag it does not place here would behave as an unknown
            // key does, which the round-trip test would see.
            _ => Ok(()),
        }
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for ApiConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

impl GoObject for WeatherConfig {
    const TAGS: &'static [&'static str] = &["enable", "provider", "key", "unit"];
    const PREFIX: &'static str = "weather.";
    const GO_TYPE: &'static str = "struct";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        match tag {
            "enable" => store_bool(&mut self.enable, raw, Self::PREFIX, tag, faults),
            "provider" => store_string(&mut self.provider, raw, Self::PREFIX, tag, faults),
            "key" => store_string(&mut self.key, raw, Self::PREFIX, tag, faults),
            "unit" => store_string(&mut self.unit, raw, Self::PREFIX, tag, faults),
            _ => {}
        }
        Ok(())
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for WeatherConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

impl GoObject for KnowledgeConfig {
    const TAGS: &'static [&'static str] = &[
        "enable",
        "provider",
        "key",
        "id",
        "model",
        "intentgraph",
        "robotName",
        "openai_prompt",
        "openai_voice",
        "openai_voice_with_english",
        "save_chat",
        "commands_enable",
        "endpoint",
        "top_p",
        "temp",
        "reasoning_effort",
    ];
    const PREFIX: &'static str = "knowledge.";
    const GO_TYPE: &'static str = "struct";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        match tag {
            "enable" => store_bool(&mut self.enable, raw, Self::PREFIX, tag, faults),
            "provider" => store_string(&mut self.provider, raw, Self::PREFIX, tag, faults),
            "key" => store_string(&mut self.key, raw, Self::PREFIX, tag, faults),
            "id" => store_string(&mut self.id, raw, Self::PREFIX, tag, faults),
            "model" => store_string(&mut self.model, raw, Self::PREFIX, tag, faults),
            "intentgraph" => store_bool(&mut self.intentgraph, raw, Self::PREFIX, tag, faults),
            "robotName" => store_string(&mut self.robot_name, raw, Self::PREFIX, tag, faults),
            "openai_prompt" => {
                store_string(&mut self.openai_prompt, raw, Self::PREFIX, tag, faults);
            }
            "openai_voice" => store_string(&mut self.openai_voice, raw, Self::PREFIX, tag, faults),
            "openai_voice_with_english" => store_bool(
                &mut self.openai_voice_with_english,
                raw,
                Self::PREFIX,
                tag,
                faults,
            ),
            "save_chat" => store_bool(&mut self.save_chat, raw, Self::PREFIX, tag, faults),
            "commands_enable" => {
                store_bool(&mut self.commands_enable, raw, Self::PREFIX, tag, faults);
            }
            "endpoint" => store_string(&mut self.endpoint, raw, Self::PREFIX, tag, faults),
            "top_p" => store_f32(&mut self.top_p, raw, Self::PREFIX, tag, faults),
            "temp" => store_f32(&mut self.temp, raw, Self::PREFIX, tag, faults),
            "reasoning_effort" => {
                store_string(&mut self.reasoning_effort, raw, Self::PREFIX, tag, faults);
            }
            _ => {}
        }
        Ok(())
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for KnowledgeConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

impl GoObject for SttConfig {
    const TAGS: &'static [&'static str] = &["provider", "language"];
    const PREFIX: &'static str = "STT.";
    const GO_TYPE: &'static str = "struct";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        match tag {
            "provider" => store_string(&mut self.provider, raw, Self::PREFIX, tag, faults),
            "language" => store_string(&mut self.language, raw, Self::PREFIX, tag, faults),
            _ => {}
        }
        Ok(())
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for SttConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

impl GoObject for ServerConfig {
    const TAGS: &'static [&'static str] = &["epconfig", "port"];
    const PREFIX: &'static str = "server.";
    const GO_TYPE: &'static str = "struct";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        match tag {
            "epconfig" => store_bool(&mut self.epconfig, raw, Self::PREFIX, tag, faults),
            "port" => store_string(&mut self.port, raw, Self::PREFIX, tag, faults),
            _ => {}
        }
        Ok(())
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for ServerConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

impl GoObject for BatteryConfig {
    const TAGS: &'static [&'static str] = &["gohome_percent"];
    const PREFIX: &'static str = "battery.";
    const GO_TYPE: &'static str = "struct";

    fn store(
        &mut self,
        tag: &'static str,
        raw: &RawValue,
        faults: &mut Faults,
    ) -> serde_json::Result<()> {
        if tag == "gohome_percent" {
            store_gohome(&mut self.gohome_percent, raw, Self::PREFIX, tag, faults);
        }
        Ok(())
    }

    fn unknown(&mut self) -> &mut Extra {
        &mut self.extra
    }
}

impl<'de> Deserialize<'de> for BatteryConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Decoded::<Self>::deserialize(deserializer)?.value)
    }
}

// ---------------------------------------------------------------------------
// The environment
// ---------------------------------------------------------------------------

/// The twelve environment variables `config.go` reads, in the order it reads
/// them.
///
/// Go calls `os.Getenv` at the point of use, so the value it sees depends on
/// when the call runs. Reading them once into a value instead makes the whole
/// configuration cycle a function of its inputs, which is what lets the boot
/// side effects be tested without touching the process environment.
///
/// Every field is a [`String`] with the empty string as its absent form,
/// because that is exactly what `os.Getenv` answers for an unset variable: Go
/// cannot tell "unset" from "set to the empty string" here and neither can any
/// rule ported from it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Env {
    /// `config.go:69`. Compared against `"true"`; anything else disables
    /// weather.
    pub weatherapi_enabled: String,
    /// `config.go:71`.
    pub weatherapi_provider: String,
    /// `config.go:72`. A key: never log it.
    pub weatherapi_key: String,
    /// `config.go:73`.
    pub weatherapi_unit: String,
    /// `config.go:77`. Compared against `"true"`.
    pub knowledge_enabled: String,
    /// `config.go:79`, and again at `:80` where `houndify` alone makes
    /// `KNOWLEDGE_ID` meaningful.
    pub knowledge_provider: String,
    /// `config.go:81`.
    pub knowledge_id: String,
    /// `config.go:83`. A key: never log it.
    pub knowledge_key: String,
    /// `config.go:87`, parsed with `strconv.Atoi` at `:88`. An unparseable
    /// value is ignored rather than reported.
    pub gohome_battery_percent: String,
    /// Read four times in Go: once at `config.go:105` and twice on
    /// `config.go:106` in `WriteSTT`, and again at `config.go:134` in
    /// `ReadConfig`, which is the comparison that makes this the only setting
    /// the shell still controls.
    pub stt_service: String,
    /// `config.go:107`.
    pub stt_language: String,
    /// `config.go:138`, compared against the stored `server.port`.
    pub ddl_rpc_port: String,
}

/// The names [`Env`] carries, in declaration order.
///
/// Written out so that a test can assert the list against `config.go` rather
/// than against the struct that was built from it, and so that a boot path can
/// report what it read without naming each variable again.
pub const ENV_NAMES: [&str; 12] = [
    "WEATHERAPI_ENABLED",
    "WEATHERAPI_PROVIDER",
    "WEATHERAPI_KEY",
    "WEATHERAPI_UNIT",
    "KNOWLEDGE_ENABLED",
    "KNOWLEDGE_PROVIDER",
    "KNOWLEDGE_ID",
    "KNOWLEDGE_KEY",
    "GOHOME_BATTERY_PERCENT",
    "STT_SERVICE",
    "STT_LANGUAGE",
    "DDL_RPC_PORT",
];

impl Env {
    /// Reads all twelve from the process environment, which is what Go's
    /// `os.Getenv` calls do.
    ///
    /// A variable that is not valid Unicode reads as empty rather than as its
    /// bytes. Go would hand the raw bytes through; none of the twelve is ever
    /// anything but ASCII, and an empty string is the safer of the two answers
    /// because it is the one the rules below already understand.
    pub fn from_process() -> Self {
        let get = |name: &str| std::env::var(name).unwrap_or_default();
        Self {
            weatherapi_enabled: get(ENV_NAMES[0]),
            weatherapi_provider: get(ENV_NAMES[1]),
            weatherapi_key: get(ENV_NAMES[2]),
            weatherapi_unit: get(ENV_NAMES[3]),
            knowledge_enabled: get(ENV_NAMES[4]),
            knowledge_provider: get(ENV_NAMES[5]),
            knowledge_id: get(ENV_NAMES[6]),
            knowledge_key: get(ENV_NAMES[7]),
            gohome_battery_percent: get(ENV_NAMES[8]),
            stt_service: get(ENV_NAMES[9]),
            stt_language: get(ENV_NAMES[10]),
            ddl_rpc_port: get(ENV_NAMES[11]),
        }
    }
}

// ---------------------------------------------------------------------------
// The pure half of the three Go functions
// ---------------------------------------------------------------------------

impl ApiConfig {
    /// Go's `json.Marshal(APIConfig)` (`config.go:63`, `:98`, `:154`).
    ///
    /// Compact, in declaration order, with no trailing newline, which is
    /// exactly the bytes `os.WriteFile` puts in the file.
    ///
    /// # Panics
    ///
    /// Never. Serializing into a [`Vec`] cannot fail at the writer, every
    /// number in the struct is either an integer or a [`RawValue`] that was
    /// rendered from a finite [`f32`], and a [`Value`] in an [`Extra`] map
    /// cannot hold a NaN. Go discards its own marshal error the same way, at
    /// all three call sites.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        go_marshal(self).expect("an ApiConfig has no unserialisable value in it")
    }

    /// Go's `json.Unmarshal(configBytes, &APIConfig)` (`config.go:125`).
    ///
    /// # Errors
    ///
    /// Anything malformed, and anything whose type does not fit the field.
    /// The two are not the same failure. Go checks the whole document before it
    /// decodes anything (`decode.go:98-105`), so malformed bytes leave the
    /// configuration at its zero value; a type error is recorded and decoding
    /// carries on to the end of the document (`decode.go:243-247`), so every
    /// other field is filled. [`DecodeError`] carries both the fault and
    /// whatever decoded, because Go's caller reads the half-filled global
    /// regardless of the error and `vars.go:234` does so on the line after
    /// `ReadConfig` returns.
    // The error is a whole configuration plus a fault, which is what the rule
    // above asks of it, so it is one machine word or two larger than the `Ok`
    // variant beside it. Boxing it would move the same bytes to the heap and
    // make the shape harder to read for nothing.
    #[allow(clippy::result_large_err)]
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        match serde_json::from_slice::<Decoded<Self>>(bytes) {
            Ok(Decoded { value, fault: None }) => Ok(value),
            Ok(Decoded {
                value,
                fault: Some(fault),
            }) => Err(DecodeError {
                config: value,
                fault,
            }),
            Err(error) => Err(DecodeError {
                config: Self::default(),
                fault: DecodeFault::Malformed(error.to_string()),
            }),
        }
    }

    /// Go's `WriteSTT` (`config.go:102-109`).
    ///
    /// Go's own comment says what it is: "was not part of the original code, so
    /// this is its own function, launched if stt not found in config"
    /// (`config.go:103-104`). It sets the provider unconditionally and the
    /// language only for the two services that have per-language models, so a
    /// switch away from those two leaves the old language string in the file.
    pub fn write_stt(&mut self, env: &Env) {
        self.stt.provider = env.stt_service.clone();
        if LANGUAGE_AWARE_STT.contains(&env.stt_service.as_str()) {
            self.stt.language = env.stt_language.clone();
        }
    }

    /// Go's `CreateConfigFromEnv` without its write (`config.go:67-97`).
    ///
    /// Go seeds the package global, which at the only call site
    /// (`config.go:113`, reached when the file does not exist) is still the
    /// zero value, so starting from [`ApiConfig::default`] is the same thing.
    ///
    /// The order is Go's, and two parts of it are worth naming. The go-home
    /// percent is read from the environment and then forced to
    /// [`DEFAULT_GOHOME_PERCENT`] if that left it absent (`config.go:87-95`),
    /// so a fresh file never carries a `null` there. And `HasReadFromEnv` is
    /// set last (`config.go:97`), after [`ApiConfig::write_stt`], which is what
    /// makes the port-change rule in [`read_config`] a no-op on the next boot.
    pub fn from_env(env: &Env) -> Self {
        let mut config = Self::default();

        // `config.go:69-76`.
        if env.weatherapi_enabled == ENABLED {
            config.weather.enable = true;
            config.weather.provider = env.weatherapi_provider.clone();
            config.weather.key = env.weatherapi_key.clone();
            config.weather.unit = env.weatherapi_unit.clone();
        } else {
            config.weather.enable = false;
        }

        // `config.go:77-86`. The key is set after the provider test, so a
        // houndify provider fills both the ID and the key and any other
        // provider fills the key alone.
        if env.knowledge_enabled == ENABLED {
            config.knowledge.enable = true;
            config.knowledge.provider = env.knowledge_provider.clone();
            if env.knowledge_provider == HOUNDIFY {
                config.knowledge.id = env.knowledge_id.clone();
            }
            config.knowledge.key = env.knowledge_key.clone();
        } else {
            config.knowledge.enable = false;
        }

        // `config.go:87-91`. Go's `strconv.Atoi` failure is discarded, so a
        // value that is not a number leaves the field absent and the forcing
        // below turns it into the default.
        if !env.gohome_battery_percent.is_empty()
            && let Ok(percent) = env.gohome_battery_percent.parse::<i32>()
        {
            config.battery.gohome_percent = Some(percent);
        }

        // `config.go:92-95`.
        config.force_gohome_percent();

        // `config.go:96`.
        config.write_stt(env);

        // `config.go:97`.
        config.has_read_from_env = true;

        config
    }

    /// Go's two identical `GoHomePercent == nil` blocks (`config.go:92-95` and
    /// `:149-152`).
    ///
    /// It runs before the write in [`ApiConfig::from_env`] and before the one
    /// in [`read_config`], and before neither of the other two writes: Go's
    /// `WriteConfigToDisk` (`config.go:61-65`) does not force it, so a web UI
    /// save of a configuration whose percent is absent writes `null` and the
    /// next boot turns it into 25.
    fn force_gohome_percent(&mut self) {
        if self.battery.gohome_percent.is_none() {
            self.battery.gohome_percent = Some(DEFAULT_GOHOME_PERCENT);
        }
    }
}

// ---------------------------------------------------------------------------
// The three Go functions that touch the disk
// ---------------------------------------------------------------------------

/// Go's `WriteConfigToDisk` (`config.go:61-65`).
///
/// The save path the web UI and two of the LLM paths reach
/// (`config-ws/webserver.go:202`, `:217`, `:255`, `initwirepod/web.go:42`,
/// `:51`, `localization/download.go:85`, `ttr/kgsim.go:282`,
/// `ttr/kgsim_cmds.go:498`). It writes whatever it is given: unlike the two
/// boot writers it does not force the go-home percent first.
///
/// # Errors
///
/// Whatever the replacement reports. Go discards this error
/// (`config.go:64`); the port returns it so a full disk is visible.
pub async fn write_config_to_disk(config: &ApiConfig, dir: &DataDir) -> io::Result<()> {
    // `config.go:62`, through `logger.Println`, which is DEBUG with no
    // component (`logger.go:234-238`).
    tracing::debug!(comp = "", "Configuration changed, writing to disk");
    write_atomic(
        dir.api_config_path(),
        config.to_json_bytes(),
        CONFIG_FILE_MODE,
    )
    .await
}

/// Go's `CreateConfigFromEnv` (`config.go:67-100`).
///
/// The seeding and the write, in Go's order. The config is returned whether or
/// not the write succeeded, because Go's global is filled before the write and
/// stays filled when it fails.
pub async fn create_config_from_env(env: &Env, dir: &DataDir) -> (ApiConfig, io::Result<()>) {
    let config = ApiConfig::from_env(env);
    // `config.go:98-99`.
    let written = write_atomic(
        dir.api_config_path(),
        config.to_json_bytes(),
        CONFIG_FILE_MODE,
    )
    .await;
    (config, written)
}

/// What one [`read_config`] call did, beyond producing a configuration.
///
/// Go's `ReadConfig` returns nothing and reports through the log, and the two
/// failure arms are early returns (`config.go:123`, `:131`). Naming the four
/// arms here lets the boot path decide what a failure means without parsing a
/// log line, and lets a test assert which one ran.
#[derive(Debug)]
pub enum BootOutcome {
    /// No file was there, so one was seeded from the environment and written
    /// (`config.go:112-114`). The result is that write's.
    Created(io::Result<()>),
    /// The file was read, updated and rewritten (`config.go:115-156`). The
    /// result is the rewrite's.
    Read(io::Result<()>),
    /// The file existed and could not be read (`config.go:117-124`). Nothing
    /// was written.
    ReadFailed(io::Error),
    /// The file was read and was not the JSON this struct describes
    /// (`config.go:125-132`). Nothing was written, and whatever did decode is
    /// in [`BootConfig::config`] with knowledge and weather off, which is what
    /// Go leaves in its global.
    ParseFailed(DecodeFault),
}

/// The configuration [`read_config`] produced and what happened on the way.
#[derive(Debug)]
pub struct BootConfig {
    /// The configuration in memory, which on either failure arm is the zero
    /// value with knowledge and weather off.
    pub config: ApiConfig,
    /// Which of Go's four arms ran.
    pub outcome: BootOutcome,
}

/// Go's `ReadConfig` (`config.go:111-158`).
///
/// This is the boot path, and it is not a loader: on every successful read it
/// rewrites the file (`config.go:154-155`), whether or not anything changed.
/// Four things can change on the way past, in this order:
///
/// 1. The STT provider, whenever it differs from `STT_SERVICE`
///    (`config.go:133-136`). This runs at every boot, not only the first, and
///    it is the reason an operator who exports a different engine gets it.
/// 2. `HasReadFromEnv` and `PastInitialSetup`, together, when the former is
///    false and the stored port differs from `DDL_RPC_PORT`
///    (`config.go:137-142`).
/// 3. The Together model name, Llama-2 becoming Llama-3 (`config.go:144-147`).
/// 4. The go-home percent, absent becoming [`DEFAULT_GOHOME_PERCENT`]
///    (`config.go:149-152`).
///
/// Either failure arm disables knowledge and weather in memory and returns
/// without writing, so a file that cannot be read or parsed is left exactly as
/// it is for an operator to look at.
///
/// Go calls this once, from `vars.Init` (`vars.go:231`), which
/// `initwirepod/startserver.go:87` calls once. The package global is therefore
/// still the zero value when it runs, which is why every arm here starts from
/// [`ApiConfig::default`] rather than from a configuration passed in.
pub async fn read_config(env: &Env, dir: &DataDir) -> BootConfig {
    let path = dir.api_config_path();

    match load(path.clone()).await {
        // `config.go:112-114`.
        OnDisk::Missing => {
            let (config, written) = create_config_from_env(env, dir).await;
            tracing::debug!(comp = "", "API config JSON created");
            BootConfig {
                config,
                outcome: BootOutcome::Created(written),
            }
        }
        // `config.go:118-124`.
        OnDisk::Unreadable(error) => {
            tracing::debug!(comp = "", "Failed to read API config file");
            // `config.go:122`, Go's `logger.Println(err)`. The text is Rust's
            // rather than Go's `*fs.PathError`, and it names no file because
            // the underlying error does not carry one.
            tracing::debug!(comp = "", "{error}");
            BootConfig {
                config: disabled(),
                outcome: BootOutcome::ReadFailed(error),
            }
        }
        OnDisk::Bytes(bytes) => match ApiConfig::from_json_bytes(&bytes) {
            // `config.go:126-132`.
            Err(DecodeError { mut config, fault }) => {
                tracing::debug!(comp = "", "Failed to unmarshal API config JSON");
                // `config.go:130`. The text is this module's rather than Go's
                // `*json.UnmarshalTypeError`, but like Go's it quotes no string
                // and only ever a number literal, so no key can reach a log
                // line this way.
                tracing::debug!(comp = "", "{fault}");
                // `config.go:127-128`, over whatever decoded rather than over a
                // zero value: Go's global holds every field that decoded and
                // `vars.go:234` reads one of them.
                disable(&mut config);
                BootConfig {
                    config,
                    outcome: BootOutcome::ParseFailed(fault),
                }
            }
            Ok(mut config) => {
                // `config.go:133-136`, with Go's comment: "stt service is the
                // only thing controlled by shell".
                if config.stt.provider != env.stt_service {
                    config.write_stt(env);
                }

                // `config.go:137-142`. Both flags move together, and only from
                // a configuration that has never been seeded from the
                // environment.
                if !config.has_read_from_env && config.server.port != env.ddl_rpc_port {
                    config.has_read_from_env = true;
                    config.past_initial_setup = true;
                }

                // `config.go:144-147`.
                if config.knowledge.model == LLAMA2_MODEL {
                    tracing::debug!(comp = "", "Setting Together model to Llama3");
                    config.knowledge.model = LLAMA3_MODEL.to_owned();
                }

                // `config.go:149-152`.
                config.force_gohome_percent();

                // `config.go:154-155`.
                let written = write_atomic(path, config.to_json_bytes(), CONFIG_FILE_MODE).await;

                // `config.go:156`.
                tracing::debug!(comp = "", "API config successfully read");
                BootConfig {
                    config,
                    outcome: BootOutcome::Read(written),
                }
            }
        },
    }
}

/// What either failure arm does to the configuration in memory
/// (`config.go:119-120`, `:127-128`).
///
/// On the read-failure arm nothing has decoded and Go is assigning `false` into
/// a global that is already false, so the assignments have no effect there. On
/// the parse-failure arm they do: the global holds every field the document
/// filled before and after the fault, and either `enable` may be one of them.
fn disable(config: &mut ApiConfig) {
    config.knowledge.enable = false;
    config.weather.enable = false;
}

/// The zero configuration with both of those off, which is what the
/// read-failure arm leaves behind (`config.go:119-120`).
fn disabled() -> ApiConfig {
    let mut config = ApiConfig::default();
    disable(&mut config);
    config
}

/// What Go's two file steps found (`config.go:112`, `:117`).
///
/// They are two steps rather than one because Go distinguishes them: a failing
/// `os.Stat` seeds a fresh configuration, while a failing `os.ReadFile` on a
/// file that does stat is the error arm.
enum OnDisk {
    /// `os.Stat` failed, however it failed (`config.go:112`).
    Missing,
    /// The file's bytes.
    Bytes(Vec<u8>),
    /// `os.ReadFile` failed on a file that exists (`config.go:118`).
    Unreadable(io::Error),
}

/// Runs Go's stat-then-read inside `spawn_blocking`, for the same reason
/// [`write_atomic`] does: these are blocking file operations on a runtime whose
/// worker threads are also carrying gRPC streams.
async fn load(path: PathBuf) -> OnDisk {
    tokio::task::spawn_blocking(move || {
        if std::fs::metadata(&path).is_err() {
            return OnDisk::Missing;
        }
        match std::fs::read(&path) {
            Ok(bytes) => OnDisk::Bytes(bytes),
            Err(error) => OnDisk::Unreadable(error),
        }
    })
    .await
    .unwrap_or_else(|error| OnDisk::Unreadable(io::Error::other(error)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gofmt::go_json_f32;

    /// The names have to be the ones `config.go` passes to `os.Getenv` and
    /// nothing else, because a misspelling here is silent: the variable simply
    /// never has a value and every rule that reads it takes its absent branch.
    #[test]
    fn the_env_names_are_the_twelve_config_go_reads() {
        assert_eq!(
            ENV_NAMES,
            [
                "WEATHERAPI_ENABLED",
                "WEATHERAPI_PROVIDER",
                "WEATHERAPI_KEY",
                "WEATHERAPI_UNIT",
                "KNOWLEDGE_ENABLED",
                "KNOWLEDGE_PROVIDER",
                "KNOWLEDGE_ID",
                "KNOWLEDGE_KEY",
                "GOHOME_BATTERY_PERCENT",
                "STT_SERVICE",
                "STT_LANGUAGE",
                "DDL_RPC_PORT",
            ],
            "the environment variables config.go reads moved"
        );
    }

    /// Go breaks a tie between two fields that fold together by taking the
    /// first in name order (`encode.go:1301-1304`), and [`resolve`] takes the
    /// first in declaration order instead. The two agree only while no two tags
    /// in one object fold together, which is what this checks.
    #[test]
    fn no_two_tags_fold_together() {
        for tags in [
            ApiConfig::TAGS,
            WeatherConfig::TAGS,
            KnowledgeConfig::TAGS,
            SttConfig::TAGS,
            ServerConfig::TAGS,
            BatteryConfig::TAGS,
        ] {
            let mut folded: Vec<String> = tags.iter().map(|tag| fold_name(tag)).collect();
            folded.sort();
            let count = folded.len();
            folded.dedup();
            assert_eq!(
                folded.len(),
                count,
                "two tags in {tags:?} fold to the same name, so Go's tie-break is observable"
            );
        }
    }

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

    /// The lookup Go does per key: the exact tag first, and the folded one only
    /// when there is no exact match (`decode.go:694-697`).
    #[test]
    fn a_key_finds_its_field_exactly_or_by_fold() {
        assert_eq!(resolve(ApiConfig::TAGS, "weather"), Some("weather"));
        assert_eq!(resolve(ApiConfig::TAGS, "WEATHER"), Some("weather"));
        assert_eq!(resolve(ApiConfig::TAGS, "stt"), Some("STT"));
        assert_eq!(resolve(ApiConfig::TAGS, "STT"), Some("STT"));
        assert_eq!(resolve(ApiConfig::TAGS, "\u{017f}tt"), Some("STT"));
        assert_eq!(resolve(ApiConfig::TAGS, "fork_only"), None);
        assert_eq!(resolve(ApiConfig::TAGS, ""), None);
        assert_eq!(resolve(KnowledgeConfig::TAGS, "TOP_P"), Some("top_p"));
    }

    /// [`parse_rendered`]'s `expect` rests on the field never holding anything
    /// but a [`go_json_f32`] rendering, and on every such rendering parsing
    /// back to the value it came from. The second half is what this checks, at
    /// the two cutoffs where the rendering is not a plain decimal.
    #[test]
    fn every_rendering_parses_back_to_the_float_it_came_from() {
        for value in [0.0f32, -0.0, 0.7, 1.0, 1e20, 1e21, 1e-7, f32::MAX] {
            let mut knowledge = KnowledgeConfig::default();
            knowledge
                .set_top_p(value)
                .expect("a finite float32 renders");

            assert_eq!(
                knowledge.top_p().to_bits(),
                value.to_bits(),
                "{} did not survive the round trip through {}",
                value,
                go_json_f32(value).expect("a finite float32 renders")
            );
        }
    }
}
