//! The Phase 1 stores `AppState` carries, their defaults, the two ways the
//! configuration changes, and the production log clock.
//!
//! `tests/state.rs` covers what the P4 slice put in the state; this covers what
//! Phase 1 added. Nothing here touches the disk: every store is asserted by the
//! path it would write to and by the emptiness it starts in, so no test can
//! reach the repository, the live `%APPDATA%\wire-pod` the Go server is serving
//! from, or the real `~/.anki_vector`. Every path below is a fixed literal for
//! the same reason.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing_subscriber::layer::SubscriberExt;
use wirepod_core::config::config_gate;
use wirepod_core::logger::{LogClock, LogLayer, LogLevel, LogRing, ManualLogClock};
use wirepod_core::paths::{AssetDir, DEFAULT_SDK_INI_DIR, DataDir, sdk_ini_dir};
use wirepod_core::persist::WriteGate;
use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn, install_tracing_backstop};
use wirepod_core::timefmt::legacy_stamp;
use wirepod_core::wallclock::{FixedWallClock, SystemWallClock, WallClock, WallTime};
use wirepod_core::{
    ApiConfig, AppState, JdocsStore, Paths, PrimaryEntry, RecurringInfo, RobotConn,
    RobotConnFactory, SDK_INI_FILE_MODE, SdkIniStore, SessionCertStore, TokenStores, WallLogClock,
    host_of, sdk_config_path,
};

/// A robot serial: the machine's own, which every other test in this crate
/// already names.
const ESN_A: &str = "00303f28";

/// 2026-01-01T00:00:00Z, which is exactly 20454 days after the epoch, plus a
/// fraction that is not a whole millisecond.
const FIXED_SECS: i64 = 1_767_225_600;
/// 123.456789 milliseconds into the second, so a truncation that rounded or
/// that dropped the fraction entirely would show.
const FIXED_NANOS: u32 = 123_456_789;
/// Five hours behind UTC, so the stamp lands on the previous day and a clock
/// that formatted in UTC would fail every assertion below.
const FIXED_OFFSET: i32 = -5 * 3600;

fn fixed_instant() -> WallTime {
    WallTime::new(FIXED_SECS, FIXED_NANOS)
}

fn fixed_wall() -> Arc<dyn WallClock> {
    Arc::new(FixedWallClock::new(fixed_instant(), FIXED_OFFSET))
}

fn factory() -> Arc<dyn RobotConnFactory> {
    // Every state in this file is built through here, and `build` logs. The
    // backstop has to be in place before the first test thread reaches that
    // callsite, or `tracing` caches it as never interested and the warning
    // assertions below read an empty ring however they are ordered.
    install_tracing_backstop();
    let conn: Arc<dyn RobotConn> = Arc::new(FakeRobotConn::new());
    Arc::new(FakeConnFactory::connecting_to(conn))
}

/// The state every default test starts from.
fn default_state() -> Arc<AppState> {
    AppState::builder(factory()).build()
}

/// A configuration distinguishable from [`ApiConfig::default`] by one field
/// that is neither a key nor free text.
fn config_with_provider(provider: &str) -> ApiConfig {
    let mut config = ApiConfig::default();
    config.weather.provider = provider.to_owned();
    config
}

/// The instant the rings below stamp their entries with.
///
/// Anything but zero: `GetEntries` keeps an entry only when its `TimeMS` is
/// strictly greater than the `since` it was asked for (`logger.go:227`), so a
/// ring at the epoch would answer nothing to the `since` of 0 that
/// [`recorded_lines`] asks with.
const STAMPED_AT: i64 = 1_767_000_000_000;

/// A ring wired to a subscriber, for a build that logs.
fn ring() -> Arc<LogRing> {
    Arc::new(LogRing::new(Arc::new(ManualLogClock::new(
        STAMPED_AT,
        "2026.01.02 03:04:05",
    ))))
}

/// Installs `ring` as this thread's subscriber until the guard is dropped, over
/// the process-wide backstop. Copied from `tests/config.rs`, where the comment
/// on its own copy explains why both halves are load bearing: `tracing` caches
/// one interest answer per callsite for every thread, so a callsite first
/// reached by a test that installed nothing is cached as never interested and
/// every later assertion on that line sees an empty ring.
fn watching(ring: &Arc<LogRing>) -> tracing::subscriber::DefaultGuard {
    install_tracing_backstop();
    let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(ring)));
    tracing::subscriber::set_default(subscriber)
}

/// Every log line one body recorded, as `(level, comp, msg)`.
fn recorded_lines(ring: &LogRing) -> Vec<(String, String, String)> {
    ring.get_entries(LogLevel::Debug, 0)
        .into_iter()
        .map(|entry| (entry.level, entry.comp, entry.msg))
        .collect()
}

/// A [`WallClock`] that counts both of its methods, so a reading that costs two
/// clock calls is a failure rather than a subtlety.
struct CountingWallClock {
    inner: FixedWallClock,
    now_calls: AtomicUsize,
    offset_calls: AtomicUsize,
}

impl CountingWallClock {
    fn new() -> Self {
        Self {
            inner: FixedWallClock::new(fixed_instant(), FIXED_OFFSET),
            now_calls: AtomicUsize::new(0),
            offset_calls: AtomicUsize::new(0),
        }
    }

    fn now_calls(&self) -> usize {
        self.now_calls.load(Ordering::SeqCst)
    }

    fn offset_calls(&self) -> usize {
        self.offset_calls.load(Ordering::SeqCst)
    }
}

impl WallClock for CountingWallClock {
    fn now(&self) -> WallTime {
        self.now_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.now()
    }

    fn utc_offset_secs_at(&self, unix_secs: i64) -> i32 {
        self.offset_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.utc_offset_secs_at(unix_secs)
    }
}

// ---------------------------------------------------------------------------
// The defaults
// ---------------------------------------------------------------------------

#[test]
fn a_default_state_carries_every_phase_one_store_empty() {
    let state = default_state();

    // Go's un-packaged layout (`vars.go:35-55`), never the packaged one.
    assert_eq!(state.paths().data(), &DataDir::source());
    assert_eq!(state.paths().assets().root(), Path::new("."));

    assert_eq!(*state.config(), ApiConfig::default());
    assert_eq!(
        state.config_gate().path(),
        "./apiConfig.json",
        "the configuration gate left Go's source-layout literal"
    );

    assert!(state.jdocs().snapshot().is_empty());
    assert_eq!(
        state.jdocs().path(),
        "./jdocs/jdocs.json",
        "the jdocs store left Go's source-layout literal"
    );

    assert!(state.session_certs().is_empty());

    assert_eq!(
        state.sdk_ini().dir(),
        "./.anki_vector/",
        "the SDK ini store defaulted somewhere other than the working directory"
    );
    assert_eq!(state.sdk_ini().path(), "./.anki_vector/sdk_config.ini");

    assert_eq!(state.tokens().primary_len(), 0);
    assert_eq!(state.tokens().secondary_len(), 0);
    assert_eq!(state.tokens().session_len(), 0);

    assert!(state.logs().is_empty());
    assert_eq!(state.logs().tray_len(), 0);
    assert_eq!(state.logs().info_len(), 0);
}

/// The default SDK ini directory is relative on purpose: Go resolves it from
/// the user's home directory on Windows and macOS (`vars.go:208-209`), and
/// defaulting there would have an unconfigured test writing into the real
/// `~/.anki_vector/`.
#[test]
fn the_default_sdk_ini_directory_is_not_the_home_directory() {
    let state = default_state();
    assert_eq!(state.sdk_ini().dir(), sdk_ini_dir(Path::new(".")));
    assert_eq!(
        DEFAULT_SDK_INI_DIR,
        sdk_ini_dir(Path::new(".")),
        "the named fallback and the working-directory spelling have drifted apart"
    );
    assert_eq!(state.sdk_ini().dir(), DEFAULT_SDK_INI_DIR);
}

/// Deviation 38 is that the port resolves every path explicitly and says what
/// it resolved rather than guessing one from the working directory. A state
/// that falls back to [`DEFAULT_SDK_INI_DIR`] resolved nothing, so the build
/// says so, at `WARN` and with no component, as a line Go has no counterpart
/// for.
#[test]
fn a_defaulted_sdk_ini_directory_is_warned_about() {
    let ring = ring();
    let state = {
        let _guard = watching(&ring);
        default_state()
    };

    assert_eq!(state.sdk_ini().dir(), DEFAULT_SDK_INI_DIR);
    assert_eq!(
        recorded_lines(&ring),
        [(
            "WARN".to_owned(),
            String::new(),
            format!("SDK ini directory was not resolved; falling back to {DEFAULT_SDK_INI_DIR}")
        )],
        "a build that fell back to the working directory said nothing about it"
    );
}

/// The other half: the boot path resolves the directory, so the warning is a
/// signal rather than a line printed at every start.
#[test]
fn a_resolved_sdk_ini_directory_is_not_warned_about() {
    let ring = ring();
    let state = {
        let _guard = watching(&ring);
        AppState::builder(factory())
            .sdk_ini(SdkIniStore::new("/fixed/home/.anki_vector/"))
            .build()
    };

    assert_eq!(state.sdk_ini().dir(), "/fixed/home/.anki_vector/");
    assert_eq!(
        recorded_lines(&ring),
        [],
        "a build handed an SDK ini directory warned about it anyway"
    );
}

/// The three stores whose identity is a path are built from whatever
/// [`Paths`] the builder ended up with, whether or not it was set before them.
#[test]
fn the_defaulted_stores_follow_the_builders_paths() {
    let data = DataDir::rooted("/fixed/pod");
    let paths = Paths::new(data.clone(), AssetDir::new("/fixed/assets"));
    let state = AppState::builder(factory()).paths(paths).build();

    assert_eq!(state.config_gate().path(), config_gate(&data).path());
    assert_eq!(state.jdocs().path(), data.jdocs_path());
    assert_eq!(state.paths().assets().root(), Path::new("/fixed/assets"));
}

#[test]
fn the_config_gate_is_the_one_the_config_module_builds() {
    let data = DataDir::rooted("/fixed/pod");
    let state = AppState::builder(factory())
        .paths(Paths::new(data.clone(), AssetDir::new(".")))
        .build();
    let expected = config_gate(&data);

    assert_eq!(state.config_gate().path(), expected.path());
    assert_eq!(state.config_gate().mode(), expected.mode());
    assert_eq!(
        state.config_gate().temporary_in(),
        expected.temporary_in(),
        "the gate puts its temporary somewhere the config module's does not"
    );
    assert_eq!(
        state.config_gate().path(),
        data.api_config_path().to_string_lossy(),
        "the gate names a different file from the one the data directory does"
    );
}

// ---------------------------------------------------------------------------
// The setters
// ---------------------------------------------------------------------------

#[test]
fn the_builder_setters_replace_each_default() {
    let data = DataDir::rooted("/fixed/pod");
    let ring = Arc::new(LogRing::new(Arc::new(ManualLogClock::new(
        7,
        "2026.01.02 03:04:05",
    ))));
    ring.record(LogLevel::Info, "", "", "a line the state should see");

    let tokens = TokenStores::new();
    tokens.add_primary(PrimaryEntry {
        target: host_of("192.168.8.203:443").to_owned(),
        guid: "<guid>".to_owned(),
        guid_hash: "<hash>".to_owned(),
    });

    let state = AppState::builder(factory())
        .paths(Paths::new(data.clone(), AssetDir::new("/fixed/assets")))
        .config(config_with_provider("openweathermap"))
        // The gate has to name the file the paths name, because `build`
        // debug-asserts that it does, so what distinguishes this one from the
        // gate the builder would have made is its mode.
        .config_gate(WriteGate::new(
            data.api_config_path().to_string_lossy().into_owned(),
            0o600,
        ))
        .jdocs(JdocsStore::new("/fixed/elsewhere/jdocs.json"))
        .session_certs(SessionCertStore::with_info(vec![RecurringInfo {
            id: "Vector-R2D2".to_owned(),
            esn: ESN_A.to_owned(),
            ip: "192.168.8.203".to_owned(),
        }]))
        .sdk_ini(SdkIniStore::new("/fixed/home/.anki_vector/"))
        .tokens(tokens)
        .logs(Arc::clone(&ring))
        .wall(fixed_wall())
        .build();

    assert_eq!(state.paths().data(), &DataDir::rooted("/fixed/pod"));
    assert_eq!(state.paths().assets().root(), Path::new("/fixed/assets"));
    assert_eq!(state.config().weather.provider, "openweathermap");
    assert_eq!(
        state.config_gate().path(),
        data.api_config_path().to_string_lossy()
    );
    assert_eq!(
        state.config_gate().mode(),
        0o600,
        "the builder ignored the configuration gate it was handed"
    );
    assert_eq!(state.jdocs().path(), "/fixed/elsewhere/jdocs.json");
    assert_eq!(state.session_certs().len(), 1);
    assert_eq!(state.session_certs().snapshot()[0].esn, ESN_A);
    assert_eq!(state.sdk_ini().dir(), "/fixed/home/.anki_vector/");
    assert_eq!(
        state.sdk_ini().path(),
        sdk_config_path("/fixed/home/.anki_vector/")
    );
    assert_eq!(
        state.sdk_ini().path(),
        "/fixed/home/.anki_vector/sdk_config.ini"
    );
    assert_eq!(state.tokens().primary_len(), 1);
    assert_eq!(state.logs().len(), 1, "the builder built a ring of its own");
    assert!(
        Arc::ptr_eq(state.logs(), &ring),
        "the state holds a different ring from the one the tracing layer fills"
    );
    assert_eq!(state.wall().now(), fixed_instant());
    assert_eq!(state.wall().utc_offset_secs_at(FIXED_SECS), FIXED_OFFSET);
}

/// The SDK ini store carries the mode Go's `SaveTo` hands `os.WriteFile`
/// whichever directory it was pointed at, so a setter cannot quietly change it.
#[test]
fn the_sdk_ini_setter_keeps_gos_file_mode() {
    let state = AppState::builder(factory())
        .sdk_ini(SdkIniStore::new("/fixed/home/.anki_vector/"))
        .build();
    assert_eq!(SDK_INI_FILE_MODE, 0o666);
    assert_eq!(
        state.sdk_ini().path(),
        "/fixed/home/.anki_vector/sdk_config.ini"
    );
}

// ---------------------------------------------------------------------------
// The configuration pointer
// ---------------------------------------------------------------------------

#[test]
fn replacing_the_config_swaps_the_arc_and_old_snapshots_survive() {
    let state = AppState::builder(factory())
        .config(config_with_provider("openweathermap"))
        .build();

    let before = state.config();
    assert_eq!(before.weather.provider, "openweathermap");

    state.replace_config(config_with_provider("weatherapi"));

    let after = state.config();
    assert_eq!(
        after.weather.provider, "weatherapi",
        "the replacement kept the old configuration"
    );
    assert_eq!(
        before.weather.provider, "openweathermap",
        "a handler part way through a request had its configuration changed underneath it"
    );
    assert!(
        !Arc::ptr_eq(&before, &after),
        "the replacement left the same pointer in place"
    );

    let again = state.config();
    assert!(
        Arc::ptr_eq(&after, &again),
        "two reads with no write between them answered two different documents"
    );
}

/// Go's web UI writers each own a few fields of the one shared struct and
/// mutate them in place before flushing (`config-ws/webserver.go:196-202`,
/// `:212-217`, `:253-255`), so two of them running at once cannot drop each
/// other's fields. Two field edits here must not either, which is why
/// `update_config` clones, edits and stores under one hold of the write lock
/// instead of reading a pointer, editing a clone and replacing.
///
/// Each thread appends to a field of its own and the count is the assertion, so
/// a lost update is a shortfall rather than a coin toss about which write landed
/// last. The yield inside the edit is what makes the shortfall certain rather
/// than likely: under a read-clone-edit-replace implementation it sits in the
/// window between the read and the write, and under this one it merely holds the
/// guard a moment longer.
#[test]
fn two_disjoint_field_updates_both_survive() {
    /// Enough rounds that a lost update is a certainty rather than a race the
    /// scheduler might decline to run, and few enough to finish in milliseconds.
    const ROUNDS: usize = 300;

    let state = AppState::builder(factory()).build();

    let weather_writer = Arc::clone(&state);
    let weather = std::thread::spawn(move || {
        for _ in 0..ROUNDS {
            weather_writer.update_config(|config| {
                config.weather.unit.push('w');
                std::thread::yield_now();
            });
        }
    });

    let stt_writer = Arc::clone(&state);
    let stt = std::thread::spawn(move || {
        for _ in 0..ROUNDS {
            stt_writer.update_config(|config| {
                config.stt.language.push('s');
                std::thread::yield_now();
            });
        }
    });

    weather.join().expect("the weather writer panicked");
    stt.join().expect("the STT writer panicked");

    let config = state.config();
    assert_eq!(
        config.weather.unit.len(),
        ROUNDS,
        "the weather writer's edits were overwritten by the other writer's"
    );
    assert_eq!(
        config.stt.language.len(),
        ROUNDS,
        "the STT writer's edits were overwritten by the other writer's"
    );
    assert_eq!(
        config.weather.provider,
        ApiConfig::default().weather.provider,
        "an edit reached a field neither writer named"
    );
}

/// An edit sees what the edit before it published, which is what makes the two
/// writers above additive rather than merely both present at the end.
#[test]
fn an_update_starts_from_the_document_the_last_one_published() {
    let state = AppState::builder(factory())
        .config(config_with_provider("openweathermap"))
        .build();

    state.update_config(|config| config.stt.language.push_str("en-US"));

    let after = state.config();
    assert_eq!(after.stt.language, "en-US");
    assert_eq!(
        after.weather.provider, "openweathermap",
        "the edit dropped a field it never named"
    );

    let before = Arc::clone(&after);
    state.update_config(|config| config.stt.language.push_str("-2"));

    assert_eq!(state.config().stt.language, "en-US-2");
    assert_eq!(
        before.stt.language, "en-US",
        "a handler part way through a request had its configuration changed underneath it"
    );
}

// ---------------------------------------------------------------------------
// The wall clock
// ---------------------------------------------------------------------------

#[test]
fn the_wall_clock_defaults_to_the_system_clock_and_can_be_fixed() {
    let state = default_state();
    let system = SystemWallClock::new().now();
    let read = state.wall().now();

    assert!(
        read.unix_secs > 1_700_000_000,
        "the default wall clock is not reading the system clock"
    );
    assert!(
        (read.unix_secs - system.unix_secs).abs() <= 5,
        "the default wall clock is stopped somewhere other than now"
    );
    assert_eq!(
        state.wall().utc_offset_secs_at(read.unix_secs),
        SystemWallClock::new().utc_offset_secs_at(read.unix_secs),
        "the default wall clock is not in the system's zone"
    );

    let fixed = AppState::builder(factory()).wall(fixed_wall()).build();
    assert_eq!(fixed.wall().now(), fixed_instant());
    assert_eq!(fixed.wall().utc_offset_secs_at(FIXED_SECS), FIXED_OFFSET);
}

// ---------------------------------------------------------------------------
// The production log clock
// ---------------------------------------------------------------------------

#[test]
fn the_log_clock_spends_one_reading_on_both_fields() {
    let clock = WallLogClock::new(fixed_wall());
    let reading = clock.now();

    assert_eq!(
        reading.unix_millis,
        FIXED_SECS * 1_000 + 123,
        "the millisecond field is not Go's UnixMilli of the reading"
    );
    assert_eq!(reading.stamp, legacy_stamp(fixed_instant(), FIXED_OFFSET));
    assert_eq!(
        reading.stamp, "2025.12.31 19:00:00",
        "the stamp is not the reading formatted at the reading's own offset"
    );
}

/// Go's `UnixMilli` is `t.unixSec()*1e3 + int64(t.nsec())/1e6` (the Go standard
/// library's `time/time.go:1434-1436`) over a nanosecond field that is always in
/// `[0, 1e9)`, so an instant half a millisecond before the epoch is
/// `-1000 + 999`, which is `-1`. Truncating the instant as a whole toward zero
/// would answer `0`, and nothing else in this file would notice.
#[test]
fn a_reading_before_the_epoch_floors_its_millisecond_field() {
    let before_epoch = WallTime::new(-1, 999_500_000);
    let clock = WallLogClock::new(Arc::new(FixedWallClock::new(before_epoch, 0)));

    assert_eq!(
        clock.now().unix_millis,
        -1,
        "a pre-epoch reading was truncated toward zero rather than following Go's UnixMilli"
    );
}

#[test]
fn the_log_clock_reads_the_wall_clock_once_per_line() {
    let counting = Arc::new(CountingWallClock::new());
    let clock = WallLogClock::new(Arc::clone(&counting) as Arc<dyn WallClock>);

    let first = clock.now();
    assert_eq!(
        counting.now_calls(),
        1,
        "one line cost more than one clock reading, so its stamp and its millisecond field can disagree"
    );
    assert_eq!(counting.offset_calls(), 1);

    let second = clock.now();
    assert_eq!(counting.now_calls(), 2);
    assert_eq!(first, second);
}

/// The ring a default build creates is wired to the state's own wall clock, so
/// a test that fixes the clock fixes the stamps the web UI would show.
#[test]
fn the_default_log_ring_stamps_from_the_states_wall_clock() {
    let state = AppState::builder(factory()).wall(fixed_wall()).build();
    state
        .logs()
        .record(LogLevel::Info, "", ESN_A, "the ring took the state's clock");

    let entries = state.logs().get_entries(LogLevel::Debug, 0);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].time_ms, FIXED_SECS * 1_000 + 123);
    assert_eq!(
        state.logs().tray_text(),
        format!("2025.12.31 19:00:00: {ESN_A}: the ring took the state's clock\n")
    );
}
