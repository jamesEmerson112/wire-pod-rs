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
//! **Forward compatibility.** Go's decoder drops any key its struct does not
//! name, so a fork-only or newer-version key is lost the moment the Go server
//! rewrites the file. Every object here carries a `#[serde(flatten)]` map
//! instead, so an unknown key survives a read-modify-write. That is what makes
//! rolling back to the Go server safe in one direction and forward in the
//! other, and it is a deliberate difference from Go rather than a shortfall.
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
use std::io;
use std::path::PathBuf;

use serde::de::Error as _;
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
/// `<`, `>` and `&`, "because they can lead to security holes when
/// user-controlled strings are rendered into JSON and served to some browsers"
/// (`encoding/json/encode.go:1000-1004`), and U+2028 and U+2029
/// unconditionally, because they are line terminators in JavaScript
/// (`encode.go:1028-1042`). `serde_json` escapes none of the five.
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
/// (`encode.go:988-1008`), and `serde_json`'s `ESCAPE` table and
/// `write_char_escape` name exactly the same set. Neither escapes `/`, and
/// neither escapes non-ASCII. Go's one remaining case, invalid UTF-8 becoming
/// `\ufffd` (`encode.go:1021-1027`), cannot arise: a Rust [`String`] is UTF-8
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

// ---------------------------------------------------------------------------
// Decoding tolerances Go has and serde does not
// ---------------------------------------------------------------------------

/// Reads a field that may be JSON `null`, leaving it at its zero value when it
/// is.
///
/// Go's decoder writes nothing into a non-pointer field for a `null`: the value
/// keeps whatever it held, which at boot is the zero value, and no error is
/// reported. `serde` instead refuses a `null` wherever the field is not an
/// [`Option`], so without this a single `"battery":null` in a hand-edited file
/// would fail the parse and take knowledge and weather down with it
/// (`config.go:126-132`) where Go boots normally.
///
/// Applied to every field that is not already an [`Option`]. The one that is,
/// `gohome_percent`, is a Go pointer, where `null` is meaningful rather than
/// merely tolerated.
fn null_is_zero<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Reads a Go `float32` field and re-renders it as the bytes `encoding/json`
/// would write for it.
///
/// The rendering happens on the way in rather than on the way out so that the
/// [`RawValue`] in the struct is always Go's spelling of some `float32`,
/// whatever spelling the file used. That is Go's behaviour too: it parses into
/// a `float32` and marshals from it, so a hand-edited `0.70` comes back as
/// `0.7` and a `1.0` comes back as `1`.
///
/// It also keeps the field usable from a flattened struct. A [`RawValue`] can
/// only be deserialized by a deserializer that knows its private newtype, and
/// `serde`'s flatten support buffers every field into an intermediate value
/// first, so a bare `Box<RawValue>` field beside a `#[serde(flatten)]` map
/// cannot be read at all. Going through an [`f32`] side-steps that entirely.
fn go_f32<'de, D>(deserializer: D) -> Result<Box<RawValue>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<f32>::deserialize(deserializer)?.unwrap_or_default();
    // Only NaN and the infinities fail, and JSON can spell none of them. What
    // can reach here is a literal so large it parses as an infinity, which Go
    // rejects too, with `cannot unmarshal number ... into Go value of type
    // float32`.
    go_json_f32_raw(value).map_err(D::Error::custom)
}

/// The rendering of a zero `float32`, which is the Go zero value of `top_p` and
/// `temp` and therefore what a missing one falls back to.
fn zero_f32() -> Box<RawValue> {
    go_json_f32_raw(0.0).expect("zero is a finite float32")
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
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct ApiConfig {
    /// `config.go:18-23`.
    #[serde(deserialize_with = "null_is_zero")]
    pub weather: WeatherConfig,
    /// `config.go:24-42`.
    #[serde(deserialize_with = "null_is_zero")]
    pub knowledge: KnowledgeConfig,
    /// `config.go:43-46`. The tag is upper case where every other one is lower
    /// case, which is Go's inconsistency and not a typo here.
    #[serde(rename = "STT", deserialize_with = "null_is_zero")]
    pub stt: SttConfig,
    /// `config.go:47-51`.
    #[serde(deserialize_with = "null_is_zero")]
    pub server: ServerConfig,
    /// `config.go:52-56`.
    #[serde(deserialize_with = "null_is_zero")]
    pub battery: BatteryConfig,
    /// Go's `HasReadFromEnv` (`config.go:57`). Set once, either by
    /// [`ApiConfig::from_env`] seeding a fresh file (`config.go:97`) or by the
    /// port-change rule in [`read_config`] (`config.go:137-142`).
    #[serde(rename = "hasreadfromenv", deserialize_with = "null_is_zero")]
    pub has_read_from_env: bool,
    /// Go's `PastInitialSetup` (`config.go:58`), which the web UI reads to
    /// decide whether to show the first-run wizard.
    #[serde(rename = "pastinitialsetup", deserialize_with = "null_is_zero")]
    pub past_initial_setup: bool,
    /// Top-level keys this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Go's anonymous weather struct (`config.go:18-23`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct WeatherConfig {
    /// `config.go:19`.
    #[serde(deserialize_with = "null_is_zero")]
    pub enable: bool,
    /// `config.go:20`.
    #[serde(deserialize_with = "null_is_zero")]
    pub provider: String,
    /// `config.go:21`. An API key: never log this, and never put it in a test
    /// fixture.
    #[serde(deserialize_with = "null_is_zero")]
    pub key: String,
    /// `config.go:22`.
    #[serde(deserialize_with = "null_is_zero")]
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
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct KnowledgeConfig {
    /// `config.go:25`.
    #[serde(deserialize_with = "null_is_zero")]
    pub enable: bool,
    /// `config.go:26`.
    #[serde(deserialize_with = "null_is_zero")]
    pub provider: String,
    /// `config.go:27`. The LLM API key: never log this, and never put it in a
    /// test fixture.
    #[serde(deserialize_with = "null_is_zero")]
    pub key: String,
    /// `config.go:28`, the Houndify client ID, which only a `houndify`
    /// provider ever fills (`config.go:80-82`).
    #[serde(deserialize_with = "null_is_zero")]
    pub id: String,
    /// `config.go:29`. [`read_config`] rewrites one value of this field
    /// (`config.go:144-147`).
    #[serde(deserialize_with = "null_is_zero")]
    pub model: String,
    /// `config.go:30`.
    #[serde(deserialize_with = "null_is_zero")]
    pub intentgraph: bool,
    /// `config.go:31`. The one camel-case tag in the file.
    #[serde(rename = "robotName", deserialize_with = "null_is_zero")]
    pub robot_name: String,
    /// `config.go:32`. Free text an operator types, which is the field that
    /// makes [`GoFormatter`]'s HTML escaping load-bearing.
    #[serde(deserialize_with = "null_is_zero")]
    pub openai_prompt: String,
    /// `config.go:33`.
    #[serde(deserialize_with = "null_is_zero")]
    pub openai_voice: String,
    /// `config.go:34`.
    #[serde(deserialize_with = "null_is_zero")]
    pub openai_voice_with_english: bool,
    /// `config.go:35`.
    #[serde(deserialize_with = "null_is_zero")]
    pub save_chat: bool,
    /// `config.go:36`.
    #[serde(deserialize_with = "null_is_zero")]
    pub commands_enable: bool,
    /// `config.go:37`.
    #[serde(deserialize_with = "null_is_zero")]
    pub endpoint: String,
    /// Go's `TopP float32` (`config.go:38`).
    #[serde(deserialize_with = "go_f32")]
    top_p: Box<RawValue>,
    /// Go's `Temperature float32` (`config.go:39`), tagged `temp`.
    #[serde(deserialize_with = "go_f32")]
    temp: Box<RawValue>,
    /// `config.go:41`. Go's comment above it is the whole vocabulary:
    /// "for gpt-5*/o* reasoning models: none|low|medium|high|xhigh
    /// (`""` = medium)" (`config.go:40`).
    #[serde(deserialize_with = "null_is_zero")]
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
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct SttConfig {
    /// Go's `Service` (`config.go:44`). The one setting the shell still
    /// controls, through `STT_SERVICE` (`config.go:133-136`).
    #[serde(deserialize_with = "null_is_zero")]
    pub provider: String,
    /// `config.go:45`, filled only for the two services that have models per
    /// language (`config.go:106-108`).
    #[serde(deserialize_with = "null_is_zero")]
    pub language: String,
    /// Keys inside `STT` this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Go's anonymous server struct (`config.go:47-51`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    /// `config.go:49`, with Go's comment at `:48`: "false for ip, true for
    /// escape pod".
    #[serde(deserialize_with = "null_is_zero")]
    pub epconfig: bool,
    /// `config.go:50`. A string, not a number, because that is how Go declares
    /// it and how it is compared against `DDL_RPC_PORT` (`config.go:138`).
    #[serde(deserialize_with = "null_is_zero")]
    pub port: String,
    /// Keys inside `server` this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
}

/// Go's anonymous battery struct (`config.go:52-56`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
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
    /// for a value outside it: one already in the file fails the parse, and one
    /// in `GOHOME_BATTERY_PERCENT` is ignored the way an unparseable one is
    /// (`config.go:87-91`), leaving the default.
    pub gohome_percent: Option<i32>,
    /// Keys inside `battery` this struct does not name.
    #[serde(flatten)]
    pub extra: Extra,
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
    /// `config.go:105` and `:106` in `WriteSTT`, and `config.go:134` in
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
    /// Where Go keeps the fields that decoded before the first type error and
    /// carries on, this keeps nothing; both then take the same failure path
    /// (`config.go:126-132`), which writes nothing and disables knowledge and
    /// weather, so the difference is confined to that boot's memory.
    pub fn from_json_bytes(bytes: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(bytes)
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
    /// (`config.go:125-132`). Nothing was written.
    ParseFailed(serde_json::Error),
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
            Err(error) => {
                tracing::debug!(comp = "", "Failed to unmarshal API config JSON");
                // `config.go:130`. Unlike Go's `UnmarshalTypeError`, a serde
                // message can quote the offending scalar. The only scalars that
                // can be quoted are ones whose declared type is not a string,
                // so neither `key` can reach a log line this way.
                tracing::debug!(comp = "", "{error}");
                BootConfig {
                    config: disabled(),
                    outcome: BootOutcome::ParseFailed(error),
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

/// The in-memory result of either failure arm (`config.go:119-120`,
/// `:127-128`).
///
/// Go assigns `false` into a global that is already false at boot, so the
/// assignments have no effect there either; they are written out because they
/// are the whole of what Go does before returning, and because a later commit
/// that gives `ReadConfig` a non-zero starting point would need them.
fn disabled() -> ApiConfig {
    let mut config = ApiConfig::default();
    config.knowledge.enable = false;
    config.weather.enable = false;
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
