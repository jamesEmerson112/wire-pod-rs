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
//! [`crate::gojson::GoFormatter`], which reproduces `encoding/json`'s HTML
//! escaping.
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
//! That loop is not this file's alone, and it does not live here.
//! [`crate::gojson`] holds Go's marshaller and Go's object loop, which every
//! state file the port reads and writes goes through; this module holds
//! `apiConfig.json`'s structs, their tag tables and the two field writers only
//! its fields need, `store_f32` and `store_gohome`. This file's root is an
//! object, so [`crate::gojson::Decoded`] serves it, and
//! [`crate::store::jdocs`]'s is an array, so that module brings a root of its
//! own.
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
//! [`WriteGate`] is the file's location, both injected. Wiring this into the
//! binary is the boot commit's job, not this module's.
//!
//! **One gate.** The three writers below take a [`WriteGate`] rather than a
//! [`DataDir`] because they are three writers of one file. Go's three overlap
//! only in that they all reach the same global; here the web UI's save runs on
//! an axum handler while the boot rewrite runs as a task, and two unordered
//! writes can land their renames in the opposite order from their marshals.
//! The gate serialises them and moves each marshal inside its own turn, which
//! it can only do while all three hold the same gate, so [`config_gate`] builds
//! it once and C13 keeps it in `AppState`.

use std::fmt;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use crate::gofmt::{GoJsonError, go_json_f32_raw};
use crate::gojson::{
    DecodeFault, Decoded, Extra, Faults, GoObject, Kind, go_marshal, kind, store_bool,
    store_object, store_string, text,
};
use crate::paths::DataDir;
use crate::persist::WriteGate;

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

/// The rendering of a zero `float32`, which is the Go zero value of `top_p` and
/// `temp` and therefore what a missing one falls back to.
fn zero_f32() -> Box<RawValue> {
    go_json_f32_raw(0.0).expect("zero is a finite float32")
}

// ---------------------------------------------------------------------------
// Decoding the way `encoding/json` decodes
// ---------------------------------------------------------------------------

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

// The two field writers this file brings of its own. Each is one arm of Go's
// `literalStore`, like the four in [`crate::gojson`], and each leaves the field
// alone for a `null` the way Go does (`decode.go:899-903`). They are here
// rather than there because each is tied to a shape only `apiConfig.json` has:
// a `float32` carried as the bytes `encoding/json` would write for it, and the
// one `*int`.

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
    /// makes [`crate::gojson::GoFormatter`]'s HTML escaping load-bearing.
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
// it answers to, and what one value does to one field. The loop itself, the
// exact-then-folded key match, the duplicate rule and the fault recording are
// [`crate::gojson`]'s and are Go's.
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
    /// rendered from a finite [`f32`], and a [`serde_json::Value`] in an
    /// [`Extra`] map cannot hold a NaN. Go discards its own marshal error the
    /// same way, at all three call sites.
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

/// The gate `apiConfig.json`'s three writers share.
///
/// Go has three of them, all reaching one file: `WriteConfigToDisk` from the
/// web UI and the LLM paths (`config.go:64`), `CreateConfigFromEnv` from the
/// first boot (`config.go:99`) and `ReadConfig`'s rewrite from every boot after
/// it (`config.go:155`). Two of them can overlap in this port, because the web
/// UI's save is an axum handler and the boot rewrite is a task, and two
/// unordered writes of one file can land their renames in the opposite order
/// from their marshals and leave the file holding a configuration the server
/// has already moved past. [`WriteGate`] is what stops that, and it only stops
/// it while all three writers hold *one* gate: two gates over the same path
/// order nothing. This builds the one, C13 keeps it in `AppState`, and C22
/// boots through it.
pub fn config_gate(dir: &DataDir) -> WriteGate {
    WriteGate::new(
        dir.api_config_path().to_string_lossy().into_owned(),
        CONFIG_FILE_MODE,
    )
}

/// Go's `WriteConfigToDisk` (`config.go:61-65`).
///
/// The save path the web UI and two of the LLM paths reach
/// (`config-ws/webserver.go:202`, `:217`, `:255`, `initwirepod/web.go:42`,
/// `:51`, `localization/download.go:85`, `ttr/kgsim.go:282`,
/// `ttr/kgsim_cmds.go:498`). It writes whatever it is given: unlike the two
/// boot writers it does not force the go-home percent first.
///
/// The file is named by the gate rather than by a directory, so that this
/// writer and the two below cannot be handed two different gates over one file.
/// The marshal runs inside the gate's turn, which is what makes the bytes
/// reaching the disk no older than the moment this write took its turn.
///
/// # Errors
///
/// Whatever the replacement reports. Go discards this error
/// (`config.go:64`); the port returns it so a full disk is visible.
pub async fn write_config_to_disk(config: &ApiConfig, gate: &WriteGate) -> io::Result<()> {
    // `config.go:62`, through `logger.Println`, which is DEBUG with no
    // component (`logger.go:234-238`).
    tracing::debug!(comp = "", "Configuration changed, writing to disk");
    gate.write(|| config.to_json_bytes()).await
}

/// Go's `CreateConfigFromEnv` (`config.go:67-100`).
///
/// The seeding and the write, in Go's order. The config is returned whether or
/// not the write succeeded, because Go's global is filled before the write and
/// stays filled when it fails.
pub async fn create_config_from_env(env: &Env, gate: &WriteGate) -> (ApiConfig, io::Result<()>) {
    let config = ApiConfig::from_env(env);
    // `config.go:98-99`.
    let written = gate.write(|| config.to_json_bytes()).await;
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
///
/// The file is named by the gate rather than by a directory, for the reason
/// [`config_gate`] gives: the boot rewrite and the web UI's save are two
/// writers of one file, and they are only ordered while they share one gate.
/// Both the read and the rewrite go through it, so `gate.path()` is the only
/// spelling of the file this function knows.
pub async fn read_config(env: &Env, gate: &WriteGate) -> BootConfig {
    let path = PathBuf::from(gate.path());

    match load(path).await {
        // `config.go:112-114`.
        OnDisk::Missing => {
            let (config, written) = create_config_from_env(env, gate).await;
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
                let written = gate.write(|| config.to_json_bytes()).await;

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
/// [`crate::persist::write_atomic`] does: these are blocking file operations on
/// a runtime whose worker threads are also carrying gRPC streams.
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
    use crate::gojson::{fold_name, resolve};

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
