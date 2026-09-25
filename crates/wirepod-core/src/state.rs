//! The shared server state the SDK-app handlers read.
//!
//! Go keeps this as roughly thirty unsynchronized package-level globals spread
//! across `vars`, `logger`, `sdkapp`, `config-ws` and the token server. This is
//! the same state as one value shared as an `Arc`, and every field below names
//! the global it stands in for. What is still missing is the server-config
//! store, which a later commit brings; nothing else Phase 1 landed is outside
//! this struct.
//!
//! The bot-info store is a [`std::sync::RwLock`], and the only way to read it
//! across an `.await` is [`AppState::bot_info_snapshot`], which clones. That is
//! deliberate: the crate's `deny(clippy::await_holding_lock)` does not follow a
//! guard handed out to `wirepod-server`, so no guard is handed out.
//!
//! The configuration is an [`Arc`] *inside* the lock rather than a value inside
//! one, for the same reason. A reader clones the `Arc` and drops the guard in
//! one expression, so a handler can hold a whole configuration across an
//! `.await` without holding the lock; a rewrite replaces the pointer and the
//! readers that already cloned keep the document they started with, which is
//! the one thing Go's shared global cannot offer.
//!
//! What that costs is what [`AppState::update_config`] exists to pay back. Go's
//! web UI writers reach into one shared struct, change the few fields each of
//! them owns and flush it (`config-ws/webserver.go:196-202`, `:212-217`,
//! `:253-255`). Nothing orders them, but because they mutate in place they
//! cannot drop one another's fields. A clone, an edit and a
//! [`AppState::replace_config`] can: two handlers that started from the same
//! pointer each publish a document missing the other's edit, and the later
//! write wins whole. So a field edit is one call that clones, edits and stores
//! under a single hold of the write lock, and [`AppState::replace_config`] is
//! left to the boot path, which replaces the whole document and has no
//! concurrent writer to lose.
//!
//! Nothing here derives [`Debug`], and that guards exactly one thing: a `{:?}`
//! of the whole state. It is not a redaction. [`ApiConfig`] derives a full
//! [`Debug`] of its own (`config.rs:264`), and so do [`BotInfo`] and its rows
//! (`store/bot_info.rs:30`, `:47`), so anything that reaches through an
//! accessor and formats what it gets back still prints the operator's provider
//! keys and the robots' GUIDs. The stores that genuinely redact say so in a
//! hand-written `Debug` (see [`crate::token::stores::TokenStores`]). Giving
//! [`ApiConfig`] a redacting `Debug` is the real fix and belongs to a later
//! commit, which will own `config.rs`.

use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use crate::clock::{Clock, SystemClock};
use crate::config::ApiConfig;
use crate::esn::Esn;
use crate::intents::CustomIntent;
use crate::logger::{LogClock, LogInstant, LogRing};
use crate::paths::{AssetDir, DEFAULT_SDK_INI_DIR, DataDir};
use crate::persist::WriteGate;
use crate::robot::conn::RobotConnFactory;
use crate::robot::registry::{GetRobotError, RobotEntry, RobotRegistry};
use crate::store::bot_info::BotInfo;
use crate::store::bot_status::PingerState;
use crate::store::jdocs::JdocsStore;
use crate::store::sdk_ini::SdkIniStore;
use crate::store::session_certs::SessionCertStore;
use crate::timefmt::legacy_stamp;
use crate::timings::Timings;
use crate::token::stores::TokenStores;
use crate::wallclock::{SystemWallClock, WallClock};

/// Where the server's files live, resolved once and passed down.
///
/// Go's counterpart is the block of path globals at `vars.go:35-55` plus
/// `ApiConfigPath` at `config.go:13`, which `vars.Init` rewrites in place when
/// the build is packaged (`vars.go:158-188`). Every later reader takes the
/// global, so in Go the layout is decided by a build tag and a startup
/// side effect. Here it is a value, which is what lets a test point a whole
/// server at a temporary directory.
///
/// [`AssetDir`] has no Go global at all, for the reason
/// [`crate::paths::AssetDir`] gives: Go reads those files through literals
/// relative to the working directory and the packaged wrapper changes
/// directory before starting the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    data: DataDir,
    assets: AssetDir,
}

impl Paths {
    /// The state directory and the asset directory the operator resolved.
    pub fn new(data: DataDir, assets: AssetDir) -> Self {
        Self { data, assets }
    }

    /// The directory the server's mutable state lives in.
    pub fn data(&self) -> &DataDir {
        &self.data
    }

    /// The directory the files that ship with the server live in.
    pub fn assets(&self) -> &AssetDir {
        &self.assets
    }
}

impl Default for Paths {
    /// Go's un-packaged layout: the relative literals of `vars.go:35-55`
    /// resolved against the working directory, and the working directory
    /// itself for the assets.
    ///
    /// This is the branch `vars.Init` leaves alone. The packaged branch roots
    /// every path at `os.UserConfigDir()` joined with `PodName`
    /// (`vars.go:166`), which on this machine is `%APPDATA%\wire-pod` and is
    /// where the live server's state is, so it is emphatically not a default:
    /// a builder that defaulted there would have a test writing over the
    /// running server's files. [`DataDir::packaged`] is that layout and the
    /// boot path names it explicitly.
    fn default() -> Self {
        Self::new(DataDir::source(), AssetDir::new("."))
    }
}

/// The production [`LogClock`]: Go's `time.Now()` at the top of `logf`.
///
/// Go reads the clock once per line (`logger.go:138`) and spends that one
/// reading twice, as `UnixMilli` for the JSON entry (`logger.go:141`) and
/// formatted with the `2006.01.02 15:04:05` layout for the two text lines
/// (`logger.go:103`, `:114`). This is that, over the crate's wall-clock seam:
/// one [`WallClock::now`], one offset lookup at that instant, and both fields
/// built from them. Reading twice is the bug this type exists to make
/// impossible, because a line whose stamp and millisecond field straddle a
/// second would show the web UI one time and the poller another.
///
/// The offset is resolved at the instant rather than read once at startup, for
/// the reason [`crate::wallclock`] gives: a server that runs across a daylight
/// saving transition keeps stamping lines, and Go's `time.Now()` is in
/// `time.Local`, so the stamp follows the transition.
pub struct WallLogClock {
    wall: Arc<dyn WallClock>,
}

impl WallLogClock {
    /// A log clock reading `wall`.
    pub fn new(wall: Arc<dyn WallClock>) -> Self {
        Self { wall }
    }
}

impl LogClock for WallLogClock {
    fn now(&self) -> LogInstant {
        // One reading, as `logger.go:138`. Everything below is derived from it.
        let at = self.wall.now();
        let offset = self.wall.utc_offset_secs_at(at.unix_secs);
        LogInstant {
            // Go's `UnixMilli` (`logger.go:141`), which is
            // `t.unixSec()*1e3 + int64(t.nsec())/1e6` in the Go standard
            // library (`time/time.go:1434-1436`): seconds times a thousand
            // plus the truncated fraction. [`crate::wallclock::WallTime`] keeps
            // its nanosecond field in `[0, 1e9)` however far the seconds are
            // from the epoch, exactly as `time.Time` does, so this is Go's
            // arithmetic and not an approximation of it. Before the epoch the
            // seconds carry the whole negative part and the fraction is added
            // back, so half a millisecond before the epoch is `-1000 + 999`,
            // which is `-1` and not the `0` that truncating the instant as a
            // whole toward zero would give. The saturation covers
            // a clock set roughly three hundred million years out, where Go
            // would wrap; nothing observes the difference and a panic inside
            // the logger would be worse than either.
            unix_millis: at
                .unix_secs
                .saturating_mul(1_000)
                .saturating_add(i64::from(at.nanos / 1_000_000)),
            stamp: legacy_stamp(at, offset),
        }
    }
}

/// Everything a handler needs, shared as an `Arc`.
pub struct AppState {
    bot_info: RwLock<BotInfo>,
    pinger: PingerState,
    registry: RobotRegistry,
    timings: Timings,
    clock: Arc<dyn Clock>,
    /// The path globals of `vars.go:35-55` and `config.go:13`, resolved.
    paths: Paths,
    /// Go's `vars.APIConfig` (`config.go:15`).
    config: RwLock<Arc<ApiConfig>>,
    /// No Go counterpart: its three writers call `os.WriteFile(ApiConfigPath,
    /// ...)` directly (`config.go:64`, `:99`, `:155`) and are ordered by
    /// nothing. The gate is what orders them here, and it only does so while
    /// they all hold this one, which is why it is state rather than something
    /// a writer builds.
    config_gate: WriteGate,
    /// Go's `vars.CustomIntents` and `vars.CustomIntentsExist` (`vars.go:63`,
    /// `:67`) as one value: `None` is `CustomIntentsExist == false`.
    custom_intents: Mutex<Option<Vec<CustomIntent>>>,
    /// Go's `vars.BotJdocs` (`vars.go:61`).
    jdocs: JdocsStore,
    /// Go's `vars.RecurringInfo` (`vars.go:80`).
    session_certs: SessionCertStore,
    /// Go's `vars.SDKIniPath` (`vars.go:60`). Go keeps the directory and
    /// nothing else, because each of its three writers reloads the file; the
    /// store keeps the directory, the gate and the turn that stops two of those
    /// load-edit-save cycles from interleaving.
    sdk_ini: SdkIniStore,
    /// Go's `TokenHashStore` (`token/token.go:37`), `SecondaryTokenStore`
    /// (`:41`), `SessionWriteStoreNames` (`:44`) and `SessionWriteStoreCerts`
    /// (`:45`), the last two of which are one list here.
    tokens: TokenStores,
    /// Go's whole `logger` package state (`logger.go:57-77`).
    logs: Arc<LogRing>,
    /// No Go global: Go calls `time.Now()` at each site. The seam is here so
    /// the token server's claims and the log ring's stamps can both be frozen
    /// in a test.
    wall: Arc<dyn WallClock>,
}

impl AppState {
    /// A builder dialling robots through `factory`.
    pub fn builder(factory: Arc<dyn RobotConnFactory>) -> AppStateBuilder {
        AppStateBuilder::new(factory)
    }

    /// Reads the bot-info file under its lock.
    ///
    /// For the synchronous readers: `/api/get_bot_status` and
    /// `/api-sdk/get_sdk_info` both project it and answer without awaiting
    /// anything.
    pub fn with_bot_info<R>(&self, read: impl FnOnce(&BotInfo) -> R) -> R {
        read(&self.bot_info.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// A copy of the bot-info file, for a caller that has to hold it across an
    /// `.await`.
    ///
    /// The connect preamble is that caller: it resolves a serial against the
    /// file and then dials. The file holds one entry per robot ever
    /// authenticated against this server, so the copy is cheap next to the
    /// dial it precedes.
    pub fn bot_info_snapshot(&self) -> BotInfo {
        self.with_bot_info(Clone::clone)
    }

    /// Replaces the bot-info file, which the jdocs and token servers do as
    /// robots authenticate.
    pub fn set_bot_info(&self, info: BotInfo) {
        *self
            .bot_info
            .write()
            .unwrap_or_else(PoisonError::into_inner) = info;
    }

    /// The cached robot, dialling it first if it is not connected yet.
    ///
    /// This is Go's `getRobot` as the preamble calls it (`server.go:57`),
    /// resolved against the current bot-info file. It lives here rather than
    /// on the handler so that the snapshot-then-dial discipline is written once
    /// inside the crate that denies holding a lock across an await.
    pub async fn get_robot(&self, esn: &Esn) -> Result<Arc<RobotEntry>, GetRobotError> {
        let bot_info = self.bot_info_snapshot();
        self.registry.get_or_connect(esn, &bot_info).await
    }

    /// The jdocs pinger's record of which robots have checked in.
    pub fn pinger(&self) -> &PingerState {
        &self.pinger
    }

    /// Every connected robot.
    pub fn registry(&self) -> &RobotRegistry {
        &self.registry
    }

    /// The durations the handlers wait on.
    pub fn timings(&self) -> &Timings {
        &self.timings
    }

    /// The clock the pinger and the idle rule measure against.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// Where every file the server reads or writes lives.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The configuration as it stands, as a pointer the caller keeps.
    ///
    /// The lock is taken and released inside this call, so the returned
    /// document can be held across an `.await` and read as many times as the
    /// handler likes. It is a snapshot: an [`AppState::update_config`] or a
    /// [`AppState::replace_config`] that lands afterwards is invisible to it,
    /// which is what a handler part way through answering a request wants, and
    /// what Go's shared global denies it.
    pub fn config(&self) -> Arc<ApiConfig> {
        Arc::clone(&self.config.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Changes some of the configuration's fields and publishes the result,
    /// under one hold of the write lock.
    ///
    /// This is the call a handler that owns a few fields makes. Go's web UI
    /// writers own a few each and reach into the one shared struct to set them:
    /// the weather handler writes three fields and flushes
    /// (`config-ws/webserver.go:196-202`), the knowledge handler decodes
    /// straight into its own sub-struct (`:212-217`), and the STT handler sets
    /// the language and the setup flag (`:253-255`). Nothing orders those three
    /// in Go, and nothing needs to: mutating one struct in place cannot drop a
    /// field somebody else just set.
    ///
    /// Reading the pointer, cloning it, editing the clone and calling
    /// [`AppState::replace_config`] would drop one. Two handlers that read the
    /// same pointer would each publish a document carrying their own edit and
    /// missing the other's, and whichever wrote second would win the whole
    /// document. Taking the write guard first is what closes that window: the
    /// clone `edit` receives is made under the guard, so it already carries
    /// every edit published before it, and no other writer can be between its
    /// own clone and its own store.
    ///
    /// `edit` therefore runs with the write lock held. It must not touch this
    /// state again — [`AppState::config`] would deadlock — and it cannot await,
    /// because it is a plain closure rather than a future, which is also why
    /// the crate's `deny(clippy::await_holding_lock)` has nothing to say about
    /// it. Readers are unaffected either way: one that already cloned the
    /// pointer keeps the document it started with, exactly as after a replace.
    ///
    /// Like [`AppState::replace_config`], this is memory only; the file is
    /// written through [`AppState::config_gate`].
    pub fn update_config(&self, edit: impl FnOnce(&mut ApiConfig)) {
        let mut current = self.config.write().unwrap_or_else(PoisonError::into_inner);
        let mut next = (**current).clone();
        edit(&mut next);
        *current = Arc::new(next);
    }

    /// Replaces the whole configuration document, which is what the boot path
    /// does once it has read or created the file.
    ///
    /// Whole-document replacement, not a field edit: whatever the state held is
    /// gone, so a caller that means to change some fields and keep the rest
    /// calls [`AppState::update_config`] instead. The boot path is the caller
    /// with nothing to lose, because it runs before any handler is serving and
    /// the document it installs is the file it just parsed.
    ///
    /// This is memory only. The file is written through
    /// [`AppState::config_gate`], and the two are separate calls because Go's
    /// are: `WriteConfigToDisk` writes whatever the global already holds
    /// (`config.go:61-65`), so a caller sets the global and then writes.
    pub fn replace_config(&self, config: ApiConfig) {
        *self.config.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(config);
    }

    /// The one gate `apiConfig.json`'s writers share.
    ///
    /// Two gates over one file order nothing, so every writer takes this one.
    pub fn config_gate(&self) -> &WriteGate {
        &self.config_gate
    }

    /// The custom intents the web UI edits. `None` is Go's
    /// `CustomIntentsExist == false`.
    pub fn custom_intents(&self) -> &Mutex<Option<Vec<CustomIntent>>> {
        &self.custom_intents
    }

    /// The jdocs file.
    pub fn jdocs(&self) -> &JdocsStore {
        &self.jdocs
    }

    /// The robots with a session certificate on disk.
    pub fn session_certs(&self) -> &SessionCertStore {
        &self.session_certs
    }

    /// The SDK's own `sdk_config.ini`.
    pub fn sdk_ini(&self) -> &SdkIniStore {
        &self.sdk_ini
    }

    /// The token server's three transient stores.
    pub fn tokens(&self) -> &TokenStores {
        &self.tokens
    }

    /// The log ring the web UI's three handlers read.
    ///
    /// An [`Arc`], because the `tracing` layer that fills it holds one too
    /// ([`crate::logger::LogLayer::new`]): the binary builds the ring, installs
    /// the layer with a clone and hands another clone here, so the lines the
    /// layer records are the lines this reads.
    pub fn logs(&self) -> &Arc<LogRing> {
        &self.logs
    }

    /// The calendar clock the token server's claims and the log ring's stamps
    /// are read from.
    pub fn wall(&self) -> &Arc<dyn WallClock> {
        &self.wall
    }
}

/// Builds an [`AppState`].
///
/// The connection factory is the one thing with no sensible default, because it
/// decides whether the server talks to a real robot. Everything else defaults:
/// [`Timings::default`], a [`SystemClock`], a [`SystemWallClock`], an empty
/// [`BotInfo`], no liveness deadline, no state stream, the zero [`ApiConfig`],
/// an empty store per file and an empty log ring, with every path rooted at the
/// working directory the way an un-packaged Go build leaves it
/// ([`Paths::default`]).
///
/// The three stores whose identity is a path are resolved in
/// [`AppStateBuilder::build`] rather than in [`AppStateBuilder::new`], so that
/// [`AppStateBuilder::paths`] can be called in any order and the defaults still
/// follow it.
pub struct AppStateBuilder {
    factory: Arc<dyn RobotConnFactory>,
    bot_info: BotInfo,
    timings: Timings,
    clock: Option<Arc<dyn Clock>>,
    liveness_deadline: Option<Duration>,
    state_stream: bool,
    paths: Paths,
    config: ApiConfig,
    config_gate: Option<WriteGate>,
    jdocs: Option<JdocsStore>,
    session_certs: SessionCertStore,
    sdk_ini: Option<SdkIniStore>,
    tokens: TokenStores,
    logs: Option<Arc<LogRing>>,
    wall: Option<Arc<dyn WallClock>>,
}

impl AppStateBuilder {
    /// A builder dialling robots through `factory`.
    pub fn new(factory: Arc<dyn RobotConnFactory>) -> Self {
        Self {
            factory,
            bot_info: BotInfo::default(),
            timings: Timings::default(),
            clock: None,
            liveness_deadline: None,
            state_stream: false,
            paths: Paths::default(),
            config: ApiConfig::default(),
            config_gate: None,
            jdocs: None,
            session_certs: SessionCertStore::new(),
            sdk_ini: None,
            tokens: TokenStores::new(),
            logs: None,
            wall: None,
        }
    }

    /// Starts from a loaded bot-info file rather than an empty one.
    pub fn bot_info(mut self, bot_info: BotInfo) -> Self {
        self.bot_info = bot_info;
        self
    }

    /// Waits on `timings` rather than the Go defaults.
    pub fn timings(mut self, timings: Timings) -> Self {
        self.timings = timings;
        self
    }

    /// Measures elapsed time against `clock` rather than the system one.
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Bounds the connect-time liveness call. `None`, the default,
    /// reproduces Go's undeadlined `BatteryState` (`robot.go:365`).
    pub fn liveness_deadline(mut self, deadline: Option<Duration>) -> Self {
        self.liveness_deadline = deadline;
        self
    }

    /// Opens the connect-time `robot_state` stream for every new connection.
    /// Off by default; the server turns it on.
    pub fn state_stream(mut self, on: bool) -> Self {
        self.state_stream = on;
        self
    }

    /// Resolves every file under `paths` rather than under the working
    /// directory.
    ///
    /// This also decides where the three defaulted stores below point, so a
    /// builder that names the paths and nothing else gets a configuration gate,
    /// a jdocs store and an ini store rooted there.
    pub fn paths(mut self, paths: Paths) -> Self {
        self.paths = paths;
        self
    }

    /// Starts from a loaded configuration rather than the zero one.
    pub fn config(mut self, config: ApiConfig) -> Self {
        self.config = config;
        self
    }

    /// Writes `apiConfig.json` through `gate` rather than through the one
    /// [`crate::config::config_gate`] builds for the data directory.
    ///
    /// The only legitimate override is a test pointing the whole state at a
    /// temporary directory, and such a test hands a gate naming the same file
    /// [`AppStateBuilder::paths`] does: [`AppStateBuilder::build`]
    /// debug-asserts that the two agree, because a gate over one file beside a
    /// data directory naming another orders nothing between them. What the
    /// override is for is the rest of the gate — the mode and the temporary's
    /// placement — which the test can then choose.
    pub fn config_gate(mut self, gate: WriteGate) -> Self {
        self.config_gate = Some(gate);
        self
    }

    /// Starts from a loaded jdocs store rather than an empty one at the data
    /// directory's jdocs path.
    pub fn jdocs(mut self, jdocs: JdocsStore) -> Self {
        self.jdocs = Some(jdocs);
        self
    }

    /// Starts from a loaded session-certificate store rather than an empty one.
    pub fn session_certs(mut self, session_certs: SessionCertStore) -> Self {
        self.session_certs = session_certs;
        self
    }

    /// Writes the SDK ini through `sdk_ini` rather than through an empty store
    /// at [`DEFAULT_SDK_INI_DIR`], which [`AppStateBuilder::build`] warns
    /// about. The boot path passes the home directory here, which is what keeps
    /// that warning a signal rather than a line every boot prints.
    pub fn sdk_ini(mut self, sdk_ini: SdkIniStore) -> Self {
        self.sdk_ini = Some(sdk_ini);
        self
    }

    /// Starts from a non-empty set of transient token stores, which only a test
    /// wants: the running server starts all three empty and Go's restart clears
    /// them.
    pub fn tokens(mut self, tokens: TokenStores) -> Self {
        self.tokens = tokens;
        self
    }

    /// Reads and fills `logs` rather than a ring of its own.
    ///
    /// The binary passes the ring it already gave [`crate::logger::LogLayer`],
    /// because a ring nothing writes to would leave the web UI's three log
    /// handlers permanently empty.
    ///
    /// The ring has to have been built over the same [`WallClock`] that reaches
    /// [`AppStateBuilder::wall`], because a ring carries its own
    /// [`LogClock`] and nothing here can reach in and change it. Wire the two
    /// to different clocks and a line's stamp and the token server's claims
    /// read different times, which is exactly the disagreement
    /// [`WallLogClock`] exists to prevent within one line. A builder that sets
    /// neither gets the pairing for free: [`AppStateBuilder::build`] wraps its
    /// own wall clock in a [`WallLogClock`] and builds the ring over that.
    pub fn logs(mut self, logs: Arc<LogRing>) -> Self {
        self.logs = Some(logs);
        self
    }

    /// Reads calendar time from `wall` rather than from the system clock.
    ///
    /// A ring left to default is built over this clock, so a test that fixes
    /// the wall clock fixes its log stamps with it.
    pub fn wall(mut self, wall: Arc<dyn WallClock>) -> Self {
        self.wall = Some(wall);
        self
    }

    /// The finished state, ready to be handed to the router.
    pub fn build(self) -> Arc<AppState> {
        let clock: Arc<dyn Clock> = self.clock.unwrap_or_else(|| Arc::new(SystemClock::new()));
        let wall: Arc<dyn WallClock> = self
            .wall
            .unwrap_or_else(|| Arc::new(SystemWallClock::new()));
        let logs = self.logs.unwrap_or_else(|| {
            Arc::new(LogRing::new(Arc::new(WallLogClock::new(Arc::clone(&wall)))))
        });
        let config_gate = self
            .config_gate
            .unwrap_or_else(|| crate::config::config_gate(self.paths.data()));
        // A gate over one file beside a data directory naming another is a
        // wiring mistake with no symptom until something writes: the gate's
        // holders would order their writes against each other while writing a
        // file nothing else reads. The defaulted gate cannot disagree, so this
        // only ever catches a caller that reached
        // [`AppStateBuilder::config_gate`], which is a test.
        debug_assert_eq!(
            config_gate.path(),
            self.paths.data().api_config_path().to_string_lossy(),
            "the configuration gate and the resolved paths name different files"
        );
        let jdocs = self
            .jdocs
            .unwrap_or_else(|| JdocsStore::new(self.paths.data().jdocs_path()));
        let sdk_ini = self.sdk_ini.unwrap_or_else(|| {
            // Deviation 38: the port resolves every path explicitly and says
            // what it resolved, rather than guessing one from the working
            // directory the way `vars.go:213-225` does. Reaching this arm means
            // nobody resolved this one, and the fallback corresponds to no Go
            // mode at all — Go's `SDKIniPath` is absolute in all three of its
            // branches (`vars.go:207-227`) — so the line is the disclosure that
            // `sdk_config.ini` is about to be written somewhere that is nobody's
            // home directory and that moves with the process's working
            // directory.
            tracing::warn!(
                target: "wirepod_core::state",
                comp = "",
                "SDK ini directory was not resolved; falling back to {DEFAULT_SDK_INI_DIR}"
            );
            SdkIniStore::new(DEFAULT_SDK_INI_DIR)
        });
        let registry = RobotRegistry::new(self.factory)
            .with_timings(self.timings)
            .with_clock(Arc::clone(&clock))
            .with_liveness_deadline(self.liveness_deadline)
            .with_state_stream(self.state_stream);
        Arc::new(AppState {
            bot_info: RwLock::new(self.bot_info),
            pinger: PingerState::new(),
            registry,
            timings: self.timings,
            clock,
            paths: self.paths,
            config: RwLock::new(Arc::new(self.config)),
            config_gate,
            custom_intents: Mutex::new(None),
            jdocs,
            session_certs: self.session_certs,
            sdk_ini,
            tokens: self.tokens,
            logs,
            wall,
        })
    }
}
