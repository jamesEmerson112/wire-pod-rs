//! `apiConfig.json`: the byte layout, the decoding tolerances, and the four
//! things a boot changes on its way past.
//!
//! The load-bearing case is [`the_live_file_round_trips_byte_for_byte`], which
//! reads the committed copy of this machine's own `apiConfig.json` and asserts
//! that parsing and re-marshalling it reproduces all 743 bytes. Everything
//! else exists to say which part broke when that one fails.
//!
//! The fixture is the live file with exactly two values replaced by
//! same-byte-length ASCII placeholders: `knowledge.key`, which is an API key,
//! and `knowledge.openai_prompt`, which is the operator's own text. Both were
//! replaced by a script that never printed either value and asserted the byte
//! length did not move, so the fixture is the real document's shape, order and
//! length with none of its secrets.
//!
//! Every `float32` rendering asserted here is read out of
//! `docs/phases/P1-robot-connect-auth/go-probe/expected.txt`, which is the Go
//! probe's own stdout, rather than written out by hand. The probe's `f32json`
//! section marshals a struct with one `float32` field, which is the shape
//! `pkg/vars/config.go:38-39` has.
//!
//! Everything that touches the disk runs in a directory under the system
//! temporary directory, named for the process, so nothing here can reach the
//! repository or the live `%APPDATA%\wire-pod` the Go server is serving from.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing_subscriber::layer::SubscriberExt;
use wirepod_core::config::{
    ApiConfig, BatteryConfig, BootOutcome, CONFIG_FILE_MODE, DEFAULT_GOHOME_PERCENT, Env,
    LLAMA2_MODEL, LLAMA3_MODEL, create_config_from_env, go_marshal, read_config,
    write_config_to_disk,
};
use wirepod_core::logger::{LogLayer, LogRing, ManualLogClock};
use wirepod_core::paths::DataDir;

/// The committed copy of this machine's `apiConfig.json`, redacted.
const FIXTURE: &str =
    include_str!("../../../docs/phases/P1-robot-connect-auth/fixtures/apiConfig.json");

/// The Go probe recording the `float32` renderings come from.
const EXPECTED: &str =
    include_str!("../../../docs/phases/P1-robot-connect-auth/go-probe/expected.txt");

/// What the live file measured when it was copied. A second statement of the
/// number, so that a fixture regenerated from a changed live file fails here
/// rather than quietly moving what "byte for byte" means.
const FIXTURE_LEN: usize = 743;

/// A ceiling on anything awaited, generous enough that only a hang reaches it.
/// Real durations, because this crate's tests never pause the runtime's clock.
const CEILING: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Reading the probe recording
// ---------------------------------------------------------------------------

/// The `f32json` rendering of the `float32` whose bits are `bits`, as the bare
/// number rather than as the whole document.
///
/// The recording's own README says the hex bit pattern is the authoritative
/// input and the `expr=` beside it is documentation, so this keys on the bits.
fn recorded_f32(bits: u32) -> String {
    let wanted = format!("kind=number v=0x{bits:08x} ");
    for (index, raw) in EXPECTED.lines().enumerate() {
        let text = raw.strip_suffix('\r').unwrap_or(raw);
        if text.starts_with('#') {
            continue;
        }
        let mut columns = text.split('\t');
        let section = columns.next().expect("a line has a section column");
        if section != "f32json" {
            continue;
        }
        let input = columns.next().expect("a line has an input column");
        if !input.starts_with(&wanted) {
            continue;
        }
        let output = columns.next().expect("a line has an output column");
        return output
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or_else(|| panic!("line {}: the output is not a quoted literal", index + 1))
            .to_owned();
    }
    panic!("the recording has no f32json number case for 0x{bits:08x}");
}

// ---------------------------------------------------------------------------
// A directory to work in
// ---------------------------------------------------------------------------

/// A directory under the system temporary directory, removed when the test
/// ends.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is before the epoch")
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wirepod-config-{label}-{}-{nanos:x}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("could not create the temporary directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// The data directory a boot would be pointed at.
    fn data_dir(&self) -> DataDir {
        DataDir::rooted(&self.path)
    }

    /// The bytes currently in `apiConfig.json`.
    fn file(&self) -> Vec<u8> {
        fs::read(self.path.join("apiConfig.json")).expect("apiConfig.json is missing")
    }

    /// Puts `bytes` in `apiConfig.json`, replacing anything there.
    fn seed(&self, bytes: &[u8]) {
        fs::write(self.path.join("apiConfig.json"), bytes).expect("could not seed apiConfig.json");
    }

    /// The names in the directory, sorted, so a leftover temporary is visible.
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.path)
            .expect("could not list the directory")
            .map(|entry| {
                entry
                    .expect("could not read a directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// An environment with nothing set, which is what Go sees when the shell
/// exports none of the twelve.
fn empty_env() -> Env {
    Env::default()
}

/// The instant every ring below stamps its entries with.
///
/// Anything but zero: Go's `GetEntries` keeps an entry only when its `TimeMS`
/// is strictly greater than the `since` it was asked for (`logger.go:227`), so
/// a ring at the epoch would answer nothing to the `since` of 0 that
/// [`recorded_lines`] asks with.
const STAMPED_AT: i64 = 1_767_000_000_000;

/// Every log line one body recorded, as `(level, comp, msg)`.
fn recorded_lines(ring: &LogRing) -> Vec<(String, String, String)> {
    ring.get_entries(wirepod_core::logger::LogLevel::Debug, 0)
        .into_iter()
        .map(|entry| (entry.level, entry.comp, entry.msg))
        .collect()
}

/// A ring wired to a subscriber, for a body that logs.
fn ring() -> Arc<LogRing> {
    Arc::new(LogRing::new(Arc::new(ManualLogClock::new(
        STAMPED_AT,
        "2026.01.02 03:04:05",
    ))))
}

// ---------------------------------------------------------------------------
// The byte round trip
// ---------------------------------------------------------------------------

/// The one case the rest of this file exists to explain. A real
/// `apiConfig.json`, written by the Go server on this machine, parses and
/// marshals back to exactly the bytes it arrived as.
#[test]
fn the_live_file_round_trips_byte_for_byte() {
    assert_eq!(
        FIXTURE.len(),
        FIXTURE_LEN,
        "the fixture is no longer the {FIXTURE_LEN} bytes the live file measured"
    );
    assert!(
        !FIXTURE.ends_with('\n'),
        "the fixture grew a trailing newline; os.WriteFile writes json.Marshal's bytes and nothing else (config.go:154-155)"
    );
    assert!(
        !FIXTURE.contains('\r'),
        "the fixture grew a carriage return"
    );

    let config =
        ApiConfig::from_json_bytes(FIXTURE.as_bytes()).expect("the live file did not parse");

    let rewritten = config.to_json_bytes();

    assert_eq!(
        String::from_utf8(rewritten).expect("the rewrite is not UTF-8"),
        FIXTURE,
        "the rewrite of a real apiConfig.json is not the bytes it came from"
    );
}

/// The fixture is the shape it claims to be, so that a regenerated one that
/// quietly lost a field cannot pass the round trip by comparing two equally
/// wrong documents.
#[test]
fn the_fixture_is_a_whole_config_with_both_secrets_redacted() {
    let config = ApiConfig::from_json_bytes(FIXTURE.as_bytes()).expect("the fixture did not parse");

    assert!(
        config.knowledge.key.starts_with("REDACTED"),
        "the fixture's knowledge key is not a placeholder"
    );
    assert!(
        config.knowledge.openai_prompt.starts_with("REDACTED"),
        "the fixture's prompt is not a placeholder"
    );
    assert_eq!(
        config.knowledge.key.len(),
        164,
        "the key placeholder is not the length the live value was"
    );
    assert_eq!(
        config.knowledge.openai_prompt.len(),
        67,
        "the prompt placeholder is not the length the live value was"
    );
    assert!(
        config.extra.is_empty() && config.knowledge.extra.is_empty(),
        "the live file carried a key this struct does not name"
    );
    assert_eq!(
        config.battery.gohome_percent,
        Some(DEFAULT_GOHOME_PERCENT),
        "the live file's go-home percent moved"
    );
}

// ---------------------------------------------------------------------------
// Tags, order and the two float fields
// ---------------------------------------------------------------------------

/// Every tag and the whole field order, against one literal document.
///
/// The two `float32` renderings are spliced in from the probe recording rather
/// than written here, so the only hand-written bytes are the ones Go's struct
/// tags decide.
#[test]
fn every_tag_and_the_field_order_match_gos_struct() {
    let mut config = ApiConfig::default();

    config.weather.enable = true;
    config.weather.provider = "openweathermap".to_owned();
    config.weather.key = "not-a-real-key".to_owned();
    config.weather.unit = "F".to_owned();

    config.knowledge.enable = true;
    config.knowledge.provider = "openai".to_owned();
    config.knowledge.key = "not-a-real-key-either".to_owned();
    config.knowledge.id = "houndify-client-id".to_owned();
    config.knowledge.model = "gpt-5".to_owned();
    config.knowledge.intentgraph = true;
    config.knowledge.robot_name = "Vector".to_owned();
    config.knowledge.openai_prompt = "be brief".to_owned();
    config.knowledge.openai_voice = "shimmer".to_owned();
    config.knowledge.openai_voice_with_english = true;
    config.knowledge.save_chat = true;
    config.knowledge.commands_enable = true;
    config.knowledge.endpoint = "https://example.invalid/v1".to_owned();
    config.knowledge.set_top_p(0.7).expect("0.7 renders");
    config.knowledge.set_temp(1.0).expect("1 renders");
    config.knowledge.reasoning_effort = "high".to_owned();

    config.stt.provider = "vosk".to_owned();
    config.stt.language = "en-US".to_owned();

    config.server.epconfig = true;
    config.server.port = "443".to_owned();

    config.battery.gohome_percent = Some(DEFAULT_GOHOME_PERCENT);

    config.has_read_from_env = true;
    config.past_initial_setup = true;

    let top_p = recorded_f32(0.7f32.to_bits());
    let temp = recorded_f32(1.0f32.to_bits());
    let want = format!(
        concat!(
            r#"{{"weather":{{"enable":true,"provider":"openweathermap","key":"not-a-real-key","unit":"F"}},"#,
            r#""knowledge":{{"enable":true,"provider":"openai","key":"not-a-real-key-either","id":"houndify-client-id","#,
            r#""model":"gpt-5","intentgraph":true,"robotName":"Vector","openai_prompt":"be brief","#,
            r#""openai_voice":"shimmer","openai_voice_with_english":true,"save_chat":true,"commands_enable":true,"#,
            r#""endpoint":"https://example.invalid/v1","top_p":{},"temp":{},"reasoning_effort":"high"}},"#,
            r#""STT":{{"provider":"vosk","language":"en-US"}},"#,
            r#""server":{{"epconfig":true,"port":"443"}},"#,
            r#""battery":{{"gohome_percent":25}},"#,
            r#""hasreadfromenv":true,"pastinitialsetup":true}}"#,
        ),
        top_p, temp
    );

    assert_eq!(
        String::from_utf8(config.to_json_bytes()).expect("the document is not UTF-8"),
        want,
        "a tag, a field position or a nesting level moved"
    );
}

/// The two `float32` fields render as Go renders them, at the four values the
/// phase spec names and at both of `encoding/json`'s cutoffs.
///
/// `1` and `0` are the cases `serde_json` would get wrong on its own: its `f64`
/// writer puts a decimal point on every plain rendering, so a `temp` of `1`
/// would reach the file as `1.0`. `1e20` is the other: `serde_json` switches to
/// exponent form long before Go's `1e21` cutoff.
#[test]
fn both_float_fields_render_the_way_go_renders_them() {
    for value in [0.7f32, 1.0, 0.0, 1e20, 1e21, 1e-7, -0.0] {
        let recorded = recorded_f32(value.to_bits());

        let mut config = ApiConfig::default();
        config.knowledge.set_top_p(value).expect("a finite float32");
        config.knowledge.set_temp(value).expect("a finite float32");

        let document = String::from_utf8(config.to_json_bytes()).expect("not UTF-8");

        assert!(
            document.contains(&format!(r#""top_p":{recorded},"temp":{recorded},"#)),
            "top_p and temp of {value} did not render as the recorded {recorded}"
        );
        assert_eq!(
            config.knowledge.top_p().to_bits(),
            value.to_bits(),
            "the accessor did not read {value} back"
        );
    }
}

/// A file whose `float32` fields are spelled some other way comes back spelled
/// Go's way, because Go parses into a `float32` and marshals from it rather
/// than carrying the file's bytes through.
#[test]
fn a_float_spelled_another_way_is_renormalised() {
    let document = FIXTURE
        .replace(r#""top_p":0,"#, r#""top_p":0.70,"#)
        .replace(r#""temp":0,"#, r#""temp":1.0,"#);
    assert_ne!(document, FIXTURE, "the replacement did not fire");

    let config =
        ApiConfig::from_json_bytes(document.as_bytes()).expect("the document did not parse");

    let rewritten = String::from_utf8(config.to_json_bytes()).expect("not UTF-8");
    assert!(
        rewritten.contains(&format!(
            r#""top_p":{},"temp":{},"#,
            recorded_f32(0.7f32.to_bits()),
            recorded_f32(1.0f32.to_bits())
        )),
        "0.70 and 1.0 were not renormalised into Go's spelling"
    );
}

// ---------------------------------------------------------------------------
// The go-home percent
// ---------------------------------------------------------------------------

/// Go's `*int` has no `omitempty` (`config.go:55`), so an absent percent is
/// written as `null` rather than dropped. Dropping it would change the file's
/// key set, and the web UI reads the key.
#[test]
fn an_absent_gohome_percent_is_written_as_null() {
    let mut config = ApiConfig::default();
    config.battery.gohome_percent = None;

    let document = String::from_utf8(config.to_json_bytes()).expect("not UTF-8");

    assert!(
        document.contains(r#""battery":{"gohome_percent":null},"#),
        "an absent go-home percent was not written as null: {document}"
    );
}

/// Zero is a value, not an absence: Go's comment at `config.go:53-54` says
/// "nil = default (25), 0 = disabled", so the boot may not turn a zero into 25.
#[tokio::test]
async fn a_zero_gohome_percent_survives_the_boot() {
    let directory = TempDir::new("gohome-zero");
    let seeded = {
        let mut config = ApiConfig::default();
        config.battery.gohome_percent = Some(0);
        config.to_json_bytes()
    };
    directory.seed(&seeded);

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), &directory.data_dir()))
        .await
        .expect("read_config hung");

    assert_eq!(
        boot.config.battery.gohome_percent,
        Some(0),
        "a disabled go-home percent was turned back on"
    );
}

/// `config.go:149-152`: a `null` percent in the file becomes 25 in memory and
/// in the rewritten file.
#[tokio::test]
async fn a_null_gohome_percent_is_forced_to_the_default_by_the_boot() {
    let directory = TempDir::new("gohome-null");
    directory.seed(
        br#"{"battery":{"gohome_percent":null},"hasreadfromenv":true,"pastinitialsetup":true}"#,
    );

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), &directory.data_dir()))
        .await
        .expect("read_config hung");

    assert_eq!(
        boot.config.battery.gohome_percent,
        Some(DEFAULT_GOHOME_PERCENT),
        "the boot did not force the default"
    );
    assert!(
        String::from_utf8(directory.file())
            .expect("not UTF-8")
            .contains(&format!(
                r#""battery":{{"gohome_percent":{DEFAULT_GOHOME_PERCENT}}},"#
            )),
        "the forced default did not reach the file"
    );
}

/// `config.go:61-65`: the web UI's save path does not force the default, which
/// is the difference between it and the two boot writers.
#[tokio::test]
async fn the_save_path_writes_an_absent_percent_through_unchanged() {
    let directory = TempDir::new("gohome-save");
    let mut config = ApiConfig::default();
    config.battery.gohome_percent = None;

    tokio::time::timeout(
        CEILING,
        write_config_to_disk(&config, &directory.data_dir()),
    )
    .await
    .expect("write_config_to_disk hung")
    .expect("the write failed");

    assert!(
        String::from_utf8(directory.file())
            .expect("not UTF-8")
            .contains(r#""gohome_percent":null"#),
        "the save path forced a default Go does not force"
    );
}

// ---------------------------------------------------------------------------
// Unknown keys
// ---------------------------------------------------------------------------

/// A key this struct does not name survives a read and a rewrite, at the top
/// level and inside a nested object. Go drops both.
#[test]
fn unknown_keys_survive_a_round_trip_at_every_level() {
    let document = FIXTURE
        .replace(
            r#""STT":{"provider""#,
            r#""fork_only_toplevel":{"a":[1,2]},"STT":{"provider""#,
        )
        .replace(
            r#""reasoning_effort":"medium"}"#,
            r#""reasoning_effort":"medium","fork_only_nested":42}"#,
        );
    assert_ne!(document, FIXTURE, "the replacements did not fire");

    let config =
        ApiConfig::from_json_bytes(document.as_bytes()).expect("the document did not parse");

    assert_eq!(
        config.extra.keys().collect::<Vec<_>>(),
        ["fork_only_toplevel"],
        "the unknown top-level key was not kept"
    );
    assert_eq!(
        config.knowledge.extra.keys().collect::<Vec<_>>(),
        ["fork_only_nested"],
        "the unknown nested key was not kept"
    );

    let rewritten = String::from_utf8(config.to_json_bytes()).expect("not UTF-8");
    assert!(
        rewritten.contains(r#""fork_only_toplevel":{"a":[1,2]}"#),
        "the unknown top-level key did not survive the rewrite: {rewritten}"
    );
    assert!(
        rewritten.contains(r#""fork_only_nested":42"#),
        "the unknown nested key did not survive the rewrite: {rewritten}"
    );
    assert!(
        rewritten.ends_with(r#""pastinitialsetup":true,"fork_only_toplevel":{"a":[1,2]}}"#),
        "the flattened map is not written last: {rewritten}"
    );
}

// ---------------------------------------------------------------------------
// Decoding tolerances
// ---------------------------------------------------------------------------

/// Go writes nothing into a non-pointer field for a `null` and reports no
/// error, so a hand-edited file full of them still boots.
#[test]
fn a_null_leaves_a_field_at_its_zero_value() {
    let document = br#"{"weather":null,"knowledge":{"provider":null,"top_p":null},"STT":null,"server":null,"battery":null,"hasreadfromenv":null,"pastinitialsetup":null}"#;

    let config = ApiConfig::from_json_bytes(document).expect("Go accepts every one of these nulls");

    assert!(!config.weather.enable, "weather did not stay at its zero");
    assert_eq!(
        config.knowledge.provider, "",
        "the provider did not stay empty"
    );
    assert_eq!(
        config.knowledge.top_p().to_bits(),
        0.0f32.to_bits(),
        "top_p did not stay zero"
    );
    assert_eq!(
        config.battery.gohome_percent, None,
        "a null pointer is an absence"
    );
    assert!(!config.has_read_from_env, "the flag did not stay false");
}

/// Every string goes through the formatter that reproduces `encoding/json`'s
/// HTML escaping, which `serde_json` does not do on its own.
///
/// The five characters and their spellings are `encode.go:984` through
/// `encode.go:1042`, and were confirmed against the installed Go toolchain
/// before this test was written. `/` is in the case deliberately: neither
/// encoder escapes it, so a formatter that over-escaped would fail here too.
#[test]
fn go_escapes_five_characters_serde_json_leaves_alone() {
    let mut config = ApiConfig::default();
    config.knowledge.openai_prompt = "a<b>c&d/e".to_owned();
    config.knowledge.robot_name = "sep\u{2028}line\u{2029}para".to_owned();

    let document = String::from_utf8(config.to_json_bytes()).expect("not UTF-8");

    assert!(
        document.contains(r#""robotName":"sep\u2028line\u2029para""#),
        "the two JavaScript line terminators were not escaped: {document}"
    );
    assert!(
        document.contains(r#""openai_prompt":"a\u003cb\u003ec\u0026d/e""#),
        "the three HTML characters were not escaped, or the solidus was: {document}"
    );

    // And the escaped form reads back as the text that went in, so the
    // escaping is a spelling rather than a change of value.
    let parsed = ApiConfig::from_json_bytes(document.as_bytes()).expect("the escaped form parsed");
    assert_eq!(parsed.knowledge.openai_prompt, "a<b>c&d/e");
    assert_eq!(parsed.knowledge.robot_name, "sep\u{2028}line\u{2029}para");
}

/// The short escapes both encoders agree on, so that a change to the formatter
/// that started rewriting them would be visible.
#[test]
fn the_escapes_both_encoders_share_are_left_alone() {
    let mut config = ApiConfig::default();
    config.knowledge.openai_prompt = "q\"b\\ t\the\nnl\rcr\u{08}bs\u{0c}ff\u{07}bell".to_owned();

    let document = String::from_utf8(config.to_json_bytes()).expect("not UTF-8");

    assert!(
        document.contains(r#""openai_prompt":"q\"b\\ t\the\nnl\rcr\bbs\fff\u0007bell""#),
        "an escape both encoders write the same way moved: {document}"
    );
}

// ---------------------------------------------------------------------------
// Seeding a fresh configuration from the environment
// ---------------------------------------------------------------------------

/// Every one of the twelve variables reaches the field `config.go` puts it in,
/// and the file is written (`config.go:67-100`).
#[tokio::test]
async fn a_fresh_config_is_seeded_from_every_variable() {
    let directory = TempDir::new("seed");
    let env = Env {
        weatherapi_enabled: "true".to_owned(),
        weatherapi_provider: "openweathermap".to_owned(),
        weatherapi_key: "not-a-real-key".to_owned(),
        weatherapi_unit: "F".to_owned(),
        knowledge_enabled: "true".to_owned(),
        knowledge_provider: "houndify".to_owned(),
        knowledge_id: "houndify-client-id".to_owned(),
        knowledge_key: "not-a-real-key-either".to_owned(),
        gohome_battery_percent: "42".to_owned(),
        stt_service: "vosk".to_owned(),
        stt_language: "en-US".to_owned(),
        ddl_rpc_port: "443".to_owned(),
    };

    let (config, written) =
        tokio::time::timeout(CEILING, create_config_from_env(&env, &directory.data_dir()))
            .await
            .expect("create_config_from_env hung");
    written.expect("the write failed");

    assert!(config.weather.enable, "config.go:70");
    assert_eq!(config.weather.provider, "openweathermap", "config.go:71");
    assert_eq!(config.weather.key, "not-a-real-key", "config.go:72");
    assert_eq!(config.weather.unit, "F", "config.go:73");
    assert!(config.knowledge.enable, "config.go:78");
    assert_eq!(config.knowledge.provider, "houndify", "config.go:79");
    assert_eq!(config.knowledge.id, "houndify-client-id", "config.go:81");
    assert_eq!(
        config.knowledge.key, "not-a-real-key-either",
        "config.go:83"
    );
    assert_eq!(config.battery.gohome_percent, Some(42), "config.go:87-90");
    assert_eq!(config.stt.provider, "vosk", "config.go:105");
    assert_eq!(config.stt.language, "en-US", "config.go:107");
    assert!(config.has_read_from_env, "config.go:97");
    assert!(
        !config.past_initial_setup,
        "CreateConfigFromEnv never touches PastInitialSetup"
    );
    assert_eq!(
        config.server.port, "",
        "DDL_RPC_PORT is only ever compared, never stored (config.go:138)"
    );

    assert_eq!(
        directory.file(),
        config.to_json_bytes(),
        "the file is not the config that was returned"
    );
    assert_eq!(
        directory.entries(),
        ["apiConfig.json"],
        "the write left a temporary behind"
    );
}

/// The disabled branches: neither `enable` flag is set, and the settings behind
/// them are left at their zero values even though the variables carry text
/// (`config.go:74-76`, `:84-86`).
#[test]
fn the_enable_variables_gate_everything_behind_them() {
    let env = Env {
        weatherapi_enabled: "TRUE".to_owned(),
        weatherapi_provider: "openweathermap".to_owned(),
        weatherapi_key: "not-a-real-key".to_owned(),
        weatherapi_unit: "F".to_owned(),
        knowledge_enabled: "yes".to_owned(),
        knowledge_provider: "openai".to_owned(),
        knowledge_key: "not-a-real-key-either".to_owned(),
        ..Env::default()
    };

    let config = ApiConfig::from_env(&env);

    assert!(!config.weather.enable, "TRUE is not true (config.go:69)");
    assert_eq!(
        config.weather.provider, "",
        "config.go:75 sets nothing else"
    );
    assert!(!config.knowledge.enable, "yes is not true (config.go:77)");
    assert_eq!(config.knowledge.key, "", "config.go:85 sets nothing else");
}

/// The Houndify ID is the one variable with a provider gate on it
/// (`config.go:80-82`), and the key is set after that gate, so a non-Houndify
/// provider takes the key and not the ID.
#[test]
fn the_houndify_id_is_read_only_for_the_houndify_provider() {
    let base = Env {
        knowledge_enabled: "true".to_owned(),
        knowledge_id: "houndify-client-id".to_owned(),
        knowledge_key: "not-a-real-key".to_owned(),
        ..Env::default()
    };

    let openai = ApiConfig::from_env(&Env {
        knowledge_provider: "openai".to_owned(),
        ..base.clone()
    });
    assert_eq!(
        openai.knowledge.id, "",
        "a non-houndify provider took the ID"
    );
    assert_eq!(
        openai.knowledge.key, "not-a-real-key",
        "the key is not gated"
    );

    let houndify = ApiConfig::from_env(&Env {
        knowledge_provider: "houndify".to_owned(),
        ..base
    });
    assert_eq!(houndify.knowledge.id, "houndify-client-id", "config.go:81");
}

/// `config.go:87-95`: an unparseable or absent percent leaves the default, and
/// `config.go:106-108`: the language is only read for the two services that
/// have models per language.
#[test]
fn the_two_conditional_environment_rules_take_their_other_branch() {
    let unparseable = ApiConfig::from_env(&Env {
        gohome_battery_percent: "half".to_owned(),
        stt_service: "houndify".to_owned(),
        stt_language: "en-US".to_owned(),
        ..Env::default()
    });
    assert_eq!(
        unparseable.battery.gohome_percent,
        Some(DEFAULT_GOHOME_PERCENT),
        "strconv.Atoi's failure is discarded and the default forced (config.go:88, :92-95)"
    );
    assert_eq!(unparseable.stt.provider, "houndify", "config.go:105");
    assert_eq!(
        unparseable.stt.language, "",
        "only vosk and whisper.cpp read the language (config.go:106)"
    );

    for service in ["vosk", "whisper.cpp"] {
        let config = ApiConfig::from_env(&Env {
            stt_service: service.to_owned(),
            stt_language: "de-DE".to_owned(),
            ..Env::default()
        });
        assert_eq!(
            config.stt.language, "de-DE",
            "{service} did not read the language"
        );
    }
}

// ---------------------------------------------------------------------------
// The boot path
// ---------------------------------------------------------------------------

/// `config.go:112-114`: no file means a fresh one, seeded and written, and the
/// log line that says so.
#[tokio::test]
async fn a_missing_file_is_created_from_the_environment() {
    let directory = TempDir::new("created");
    let env = Env {
        stt_service: "vosk".to_owned(),
        stt_language: "en-US".to_owned(),
        ..Env::default()
    };
    let ring = ring();

    let boot = {
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(&ring)));
        let _guard = tracing::subscriber::set_default(subscriber);
        tokio::time::timeout(CEILING, read_config(&env, &directory.data_dir()))
            .await
            .expect("read_config hung")
    };

    match boot.outcome {
        BootOutcome::Created(result) => result.expect("the write failed"),
        other => panic!("a missing file took the wrong arm: {other:?}"),
    }
    assert_eq!(directory.file(), boot.config.to_json_bytes());
    assert!(boot.config.has_read_from_env, "config.go:97");
    assert_eq!(
        recorded_lines(&ring),
        [(
            "DEBUG".to_owned(),
            String::new(),
            "API config JSON created".to_owned()
        )],
        "config.go:114"
    );
}

/// `config.go:133-136`: the STT provider is overridden from `STT_SERVICE` at
/// every boot, not only the first, and the language comes with it.
#[tokio::test]
async fn the_stt_provider_is_overridden_from_the_environment_at_every_boot() {
    let directory = TempDir::new("stt");
    let seeded = {
        let mut config = ApiConfig::default();
        config.stt.provider = "vosk".to_owned();
        config.stt.language = "en-US".to_owned();
        config.has_read_from_env = true;
        config.past_initial_setup = true;
        config.battery.gohome_percent = Some(DEFAULT_GOHOME_PERCENT);
        config.to_json_bytes()
    };
    directory.seed(&seeded);
    let env = Env {
        stt_service: "whisper.cpp".to_owned(),
        stt_language: "fr-FR".to_owned(),
        ..Env::default()
    };

    let boot = tokio::time::timeout(CEILING, read_config(&env, &directory.data_dir()))
        .await
        .expect("read_config hung");

    assert_eq!(boot.config.stt.provider, "whisper.cpp", "config.go:105");
    assert_eq!(boot.config.stt.language, "fr-FR", "config.go:107");
    assert_eq!(
        directory.file(),
        boot.config.to_json_bytes(),
        "the override did not reach the file"
    );

    // And a second boot with the same environment changes nothing, because the
    // comparison at config.go:134 no longer differs.
    let again = tokio::time::timeout(CEILING, read_config(&env, &directory.data_dir()))
        .await
        .expect("read_config hung");
    assert_eq!(directory.file(), again.config.to_json_bytes());
    assert_eq!(again.config.stt.language, "fr-FR");
}

/// `config.go:144-147`: the Together model name is rewritten, with the log line
/// that announces it.
#[tokio::test]
async fn the_llama_2_model_name_is_rewritten_to_llama_3() {
    let directory = TempDir::new("llama");
    let seeded = {
        let mut config = ApiConfig::default();
        config.knowledge.model = LLAMA2_MODEL.to_owned();
        config.has_read_from_env = true;
        config.battery.gohome_percent = Some(DEFAULT_GOHOME_PERCENT);
        config.to_json_bytes()
    };
    directory.seed(&seeded);
    let ring = ring();

    let boot = {
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(&ring)));
        let _guard = tracing::subscriber::set_default(subscriber);
        tokio::time::timeout(CEILING, read_config(&empty_env(), &directory.data_dir()))
            .await
            .expect("read_config hung")
    };

    assert_eq!(boot.config.knowledge.model, LLAMA3_MODEL, "config.go:146");
    assert!(
        String::from_utf8(directory.file())
            .expect("not UTF-8")
            .contains(LLAMA3_MODEL),
        "the rewrite did not reach the file"
    );
    assert_eq!(
        recorded_lines(&ring),
        [
            (
                "DEBUG".to_owned(),
                String::new(),
                "Setting Together model to Llama3".to_owned()
            ),
            (
                "DEBUG".to_owned(),
                String::new(),
                "API config successfully read".to_owned()
            ),
        ],
        "config.go:145 and :156"
    );

    // Any other model name is left alone.
    let directory = TempDir::new("llama-other");
    let mut other = ApiConfig::default();
    other.knowledge.model = "meta-llama/Llama-3-70b-chat-hf".to_owned();
    other.battery.gohome_percent = Some(DEFAULT_GOHOME_PERCENT);
    directory.seed(&other.to_json_bytes());
    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), &directory.data_dir()))
        .await
        .expect("read_config hung");
    assert_eq!(boot.config.knowledge.model, LLAMA3_MODEL);
}

/// `config.go:154-155`: every successful boot rewrites the file, whether or not
/// anything changed. The seeded document below fires none of the four rules and
/// is still replaced, because it is spelled with whitespace Go's marshaller
/// does not write.
#[tokio::test]
async fn every_boot_rewrites_the_file_even_when_nothing_changed() {
    let directory = TempDir::new("rewrite");
    // Written as a literal rather than by mutation so that a field added to
    // ApiConfig has to be considered here: a settled configuration is one no
    // rule in read_config would change, and a new field could change that.
    let settled = ApiConfig {
        battery: BatteryConfig {
            gohome_percent: Some(DEFAULT_GOHOME_PERCENT),
            ..BatteryConfig::default()
        },
        has_read_from_env: true,
        past_initial_setup: true,
        ..ApiConfig::default()
    };
    let canonical = settled.to_json_bytes();
    let pretty = serde_json::to_vec_pretty(&settled).expect("the pretty form is serialisable");
    assert_ne!(pretty, canonical, "the two spellings are the same document");
    directory.seed(&pretty);

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), &directory.data_dir()))
        .await
        .expect("read_config hung");

    assert!(matches!(boot.outcome, BootOutcome::Read(Ok(()))));
    assert_eq!(
        boot.config, settled,
        "no rule should have fired on a settled configuration"
    );
    assert_eq!(
        directory.file(),
        canonical,
        "the boot did not rewrite a file it had nothing to change"
    );
    assert_eq!(
        directory.entries(),
        ["apiConfig.json"],
        "the rewrite left a temporary behind"
    );
}

/// `config.go:137-142`: the two setup flags move together, and only from a
/// configuration that has never been seeded from the environment whose stored
/// port differs from `DDL_RPC_PORT`.
#[tokio::test]
async fn the_two_setup_flags_move_together_on_a_port_change() {
    for (stored_port, env_port, want) in [("443", "8080", true), ("443", "443", false)] {
        let directory = TempDir::new("flags");
        let seeded = {
            let mut config = ApiConfig::default();
            config.server.port = stored_port.to_owned();
            config.battery.gohome_percent = Some(DEFAULT_GOHOME_PERCENT);
            config.to_json_bytes()
        };
        directory.seed(&seeded);

        let boot = tokio::time::timeout(
            CEILING,
            read_config(
                &Env {
                    ddl_rpc_port: env_port.to_owned(),
                    ..Env::default()
                },
                &directory.data_dir(),
            ),
        )
        .await
        .expect("read_config hung");

        assert_eq!(
            boot.config.has_read_from_env, want,
            "stored {stored_port} against DDL_RPC_PORT {env_port}"
        );
        assert_eq!(
            boot.config.past_initial_setup, want,
            "the two flags did not move together"
        );
    }

    // A configuration that has already read the environment is never touched,
    // however far the port has moved.
    let directory = TempDir::new("flags-set");
    let seeded = {
        let mut config = ApiConfig::default();
        config.server.port = "443".to_owned();
        config.has_read_from_env = true;
        config.battery.gohome_percent = Some(DEFAULT_GOHOME_PERCENT);
        config.to_json_bytes()
    };
    directory.seed(&seeded);
    let boot = tokio::time::timeout(
        CEILING,
        read_config(
            &Env {
                ddl_rpc_port: "8080".to_owned(),
                ..Env::default()
            },
            &directory.data_dir(),
        ),
    )
    .await
    .expect("read_config hung");
    assert!(
        !boot.config.past_initial_setup,
        "config.go:137 gates the whole block on HasReadFromEnv"
    );
}

/// `config.go:117-124`: a file that exists and cannot be read disables
/// knowledge and weather in memory, logs two lines, and writes nothing.
///
/// A directory in the file's place is the portable way to arrange it: `stat`
/// succeeds, so Go takes the else branch, and the read fails.
#[tokio::test]
async fn a_file_that_cannot_be_read_is_left_exactly_as_it_is() {
    let directory = TempDir::new("unreadable");
    fs::create_dir(directory.path().join("apiConfig.json"))
        .expect("could not put a directory in the file's place");
    let ring = ring();

    let boot = {
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(&ring)));
        let _guard = tracing::subscriber::set_default(subscriber);
        tokio::time::timeout(CEILING, read_config(&empty_env(), &directory.data_dir()))
            .await
            .expect("read_config hung")
    };

    assert!(
        matches!(boot.outcome, BootOutcome::ReadFailed(_)),
        "the unreadable file took the wrong arm: {:?}",
        boot.outcome
    );
    assert!(!boot.config.knowledge.enable, "config.go:119");
    assert!(!boot.config.weather.enable, "config.go:120");
    assert!(
        fs::read_dir(directory.path().join("apiConfig.json"))
            .expect("the directory went away")
            .next()
            .is_none(),
        "something was written where the unreadable file is"
    );

    let lines = recorded_lines(&ring);
    assert_eq!(
        lines.len(),
        2,
        "config.go:121-122 logs two lines: {lines:?}"
    );
    assert_eq!(
        lines[0],
        (
            "DEBUG".to_owned(),
            String::new(),
            "Failed to read API config file".to_owned()
        ),
        "config.go:121"
    );
}

/// `config.go:125-132`: a file that is not the JSON this struct describes takes
/// the same shape of failure, and the bytes on disk are untouched.
#[tokio::test]
async fn a_file_that_does_not_parse_is_left_exactly_as_it_is() {
    let directory = TempDir::new("unparseable");
    let garbage = br#"{"weather":{"enable":"not a bool"}}"#;
    directory.seed(garbage);
    let ring = ring();

    let boot = {
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(&ring)));
        let _guard = tracing::subscriber::set_default(subscriber);
        tokio::time::timeout(CEILING, read_config(&empty_env(), &directory.data_dir()))
            .await
            .expect("read_config hung")
    };

    assert!(
        matches!(boot.outcome, BootOutcome::ParseFailed(_)),
        "the unparseable file took the wrong arm: {:?}",
        boot.outcome
    );
    assert!(!boot.config.knowledge.enable, "config.go:127");
    assert!(!boot.config.weather.enable, "config.go:128");
    assert_eq!(
        directory.file(),
        garbage,
        "the boot rewrote a file it could not read"
    );
    assert_eq!(
        directory.entries(),
        ["apiConfig.json"],
        "the failed boot left a temporary behind"
    );

    let lines = recorded_lines(&ring);
    assert_eq!(
        lines.len(),
        2,
        "config.go:129-130 logs two lines: {lines:?}"
    );
    assert_eq!(
        lines[0],
        (
            "DEBUG".to_owned(),
            String::new(),
            "Failed to unmarshal API config JSON".to_owned()
        ),
        "config.go:129"
    );
}

/// `config.go:62`: the save path's one log line.
#[tokio::test]
async fn the_save_path_logs_the_line_go_logs() {
    let directory = TempDir::new("save-log");
    let ring = ring();

    {
        let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(&ring)));
        let _guard = tracing::subscriber::set_default(subscriber);
        tokio::time::timeout(
            CEILING,
            write_config_to_disk(&ApiConfig::default(), &directory.data_dir()),
        )
        .await
        .expect("write_config_to_disk hung")
        .expect("the write failed");
    }

    assert_eq!(
        recorded_lines(&ring),
        [(
            "DEBUG".to_owned(),
            String::new(),
            "Configuration changed, writing to disk".to_owned()
        )],
        "config.go:62"
    );
}

/// The mode is the one Go passes at all three config write sites.
#[test]
fn the_file_mode_is_gos() {
    assert_eq!(
        CONFIG_FILE_MODE, 0o644,
        "config.go:64, :99 and :155 all pass 0644"
    );
}

/// [`go_marshal`] is what produces the bytes, and it is exported because the
/// handler that serves the configuration to the web UI needs the same
/// spelling.
#[test]
fn go_marshal_writes_the_same_bytes_the_config_writes() {
    let config = ApiConfig::from_json_bytes(FIXTURE.as_bytes()).expect("the fixture parsed");

    assert_eq!(
        go_marshal(&config).expect("an ApiConfig serialises"),
        config.to_json_bytes()
    );
}
