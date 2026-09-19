//! The Phase 1 stores `AppState` carries, their defaults, and the production
//! log clock.
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

use wirepod_core::config::config_gate;
use wirepod_core::logger::{LogClock, LogLevel, LogRing, ManualLogClock};
use wirepod_core::paths::{AssetDir, DataDir, sdk_ini_dir};
use wirepod_core::persist::WriteGate;
use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn};
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
/// the user's home directory (`vars.go:208`), and defaulting there would have
/// an unconfigured test writing into the real `~/.anki_vector/`.
#[test]
fn the_default_sdk_ini_directory_is_not_the_home_directory() {
    let state = default_state();
    assert_eq!(state.sdk_ini().dir(), sdk_ini_dir(Path::new(".")));
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
        .paths(Paths::new(data, AssetDir::new("/fixed/assets")))
        .config(config_with_provider("openweathermap"))
        .config_gate(WriteGate::new("/fixed/elsewhere/apiConfig.json", 0o600))
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
        "/fixed/elsewhere/apiConfig.json",
        "the builder ignored the configuration gate it was handed"
    );
    assert_eq!(state.config_gate().mode(), 0o600);
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
