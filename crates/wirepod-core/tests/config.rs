//! `apiConfig.json`: the byte layout, `encoding/json`'s decoder, and the four
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
    ApiConfig, BatteryConfig, BootOutcome, CONFIG_FILE_MODE, DEFAULT_GOHOME_PERCENT, DecodeError,
    DecodeFault, Env, LLAMA2_MODEL, LLAMA3_MODEL, config_gate, create_config_from_env, go_marshal,
    read_config, write_config_to_disk,
};
use wirepod_core::logger::{LogLayer, LogRing, ManualLogClock};
use wirepod_core::paths::DataDir;
use wirepod_core::persist::WriteGate;
use wirepod_core::test_support::install_tracing_backstop;

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
    gate: WriteGate,
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
        let gate = config_gate(&DataDir::rooted(&path));
        Self { path, gate }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// The one gate every writer of this directory's `apiConfig.json` takes.
    ///
    /// One per directory, not one per call: a second gate over the same path
    /// would order nothing, which is the whole property
    /// [`an_overlapping_save_does_not_leave_the_file_behind_the_server`]
    /// depends on.
    fn gate(&self) -> &WriteGate {
        &self.gate
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

/// A denial of one step of [`wirepod_core::persist::write_atomic`], lifted when
/// it is dropped so the directory can still be removed.
struct WritesRefused<'a> {
    directory: &'a TempDir,
}

/// Makes any write into `directory` fail, so that a write the code attempts is
/// told apart from one it skipped without asking a clock.
///
/// Windows refuses to rename over a file carrying the read-only attribute, and
/// Unix refuses to create the temporary in a directory with no write bit, so
/// each platform denies one step of the replacement.
fn refuse_writes(directory: &TempDir) -> WritesRefused<'_> {
    #[cfg(windows)]
    {
        let path = directory.path().join("apiConfig.json");
        let mut permissions = fs::metadata(&path)
            .expect("the file to protect is missing")
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).expect("could not set the read-only attribute");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555))
            .expect("could not take the write bit off the directory");
    }
    WritesRefused { directory }
}

impl Drop for WritesRefused<'_> {
    fn drop(&mut self) {
        // The lint is about Unix, where clearing the read-only flag makes the
        // file world writable; this arm is Windows only, where the flag is the
        // whole of what `Permissions` carries and clearing it is the only way
        // to let the directory be removed.
        #[cfg(windows)]
        #[allow(clippy::permissions_set_readonly_false)]
        {
            let path = self.directory.path().join("apiConfig.json");
            if let Ok(metadata) = fs::metadata(&path) {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                let _ = fs::set_permissions(&path, permissions);
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let _ = fs::set_permissions(self.directory.path(), fs::Permissions::from_mode(0o755));
        }
    }
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

/// Installs `ring` as this thread's subscriber until the guard is dropped, over
/// the process-wide backstop.
///
/// Both halves are load-bearing. `tracing` decides once per callsite whether
/// anybody is interested and caches the answer for every thread, and while at
/// most one dispatcher is registered it asks only the calling thread, so a
/// callsite first reached by a test that installed nothing is cached as *never*
/// and every later test that asserts on that line sees nothing. The harness
/// runs these in parallel, so which test gets there first is the scheduler's
/// choice: this file failed 102 runs out of 200 at two threads over
/// [`the_llama_2_model_name_is_rewritten_to_llama_3`] and
/// [`the_stt_provider_is_overridden_from_the_environment_at_every_boot`] before
/// the backstop existed. [`install_tracing_backstop`] is the fix and its doc
/// says why a global default is the only shape that works; the `set_default`
/// here is still what lets this test read its own ring back.
fn watching(ring: &Arc<LogRing>) -> tracing::subscriber::DefaultGuard {
    install_tracing_backstop();
    let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(ring)));
    tracing::subscriber::set_default(subscriber)
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

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
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

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
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

    tokio::time::timeout(CEILING, write_config_to_disk(&config, directory.gate()))
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
// Go's decoder, rule by rule
// ---------------------------------------------------------------------------
//
// Every document below was run through Go's own `encoding/json` against a copy
// of `config.go:17-59`'s struct before it was written here, and the assertion
// is what Go answered.

/// `decode.go:661-827` walks the object one key at a time and writes through a
/// pointer into the struct, so a key that appears twice is applied twice and
/// the second application stands.
///
/// Go answers `hasreadfromenv` true for the first document and `STT.provider`
/// `b` for the second. A derived `Deserialize` refuses both with a duplicate
/// field error, and because the parse-failure arm writes nothing
/// (`config.go:131`) the duplicate would stay in the file and every later boot
/// would fail the same way, where Go boots and rewrites the file deduplicated.
#[test]
fn a_duplicate_key_takes_the_last_occurrence() {
    let config = ApiConfig::from_json_bytes(br#"{"hasreadfromenv":false,"hasreadfromenv":true}"#)
        .expect("Go accepts a duplicate key");
    assert!(config.has_read_from_env, "the later occurrence did not win");

    let config = ApiConfig::from_json_bytes(br#"{"hasreadfromenv":true,"hasreadfromenv":false}"#)
        .expect("Go accepts a duplicate key");
    assert!(
        !config.has_read_from_env,
        "the earlier occurrence won instead"
    );

    let config = ApiConfig::from_json_bytes(br#"{"STT":{"provider":"a"},"STT":{"provider":"b"}}"#)
        .expect("Go accepts a duplicate key");
    assert_eq!(config.stt.provider, "b", "the later occurrence did not win");

    let config = ApiConfig::from_json_bytes(br#"{"STT":{"provider":"a","provider":"b"}}"#)
        .expect("Go accepts a duplicate key inside a nested object too");
    assert_eq!(config.stt.provider, "b");
}

/// The other half of writing through a pointer into the struct: a second
/// object does not replace the first, it is decoded on top of it, so a field
/// only the first one named survives.
#[test]
fn a_duplicate_object_is_merged_rather_than_replaced() {
    let config = ApiConfig::from_json_bytes(br#"{"STT":{"provider":"a"},"STT":{"language":"x"}}"#)
        .expect("Go accepts this");

    assert_eq!(config.stt.provider, "a", "the first object was discarded");
    assert_eq!(config.stt.language, "x", "the second object was discarded");

    // And a null second occurrence changes nothing at all, because a null into
    // a struct is ignored (`decode.go:899-903`).
    let config = ApiConfig::from_json_bytes(br#"{"STT":{"provider":"a"},"STT":null}"#)
        .expect("Go accepts this");
    assert_eq!(config.stt.provider, "a");
}

/// `decode.go:694-697`: a key that is not a tag exactly is looked up again
/// under Go's case-insensitive fold, so `WEATHER` fills `weather` and is not an
/// unknown key.
///
/// The rollback hazard is why this matters rather than the tidiness. A key kept
/// in the flatten map is written back last, and Go on rollback applies whichever
/// of two matching keys comes last, so a `WEATHER` the port preserved would
/// silently become the configuration the Go server reads.
#[test]
fn a_case_variant_key_fills_the_field_it_folds_to() {
    let config = ApiConfig::from_json_bytes(br#"{"WEATHER":{"ENABLE":true,"PrOvIdEr":"x"}}"#)
        .expect("Go accepts this");

    assert!(
        config.weather.enable,
        "the folded nested key was not stored"
    );
    assert_eq!(config.weather.provider, "x");
    assert!(
        config.extra.is_empty() && config.weather.extra.is_empty(),
        "a folded key was kept as an unknown one and would be written back"
    );

    // The exact match wins over the folded one wherever both are present, which
    // is the order `decode.go:694-697` tries them in and not the order the keys
    // are in.
    let config =
        ApiConfig::from_json_bytes(br#"{"WEATHER":{"provider":"a"},"Weather":{"key":"b"}}"#)
            .expect("Go accepts this");
    assert_eq!(config.weather.provider, "a");
    assert_eq!(config.weather.key, "b");

    // `fold.go:39-48`: the fold reaches two non-ASCII runes as well.
    let config = ApiConfig::from_json_bytes("{\"\u{017f}tt\":{\"provider\":\"vosk\"}}".as_bytes())
        .expect("Go accepts this");
    assert_eq!(
        config.stt.provider, "vosk",
        "U+017F does not fold onto S the way Go folds it"
    );
}

/// `decode.go:243-247` records the first type error and `decode.go:182` returns
/// it only after the whole document has been walked, so every other field is
/// filled, the ones before the fault and the ones after it alike.
#[test]
fn a_type_error_keeps_every_other_field_and_reports_the_first() {
    let error = ApiConfig::from_json_bytes(
        br#"{"weather":{"enable":"nope"},"knowledge":{"provider":5},"STT":{"provider":"vosk"},"hasreadfromenv":true}"#,
    )
    .expect_err("Go reports the first type error");

    assert_eq!(
        error.config.stt.provider, "vosk",
        "a field after the fault was dropped"
    );
    assert!(
        error.config.has_read_from_env,
        "a field after the fault was dropped"
    );
    assert_eq!(
        error.fault,
        DecodeFault::Type {
            path: "weather.enable".to_owned(),
            found: "string".to_owned(),
            want: "bool",
        },
        "the fault is not the first one in the document"
    );
    assert!(
        !error.config.weather.enable,
        "the field the fault hit was written anyway"
    );
    assert_eq!(
        error.config.knowledge.provider, "",
        "the second fault's field was written anyway"
    );
}

/// The same rule one level up: a value that is not an object where a struct is
/// leaves the struct alone and does not stop the document.
#[test]
fn a_scalar_where_a_struct_belongs_is_one_fault_and_no_more() {
    let error = ApiConfig::from_json_bytes(br#"{"weather":5,"STT":{"provider":"vosk"}}"#)
        .expect_err("Go reports a type error");

    assert_eq!(error.config.stt.provider, "vosk");
    assert_eq!(
        error.fault,
        DecodeFault::Type {
            path: "weather".to_owned(),
            found: "number".to_owned(),
            want: "struct",
        }
    );
}

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

    // A null is an absence rather than a no-op for the one field that is a Go
    // pointer, even over a value an earlier occurrence set
    // (`decode.go:465-467`, `:899-901`).
    let config =
        ApiConfig::from_json_bytes(br#"{"battery":{"gohome_percent":5,"gohome_percent":null}}"#)
            .expect("Go accepts this");
    assert_eq!(config.battery.gohome_percent, None);
    let config =
        ApiConfig::from_json_bytes(br#"{"battery":{"gohome_percent":null,"gohome_percent":5}}"#)
            .expect("Go accepts this");
    assert_eq!(config.battery.gohome_percent, Some(5));
}

/// The go-home pointer is allocated before the value is stored
/// (`decode.go:476-478`), so a type error leaves it pointing at a zero rather
/// than at nothing, which is `0 = disabled` and not `nil = default (25)`.
#[test]
fn a_type_error_on_the_go_home_pointer_leaves_it_at_zero() {
    for document in [
        &br#"{"battery":{"gohome_percent":"25"}}"#[..],
        &br#"{"battery":{"gohome_percent":25.0}}"#[..],
        &br#"{"battery":{"gohome_percent":true}}"#[..],
        &br#"{"battery":{"gohome_percent":{"a":1}}}"#[..],
        &br#"{"battery":{"gohome_percent":[1]}}"#[..],
    ] {
        let error = ApiConfig::from_json_bytes(document).expect_err("Go reports a type error");
        assert_eq!(
            error.config.battery.gohome_percent,
            Some(0),
            "{}: the pointer was not allocated",
            String::from_utf8_lossy(document)
        );
    }

    // A value that did store is not clobbered by a later fault.
    let error =
        ApiConfig::from_json_bytes(br#"{"battery":{"gohome_percent":5,"gohome_percent":"x"}}"#)
            .expect_err("Go reports a type error");
    assert_eq!(error.config.battery.gohome_percent, Some(5));
}

/// A `float32` literal is converted once, by `strconv.ParseFloat(s, 32)`
/// (`decode.go:1014`).
///
/// Parsing to an [`f64`] and casting rounds twice, and for a literal with
/// sixteen or more significant digits the two answers can be one unit in the
/// last place apart. Each pair below is Go's answer and the answer the double
/// rounding gives, both read off the installed Go toolchain.
#[test]
fn a_float_literal_is_rounded_once() {
    for (literal, go, double_rounded) in [
        ("0.10000002756714821", 0x3dcc_ccd1u32, 0x3dcc_ccd0u32),
        ("3.9885270197714817e-08", 0x332b_4e51, 0x332b_4e52),
        ("0.0016355645493604243", 0x3ad6_6071, 0x3ad6_6070),
        ("5.4522778987884521", 0x40ae_790f, 0x40ae_7910),
        ("0.015507491771131754", 0x3c7e_1323, 0x3c7e_1322),
    ] {
        assert_ne!(
            go, double_rounded,
            "{literal} is not a double-rounding case"
        );

        let document = format!(r#"{{"knowledge":{{"top_p":{literal},"temp":{literal}}}}}"#);
        let config = ApiConfig::from_json_bytes(document.as_bytes()).expect("the document parses");

        assert_eq!(
            config.knowledge.top_p().to_bits(),
            go,
            "{literal} did not round the way strconv.ParseFloat(s, 32) rounds it"
        );
        assert_eq!(config.knowledge.temp().to_bits(), go);
    }
}

/// `decode.go:1015-1017`: a literal `strconv.ParseFloat` answers an infinity
/// and an `ErrRange` for is a type error, and the field keeps what it had.
/// Underflow is not: `1e-46` is a zero and no error, in Go and here.
#[test]
fn a_float_that_does_not_fit_float32_is_a_type_error() {
    for literal in ["1e39", "-1e39"] {
        let document = format!(r#"{{"knowledge":{{"top_p":{literal},"model":"kept"}}}}"#);
        let error = ApiConfig::from_json_bytes(document.as_bytes())
            .expect_err("Go reports a range error as a type error");

        assert_eq!(
            error.config.knowledge.model, "kept",
            "decoding stopped at the bad float"
        );
        assert_eq!(
            error.config.knowledge.top_p().to_bits(),
            0.0f32.to_bits(),
            "the field took the infinity anyway"
        );
        assert_eq!(
            error.fault,
            DecodeFault::Type {
                path: "knowledge.top_p".to_owned(),
                found: format!("number {literal}"),
                want: "float32",
            }
        );
    }

    let config = ApiConfig::from_json_bytes(br#"{"knowledge":{"top_p":1e-46}}"#)
        .expect("Go accepts an underflow");
    assert_eq!(config.knowledge.top_p().to_bits(), 0.0f32.to_bits());
}

/// `decode.go:98-105`: Go checks the whole document before it decodes anything,
/// so bytes that are not one JSON value leave the configuration at its zero
/// value where a type error does not.
#[test]
fn a_document_that_is_not_one_json_value_keeps_nothing() {
    for document in [
        &br#"{"STT":{"provider":"vosk"},"weather":"#[..],
        &br#"{"hasreadfromenv":true} and then some"#[..],
    ] {
        let error = ApiConfig::from_json_bytes(document).expect_err("these are not JSON");
        assert_eq!(
            error.config,
            ApiConfig::default(),
            "{}: a half-decoded value survived a syntax error",
            String::from_utf8_lossy(document)
        );
        assert!(matches!(error.fault, DecodeFault::Malformed(_)));
    }

    // A document that is one JSON value but not an object is the other shape:
    // Go reports a type error against the struct and changes nothing.
    let error = ApiConfig::from_json_bytes(br#"[1,2]"#).expect_err("Go reports a type error");
    assert_eq!(error.config, ApiConfig::default());
    assert_eq!(
        error.fault,
        DecodeFault::Type {
            path: String::new(),
            found: "array".to_owned(),
            want: "apiConfig",
        }
    );

    // And a bare `null` is neither: Go writes nothing and reports nothing.
    assert_eq!(
        ApiConfig::from_json_bytes(b"null").expect("Go accepts a bare null"),
        ApiConfig::default()
    );
}

/// Every string goes through the formatter that reproduces `encoding/json`'s
/// HTML escaping, which `serde_json` does not do on its own.
///
/// The five characters and their spellings are `encode.go:984` through
/// `encode.go:1043`, and were confirmed against the installed Go toolchain
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
        tokio::time::timeout(CEILING, create_config_from_env(&env, directory.gate()))
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
        let _guard = watching(&ring);
        tokio::time::timeout(CEILING, read_config(&env, directory.gate()))
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

    let boot = tokio::time::timeout(CEILING, read_config(&env, directory.gate()))
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
    let again = tokio::time::timeout(CEILING, read_config(&env, directory.gate()))
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
        let _guard = watching(&ring);
        tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
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
    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
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

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
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

/// The other half of `config.go:154-155`, which the test above cannot reach: a
/// file already holding the exact bytes the rewrite would produce is rewritten
/// anyway.
///
/// Comparing the two byte strings and skipping the write would leave that test
/// green, because the document it seeds is a different spelling of the same
/// configuration. What tells a write that happened from one that did not,
/// without asking a clock, is a write that is refused: [`refuse_writes`] denies
/// one step of the replacement, so the boot reports the failure if it wrote and
/// reports success if it skipped.
#[tokio::test]
async fn a_file_already_holding_the_rewrite_is_rewritten_anyway() {
    let directory = TempDir::new("rewrite-same");
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
    directory.seed(&canonical);
    let guard = refuse_writes(&directory);

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
        .await
        .expect("read_config hung");

    drop(guard);

    assert!(
        matches!(boot.outcome, BootOutcome::Read(Err(_))),
        "the boot did not try to write a file whose bytes already matched: {:?}",
        boot.outcome
    );
    assert_eq!(
        directory.file(),
        canonical,
        "a refused write changed the file"
    );
    assert_eq!(
        directory.entries(),
        ["apiConfig.json"],
        "the failed rewrite left a temporary behind"
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
                directory.gate(),
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
            directory.gate(),
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
        let _guard = watching(&ring);
        tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
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

/// The half of `config.go:117-124` the test above cannot see: the arm writes
/// nothing where a write would have landed.
///
/// A directory in the file's place refuses a write as firmly as it refuses a
/// read, so a boot that wrote a zeroed configuration on this arm would look the
/// same. Here the file is real and the directory around it is ordinary: only
/// the read is denied, so a write would land and changed bytes would show it.
///
/// `FILE_SHARE_DELETE` alone is what arranges that on Windows. An open for
/// reading is refused, because the hold does not share reading; a rename is
/// not, because to the sharing rules a rename is a delete.
#[cfg(windows)]
#[tokio::test]
async fn an_unreadable_file_is_not_overwritten() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_DELETE: u32 = 0x0000_0004;

    let directory = TempDir::new("unreadable-bytes");
    let seeded = br#"{"STT":{"provider":"vosk"},"hasreadfromenv":true}"#;
    directory.seed(seeded);

    let held = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_DELETE)
        .open(directory.path().join("apiConfig.json"))
        .expect("could not hold the file open");

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
        .await
        .expect("read_config hung");

    drop(held);

    assert!(
        matches!(boot.outcome, BootOutcome::ReadFailed(_)),
        "the held file took the wrong arm: {:?}",
        boot.outcome
    );
    assert_eq!(
        directory.file(),
        seeded,
        "config.go:123 returns before the write; the arm wrote anyway"
    );
    assert_eq!(
        directory.entries(),
        ["apiConfig.json"],
        "the arm left a temporary behind"
    );
}

/// The Unix twin of [`an_unreadable_file_is_not_overwritten`]: the file has no
/// permission bits at all, so the read fails, while the directory around it
/// stays writable so a write would land.
///
/// Root ignores the mode and would read the file, which would take the boot
/// down the wrong arm; the same is true of the read-only-directory test beside
/// this one in `tests/persist.rs`, and neither CI runner is root.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_file_is_not_overwritten() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDir::new("unreadable-bytes");
    let path = directory.path().join("apiConfig.json");
    let seeded = br#"{"STT":{"provider":"vosk"},"hasreadfromenv":true}"#;
    directory.seed(seeded);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000))
        .expect("could not take every permission bit off the file");

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
        .await
        .expect("read_config hung");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
        .expect("could not put the permissions back");

    assert!(
        matches!(boot.outcome, BootOutcome::ReadFailed(_)),
        "the unreadable file took the wrong arm: {:?}",
        boot.outcome
    );
    assert_eq!(
        directory.file(),
        seeded,
        "config.go:123 returns before the write; the arm wrote anyway"
    );
    assert_eq!(
        directory.entries(),
        ["apiConfig.json"],
        "the arm left a temporary behind"
    );
}

/// `config.go:125-132` over a document with one type error: Go's global keeps
/// every field that decoded, and the next line of `vars.Init` reads one of them.
///
/// `vars.go:234` chooses the STT engine from `APIConfig.STT.Service` right after
/// `ReadConfig` returns, so a boot that threw the partial decode away would pick
/// a different engine, port and model from the same broken file.
#[tokio::test]
async fn the_parse_failure_arm_keeps_what_decoded() {
    let directory = TempDir::new("partial");
    let garbage = br#"{"weather":{"enable":"not a bool","provider":"openweathermap"},"knowledge":{"enable":true},"STT":{"provider":"vosk","language":"en-US"},"server":{"port":"443"}}"#;
    directory.seed(garbage);

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
        .await
        .expect("read_config hung");

    assert!(
        matches!(boot.outcome, BootOutcome::ParseFailed(_)),
        "the document took the wrong arm: {:?}",
        boot.outcome
    );
    assert_eq!(
        boot.config.stt.provider, "vosk",
        "vars.go:234 would read an empty STT service out of this boot"
    );
    assert_eq!(boot.config.stt.language, "en-US");
    assert_eq!(boot.config.server.port, "443");
    assert_eq!(
        boot.config.weather.provider, "openweathermap",
        "the field after the fault in the same object was dropped"
    );
    // `config.go:127-128` still runs, over the half-filled value rather than
    // over a zero one, so the `true` the document set is turned back off.
    assert!(!boot.config.knowledge.enable, "config.go:127");
    assert!(!boot.config.weather.enable, "config.go:128");
    assert_eq!(
        directory.file(),
        garbage,
        "the boot rewrote a file it could not read"
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
        let _guard = watching(&ring);
        tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
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
        let _guard = watching(&ring);
        tokio::time::timeout(
            CEILING,
            write_config_to_disk(&ApiConfig::default(), directory.gate()),
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

// ---------------------------------------------------------------------------
// Two writers of one file
// ---------------------------------------------------------------------------

/// Go's three config writers all reach one file, and two of them can overlap
/// here: the web UI's save runs on an axum handler
/// (`config-ws/webserver.go:202`) while the boot rewrite runs as a task
/// (`config.go:155`). Two unordered writes land their renames in the order the
/// writes *finish*, which is the order of their sizes, while their bytes were
/// decided in the order they were issued, so the file can be left holding a
/// configuration the server has already replaced, and stay that way until
/// something writes again.
///
/// The shape is `tests/jdocs.rs`'s
/// `an_overlapping_mutation_does_not_leave_the_file_behind_the_list`, adapted
/// to a file whose writers each bring their own value: a six-megabyte prompt
/// against a default configuration, issued in that order onto one gate.
/// `biased;` makes `join!` poll in source order, so the large save is polled
/// first and the small one is issued while the large one is in flight; without
/// a gate both reach `spawn_blocking` on that first poll and the small one's
/// rename lands first, leaving the large one's bytes on disk. Every awaited
/// duration is a real-clock ceiling; the runtime's clock is never paused.
#[tokio::test]
async fn an_overlapping_save_does_not_leave_the_file_behind_the_server() {
    /// Big enough that writing and syncing it takes far longer than writing the
    /// seven hundred bytes a default configuration marshals to, so the two
    /// renames are ordered by the size of the writes and not by how the runtime
    /// happened to schedule them.
    const BIG: usize = 6 * 1024 * 1024;

    let directory = TempDir::new("overlap");

    let mut large = ApiConfig::default();
    large.knowledge.openai_prompt = "a".repeat(BIG);
    let small = ApiConfig::default();

    let (first, second) = tokio::time::timeout(CEILING, async {
        tokio::join!(
            biased;
            write_config_to_disk(&large, directory.gate()),
            write_config_to_disk(&small, directory.gate()),
        )
    })
    .await
    .expect("the two saves hung");
    first.expect("the large save failed");
    second.expect("the small save failed");

    let want = small.to_json_bytes();
    assert_eq!(
        directory.file().len(),
        want.len(),
        "the file holds a configuration the server has moved past"
    );
    assert_eq!(
        directory.file(),
        want,
        "the file is not the configuration the last save carried"
    );
    assert_eq!(
        directory.entries(),
        ["apiConfig.json"],
        "a save left a temporary behind"
    );
}

// ---------------------------------------------------------------------------
// The two failure arms, and the range of the go-home percent
// ---------------------------------------------------------------------------

/// `config.go:127-128`: *both* assignments run, over whatever decoded.
///
/// Every other parse-failure case in this file happens to arrive with weather
/// already off, so the `Weather.Enable = false` line could be deleted and
/// nothing would notice. This document turns both on and then puts the type
/// error somewhere else entirely, in `server.port`, which Go declares a string
/// (`config.go:50`), so the decode fills both `enable` fields and the fault is
/// recorded after them.
#[tokio::test]
async fn the_parse_failure_arm_turns_off_a_knowledge_and_a_weather_it_found_on() {
    let directory = TempDir::new("both-off");
    let garbage = br#"{"weather":{"enable":true,"provider":"openweathermap"},"knowledge":{"enable":true,"provider":"openai"},"server":{"port":8080}}"#;
    directory.seed(garbage);

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
        .await
        .expect("read_config hung");

    match boot.outcome {
        BootOutcome::ParseFailed(DecodeFault::Type { path, found, want }) => {
            assert_eq!(path, "server.port");
            assert_eq!(found, "number");
            assert_eq!(want, "string");
        }
        other => panic!("the document took the wrong arm: {other:?}"),
    }
    // The decode filled both of these before it reached the fault, so the two
    // assignments at config.go:127-128 are the only reason they are off.
    assert!(!boot.config.knowledge.enable, "config.go:127");
    assert!(!boot.config.weather.enable, "config.go:128");
    assert_eq!(
        boot.config.weather.provider, "openweathermap",
        "the document really did decode before the fault"
    );
    assert_eq!(boot.config.knowledge.provider, "openai");
    assert_eq!(
        directory.file(),
        garbage,
        "the boot rewrote a file it could not read"
    );
}

/// The go-home percent is range checked, not truncated.
///
/// Go's `int` is 64 bits, so `encoding/json` decodes `5000000000` into
/// `*int` without complaint and stores it; that difference is candidate
/// deviation 5 and is deliberate. What must not happen is the other thing: a
/// cast would store `705032704` and report nothing, which would put a number
/// the operator never typed into `apiConfig.json` at the next rewrite. The
/// fault's shape is the one Go gives a literal that really is out of its own
/// range, which was run through `encoding/json` against a copy of
/// `config.go:52-56`'s struct: `cannot unmarshal number <literal> into ... of
/// type int`, with the pointer left allocated at zero (`decode.go:476-478`).
#[test]
fn a_go_home_percent_outside_the_field_is_a_type_error_and_not_a_truncation() {
    let error = ApiConfig::from_json_bytes(br#"{"battery":{"gohome_percent":5000000000}}"#)
        .expect_err("a value outside the field has to be reported");

    let DecodeError { config, fault } = error;
    assert_eq!(
        fault,
        DecodeFault::Type {
            path: "battery.gohome_percent".to_owned(),
            found: "number 5000000000".to_owned(),
            want: "int",
        }
    );
    assert_eq!(
        config.battery.gohome_percent,
        Some(0),
        "decode.go:476-478 allocates the pointer before it stores through it"
    );

    // The same literal one below the boundary is not out of range and is kept,
    // so the test above is about the range and not about the number of digits.
    let ok = ApiConfig::from_json_bytes(br#"{"battery":{"gohome_percent":2147483647}}"#)
        .expect("the largest value the field holds decodes");
    assert_eq!(ok.battery.gohome_percent, Some(i32::MAX));
    let error = ApiConfig::from_json_bytes(br#"{"battery":{"gohome_percent":2147483648}}"#)
        .expect_err("one past the largest value the field holds is a fault");
    assert_eq!(
        error.fault,
        DecodeFault::Type {
            path: "battery.gohome_percent".to_owned(),
            found: "number 2147483648".to_owned(),
            want: "int",
        }
    );
}

/// `config.go:105` and `:134`: an unset `STT_SERVICE` is a value like any
/// other.
///
/// Go compares the stored provider against `os.Getenv("STT_SERVICE")`, which is
/// the empty string when the shell exports nothing, so a stored `vosk` differs
/// from it and `WriteSTT` runs and empties the provider. The language is left
/// alone, because the empty string is neither of the two services
/// `config.go:106` names. Both then reach the file through the rewrite at
/// `config.go:155`.
///
/// This is the boot an operator gets by starting the server without the shell
/// wrapper, and it is worth pinning because a port that treated the empty
/// string as "no opinion" would quietly keep the old engine.
#[tokio::test]
async fn an_unset_stt_service_empties_the_provider_and_is_written_back() {
    let directory = TempDir::new("stt-empty");
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

    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), directory.gate()))
        .await
        .expect("read_config hung");

    assert!(
        boot.config.stt.provider.is_empty(),
        "config.go:105 assigns the variable whether or not it is set"
    );
    assert_eq!(
        boot.config.stt.language, "en-US",
        "config.go:106 leaves the language alone for anything but vosk and whisper.cpp"
    );
    assert_ne!(directory.file(), seeded, "config.go:155 did not rewrite");
    assert_eq!(
        directory.file(),
        boot.config.to_json_bytes(),
        "the file is not what the boot ended up holding"
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

/// The mode each write site passes reaches the file it creates.
///
/// The umask masks every mode this process asks for and there is no portable
/// way to read it, so a file written at `0o777` says what survives: `mode &
/// probe` is `mode & !umask` for any mode, which makes the expectation exact
/// without touching a process-wide setting other tests share.
///
/// Two of Go's three sites create the file and are pinned here. The third,
/// `config.go:155`, cannot be: the boot rewrite only runs on a file that
/// already exists, and `os.WriteFile` hands its mode to an `O_CREATE` open
/// (`os/file.go:849-859`) which applies it only when it creates the file, so
/// the mode at that site never reaches the disk in Go either. What is
/// observable there is that the file keeps the mode it had, which is the last
/// case below.
///
/// Unix only, because the mode is: Windows has no permission bit set for the
/// assertion to read. The `0o644`-against-`0o600` half of it is visible only
/// while the umask leaves the group and other read bits alone, which is the
/// default and is what CI runs with.
#[cfg(unix)]
#[tokio::test]
async fn every_config_write_site_passes_gos_mode() {
    use std::os::unix::fs::PermissionsExt;

    let mode_of = |path: &Path| {
        fs::metadata(path)
            .expect("the file is missing")
            .permissions()
            .mode()
            & 0o777
    };

    let directory = TempDir::new("mode-probe");
    let probe = directory.path().join("umask-probe");
    wirepod_core::persist::write_atomic(&probe, b"x".to_vec(), 0o777)
        .await
        .expect("the probe write failed");
    let want = CONFIG_FILE_MODE & mode_of(&probe);

    // `config.go:99`, through the arm that seeds a fresh file.
    let seeding = TempDir::new("mode-seed");
    let (_config, written) = create_config_from_env(&empty_env(), seeding.gate()).await;
    written.expect("the seeding write failed");
    assert_eq!(
        mode_of(&seeding.path().join("apiConfig.json")),
        want,
        "config.go:99 did not pass CONFIG_FILE_MODE"
    );

    // `config.go:64`, the save path.
    let saving = TempDir::new("mode-save");
    write_config_to_disk(&ApiConfig::default(), saving.gate())
        .await
        .expect("the save failed");
    assert_eq!(
        mode_of(&saving.path().join("apiConfig.json")),
        want,
        "config.go:64 did not pass CONFIG_FILE_MODE"
    );

    // `config.go:155`, where what is observable is that the existing mode
    // survives the replacement.
    let booting = TempDir::new("mode-boot");
    let path = booting.path().join("apiConfig.json");
    booting.seed(&ApiConfig::default().to_json_bytes());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640))
        .expect("could not set the mode the file is to keep");
    let boot = tokio::time::timeout(CEILING, read_config(&empty_env(), booting.gate()))
        .await
        .expect("read_config hung");
    assert!(matches!(boot.outcome, BootOutcome::Read(Ok(()))));
    assert_eq!(
        mode_of(&path),
        0o640,
        "the rewrite re-stamped a mode onto a file that already existed"
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
