//! The logger ring the web UI reads, and the tracing layer that fills it.
//!
//! Go keeps every log line in one package-level ring of 500 entries plus two
//! flat string buffers, all guarded by one mutex (`logger.go:57-77`), and three
//! HTTP handlers read them: `/api/get_logs` writes the INFO buffer as plain
//! text, `/api/get_debug_logs` writes the tray buffer as plain text, and
//! `/api/get_logs_json` writes the ring as JSON
//! (`config-ws/webserver.go:279-304`). The web UI parses all three, so the five
//! JSON tags and their order, both line layouts, and the ring's walk order are
//! a contract rather than an implementation detail.
//!
//! So is the encoder. `handleGetLogsJSON` serialises the entries with
//! `json.NewEncoder(w).Encode` (`config-ws/webserver.go:303`), which is
//! `json.Marshal`'s escaping plus one trailing newline: HTML escaping is on by
//! default, so a message carrying `&`, `<` or `>` reaches the browser as
//! `\u0026`, `\u003c` and `\u003e`. A log line is the freest text in the
//! server, since it carries whatever a robot, an operator or an LLM put in it,
//! so those three characters are ordinary rather than exotic. The handler C20
//! brings therefore writes [`crate::config::go_marshal`]'s bytes followed by a
//! `\n`, and never `serde_json::to_vec`, which escapes none of the three. The
//! two plain-text handlers beside it write their buffers verbatim
//! (`webserver.go:281`, `:286`) and have no encoder to match.
//!
//! Nothing in the port calls [`LogRing::record`] directly. Code emits `tracing`
//! events and [`LogLayer`] fills the ring from them, which is what lets one
//! call site feed both the ring and whatever formatting layer the binary
//! installs.
//!
//! [`LogLayer`] applies **no level filter**, because Go's ring records every
//! level including the DEBUG lines that only `/api/get_debug_logs` and
//! `/api/get_logs_json` ever show. Level filtering happens on the way out
//! instead, in [`LogRing::get_entries`], exactly as it does in Go. An
//! `EnvFilter` therefore belongs on the formatting layer, attached per-layer
//! with `Layer::with_filter`, and never on this one: putting it here would make
//! `RUST_LOG` silently decide what the web UI is allowed to see.
//!
//! What the layer does filter, and the only thing it filters, is the target.
//! Go's ring is fed by `logger.Debug`, `Info`, `Warn`, `Error` and `Println`
//! (`logger.go:191-214`, `:234-238`) and by nothing else, so the only lines
//! that ever reach it are the ones wire-pod itself writes. A layer that
//! admitted every event would also collect h2's per-frame events and the spans
//! hyper, tower and tonic open, plus the `log` records rustls and mdns-sd emit
//! once a binary installs the `tracing-log` bridge. The tray buffer holds 199
//! lines and the ring 500, so one robot conn-check would push wire-pod's own
//! lines out of both before the web UI could ask for them.
//! [`is_wire_pod_target`] is the admission rule.
//!
//! Go's `DEBUG_LOGGING` stdout mirror (`logger.go:82`, `:186-188`) has no
//! counterpart here on purpose. Printing events to stdout is what a `tracing`
//! formatting layer already does, and that layer is the one that gets the
//! filter.

use std::collections::VecDeque;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// How many entries the ring holds, from Go's `ring [500]Entry`
/// (`logger.go:75`).
pub const RING_LEN: usize = 500;

/// The length at which the tray buffer drops its oldest line, from
/// `len(LogTrayArray) >= 200` (`logger.go:158`).
///
/// The test is applied *after* the append, so the buffer settles one short of
/// this: it grows to 199, the 200th append takes it to 200, the test fires and
/// the front line goes, and every append after that leaves it at 199.
pub const TRAY_TRIM_AT: usize = 200;

/// The length at which the INFO buffer drops its oldest line, from
/// `len(LogArray) >= 50` (`logger.go:168`). Same append-then-trim rule as
/// [`TRAY_TRIM_AT`], so the steady state is 49.
pub const INFO_TRIM_AT: usize = 50;

/// Go's `CompJdocs` (`logger.go:34`).
pub const COMP_JDOCS: &str = "jdocs";
/// Go's `CompToken` (`logger.go:35`).
pub const COMP_TOKEN: &str = "token";
/// Go's `CompSTT` (`logger.go:36`).
pub const COMP_STT: &str = "stt";
/// Go's `CompIntent` (`logger.go:37`).
pub const COMP_INTENT: &str = "intent";
/// Go's `CompLLM` (`logger.go:38`).
pub const COMP_LLM: &str = "llm";
/// Go's `CompSDK` (`logger.go:39`).
pub const COMP_SDK: &str = "sdkapp";
/// Go's `CompWeb` (`logger.go:40`).
pub const COMP_WEB: &str = "web";
/// Go's `CompMDNS` (`logger.go:41`).
pub const COMP_MDNS: &str = "mdns";
/// Go's `CompBLE` (`logger.go:42`).
pub const COMP_BLE: &str = "ble";
/// Go's `CompLua` (`logger.go:43`).
pub const COMP_LUA: &str = "lua";
/// Go's `CompVoice` (`logger.go:44`).
pub const COMP_VOICE: &str = "voice";
/// Go's `CompConn` (`logger.go:45`).
pub const COMP_CONN: &str = "conn";

/// The twelve component names Go defines (`logger.go:33-46`), in declaration
/// order.
///
/// This is the whole vocabulary: Go passes one of these constants or the empty
/// string, never anything else, and the web UI's component column is built from
/// the values it sees. [`component_for_target`] is what keeps a `tracing`
/// target from inventing a thirteenth.
pub const COMPONENTS: [&str; 12] = [
    COMP_JDOCS,
    COMP_TOKEN,
    COMP_STT,
    COMP_INTENT,
    COMP_LLM,
    COMP_SDK,
    COMP_WEB,
    COMP_MDNS,
    COMP_BLE,
    COMP_LUA,
    COMP_VOICE,
    COMP_CONN,
];

/// Go's `logger.Level` (`logger.go:11-18`).
///
/// The variants are declared in Go's `iota` order, so the derived [`Ord`] is
/// Go's numeric ordering and `level >= INFO` (`logger.go:166`) ports as
/// `level >= LogLevel::Info`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LogLevel {
    /// Go's `DEBUG`, which is also every level Go does not recognise.
    Debug,
    /// Go's `INFO`.
    Info,
    /// Go's `WARN`.
    Warn,
    /// Go's `ERROR`.
    Error,
}

impl LogLevel {
    /// Go's `Level.String` (`logger.go:20-31`).
    ///
    /// This is the exact text that reaches [`Entry::level`] and therefore the
    /// web UI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    /// Go's `parseLevel` (`logger.go:124-135`), which reads an [`Entry`]'s
    /// stored level string back into a level for [`LogRing::get_entries`] to
    /// compare against.
    ///
    /// Anything unrecognised is `DEBUG`, the default arm. That arm is not dead:
    /// it is also what Go's `Level.String` answers for an out-of-range level
    /// (`logger.go:28-29`), and it is the only place that behaviour survives
    /// here, because [`LogLevel`] is a closed enum with no out-of-range value.
    pub fn parse(text: &str) -> Self {
        match text {
            "INFO" => Self::Info,
            "WARN" => Self::Warn,
            "ERROR" => Self::Error,
            _ => Self::Debug,
        }
    }

    /// Folds a `tracing` level onto Go's four.
    ///
    /// `TRACE` and `DEBUG` both become Go's `DEBUG`. Go has no fifth level and
    /// its own `Level.String` sends everything it does not name to `DEBUG`
    /// (`logger.go:28-29`), so this is that default arm rather than a choice
    /// made here.
    pub fn from_tracing(level: tracing::Level) -> Self {
        if level == tracing::Level::ERROR {
            Self::Error
        } else if level == tracing::Level::WARN {
            Self::Warn
        } else if level == tracing::Level::INFO {
            Self::Info
        } else {
            Self::Debug
        }
    }
}

/// One ring entry, as `/api/get_logs_json` serialises it.
///
/// Go's struct is `logger.Entry` (`logger.go:48-55`). It has six fields and
/// exactly five of them reach the wire: the sixth, `t time.Time`, is lower-case
/// and therefore unexported, so `encoding/json` skips it. The port has no sixth
/// field at all, because nothing reads it; Go's `logf` fills it and never uses
/// it either.
///
/// Field order here is Go's declaration order, which `encoding/json` uses as
/// its marshal order, and the names are Go's tags. Both are part of the
/// contract: the web UI reads `t`, `level`, `comp`, `bot` and `msg` off these
/// objects.
///
/// The encoder is part of it too, for the reason the module docs give: these go
/// out through Go's `json.NewEncoder` (`config-ws/webserver.go:303`), so
/// `msg` carries `&`, `<` and `>` as `\u0026`, `\u003c` and `\u003e`.
/// [`crate::config::go_marshal`] is the only marshaller here that does that.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Entry {
    /// Go's `TimeMS`, tagged `t`: `time.Now().UnixMilli()` (`logger.go:141`).
    /// A JSON integer, not a float and not a string.
    #[serde(rename = "t")]
    pub time_ms: i64,
    /// Go's `Level`, the *string* form, because that is what Go stores and what
    /// [`LogRing::get_entries`] parses back (`logger.go:142`, `:227`).
    pub level: String,
    /// One of [`COMPONENTS`], or empty.
    pub comp: String,
    /// The robot's serial, or empty.
    pub bot: String,
    /// The message, with ANSI escapes already stripped (`logger.go:139`).
    pub msg: String,
}

/// The wall clock the ring stamps entries with.
///
/// Go reads `time.Now()` once per line (`logger.go:138`) and uses it twice: as
/// `UnixMilli` for the JSON entry, and formatted with the `2006.01.02 15:04:05`
/// layout in local time for both text line layouts. One reading gives both, so
/// a single call returns both and the two can never disagree.
///
/// This is a separate seam from [`Clock`](crate::clock::Clock), which is
/// monotonic and cannot name a calendar instant.
pub trait LogClock: Send + Sync {
    /// Go's `now := time.Now()` (`logger.go:138`), already split into the two
    /// forms the logger needs.
    fn now(&self) -> LogInstant;
}

/// One reading of a [`LogClock`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogInstant {
    /// Go's `now.UnixMilli()` (`logger.go:141`).
    pub unix_millis: i64,
    /// Go's `now.Format("2006.01.02 15:04:05")` in local time
    /// (`logger.go:103`, `:114`).
    pub stamp: String,
}

/// A [`LogClock`] a test drives by hand.
///
/// Reads and writes go through a [`Mutex`] so the clock is `Sync` and can be
/// shared through an [`Arc`] between the ring and the test that moves it, which
/// is the shape [`ManualClock`](crate::clock::ManualClock) already uses.
#[derive(Debug)]
pub struct ManualLogClock {
    now: Mutex<LogInstant>,
}

impl ManualLogClock {
    /// Starts a clock reading `unix_millis` and `stamp`.
    pub fn new(unix_millis: i64, stamp: impl Into<String>) -> Self {
        Self {
            now: Mutex::new(LogInstant {
                unix_millis,
                stamp: stamp.into(),
            }),
        }
    }

    /// Moves the millisecond reading, forwards or backwards, leaving the stamp
    /// alone.
    pub fn set_unix_millis(&self, unix_millis: i64) {
        self.lock().unix_millis = unix_millis;
    }

    /// Moves the millisecond reading forward by `delta`.
    pub fn advance_millis(&self, delta: i64) {
        let mut now = self.lock();
        now.unix_millis = now.unix_millis.saturating_add(delta);
    }

    /// Replaces the stamp, leaving the millisecond reading alone.
    pub fn set_stamp(&self, stamp: impl Into<String>) {
        self.lock().stamp = stamp.into();
    }

    fn lock(&self) -> MutexGuard<'_, LogInstant> {
        self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl LogClock for ManualLogClock {
    fn now(&self) -> LogInstant {
        self.lock().clone()
    }
}

/// The state behind the ring's one mutex.
///
/// Go guards all of this with the same `mu` (`logger.go:74`), the file write
/// included, and this keeps that grouping: one lock, one critical section, and
/// no way for the ring, the two buffers and the file to disagree about what was
/// recorded or in what order.
struct RingInner {
    /// Go's `ring [500]Entry`. This grows to [`RING_LEN`] and then overwrites
    /// in place, where Go starts at full length with zero values; `count`
    /// bounds every walk either way, so no reader can tell the difference.
    ring: Vec<Entry>,
    /// Go's `ringIdx`: where the next entry goes.
    index: usize,
    /// Go's `ringCount`: how many slots have ever been written, capped at
    /// [`RING_LEN`].
    count: usize,
    /// Go's `LogTrayArray` (`logger.go:62`), every level.
    tray: VecDeque<String>,
    /// Go's `LogArray` (`logger.go:59`), INFO and above.
    info: VecDeque<String>,
    /// Go's `logFile` (`logger.go:69`). `None` covers both "no `LOG_FILE`" and
    /// "the open failed", which is why Go's separate `fileFailed` flag
    /// (`logger.go:70`, `:86`) needs no counterpart: it is only ever set on the
    /// path that also leaves `logFile` nil, so the `!fileFailed` half of Go's
    /// test at `logger.go:177` can never change the outcome.
    file: Option<File>,
}

/// Go's whole `logger` package state: the ring, the two text buffers and the
/// optional file sink, under one mutex.
///
/// One of these is built at startup and shared through
/// [`AppState`](crate::state::AppState). Go's is a package global reached
/// through a `sync.Once`; this is the same thing with the initialisation made
/// explicit.
pub struct LogRing {
    inner: Mutex<RingInner>,
    clock: Arc<dyn LogClock>,
}

impl LogRing {
    /// A ring with no file sink, which is Go with `LOG_FILE` unset
    /// (`logger.go:83`).
    pub fn new(clock: Arc<dyn LogClock>) -> Self {
        Self::with_sink(clock, None)
    }

    /// A ring that also appends every line to `path`, which is Go with
    /// `LOG_FILE` set.
    ///
    /// The file is opened once, append-only, created if missing
    /// (`logger.go:84`). If that open fails the sink is silently disabled for
    /// the life of the process, exactly as Go's `fileFailed` does
    /// (`logger.go:85-87`): there is nowhere to report the failure to, because
    /// the logger is the thing that failed. Write errors after a successful
    /// open are dropped too, because Go discards `WriteString`'s error
    /// (`logger.go:178`).
    pub fn with_log_file(clock: Arc<dyn LogClock>, path: &Path) -> Self {
        Self::with_sink(clock, open_sink(path))
    }

    fn with_sink(clock: Arc<dyn LogClock>, file: Option<File>) -> Self {
        Self {
            inner: Mutex::new(RingInner {
                ring: Vec::with_capacity(RING_LEN),
                index: 0,
                count: 0,
                tray: VecDeque::new(),
                info: VecDeque::new(),
                file,
            }),
            clock,
        }
    }

    /// Whether the file sink is open.
    ///
    /// Reported once in the startup log block so a mistyped `LOG_FILE` is
    /// visible somewhere. Go reports it nowhere.
    pub fn has_log_file(&self) -> bool {
        self.lock().file.is_some()
    }

    /// Go's `logf` (`logger.go:137-189`), minus the parts that belong to a
    /// `tracing` formatting layer.
    ///
    /// The order matters and is Go's: read the clock once, strip ANSI from the
    /// message, build the entry and the legacy line from the *stripped* text,
    /// then take the lock and do the ring, the tray buffer, the INFO buffer and
    /// the file write in that one critical section. The file write is
    /// synchronous and inside the lock because Go's is (`logger.go:177-179`);
    /// moving it out would let two lines interleave in the file.
    ///
    /// Not ported: Go's non-blocking send to `LogTrayChan` (`logger.go:182-185`),
    /// which only the tray app reads and which arrives with the tray shell, and
    /// the `DEBUG_LOGGING` stdout mirror (`logger.go:186-188`).
    ///
    /// The ring's mutex is a [`std::sync::Mutex`] and is therefore not
    /// reentrant, so nothing reached from inside that critical section may emit
    /// a `tracing` event: [`LogLayer`] would call straight back into here and
    /// deadlock the thread. That is why the file write inside the lock discards
    /// its error rather than logging it, and it is a rule for anything added to
    /// this critical section later.
    pub fn record(&self, level: LogLevel, comp: &str, bot: &str, msg: &str) {
        let now = self.clock.now();
        let clean = strip_ansi(msg);
        let entry = Entry {
            time_ms: now.unix_millis,
            level: level.as_str().to_owned(),
            comp: comp.to_owned(),
            bot: bot.to_owned(),
            msg: clean.clone(),
        };
        let legacy = legacy_line(&now.stamp, comp, bot, &clean);

        let mut inner = self.lock();

        // `ring[ringIdx] = e; ringIdx = (ringIdx + 1) % len(ring)`
        // (`logger.go:151-155`).
        if inner.ring.len() < RING_LEN {
            inner.ring.push(entry);
        } else {
            let index = inner.index;
            inner.ring[index] = entry;
        }
        inner.index = (inner.index + 1) % RING_LEN;
        if inner.count < RING_LEN {
            inner.count += 1;
        }

        // Append, then trim if the length has *reached* the bound
        // (`logger.go:157-160`). Go rebuilds `LogTrayList` by concatenating the
        // whole buffer right here (`logger.go:161-164`); that string is built
        // on demand by `tray_text` instead, which no reader can distinguish.
        inner.tray.push_back(legacy.clone());
        if inner.tray.len() >= TRAY_TRIM_AT {
            inner.tray.pop_front();
        }

        // The same rule for INFO and above (`logger.go:166-175`).
        if level >= LogLevel::Info {
            inner.info.push_back(legacy);
            if inner.info.len() >= INFO_TRIM_AT {
                inner.info.pop_front();
            }
        }

        if let Some(file) = inner.file.as_mut() {
            let line = file_line(&now.stamp, level, comp, bot, &clean);
            // Go discards this error (`logger.go:178`).
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// Go's `GetEntries` (`logger.go:216-232`), which is what
    /// `/api/get_logs_json` serialises.
    ///
    /// Three things are load-bearing. The walk starts at the write index once
    /// the ring has wrapped and at zero before that, so entries come out in
    /// insertion order, oldest first. The level test re-parses the entry's
    /// stored string with [`LogLevel::parse`] rather than trusting a cached
    /// level. And `since` is **strictly** greater: the web UI polls with the
    /// largest `t` it has already shown, so an inclusive test would re-deliver
    /// that entry on every poll.
    ///
    /// The result is a [`Vec`], which serialises as `[]` when empty. That is
    /// deliberate parity: Go builds its slice with `make([]Entry, 0, ringCount)`
    /// (`logger.go:220`), so it is non-nil and marshals as `[]` rather than the
    /// `null` a nil slice would give.
    pub fn get_entries(&self, min: LogLevel, since: i64) -> Vec<Entry> {
        let inner = self.lock();
        let mut out = Vec::with_capacity(inner.count);
        let start = if inner.count == RING_LEN {
            inner.index
        } else {
            0
        };
        for offset in 0..inner.count {
            let entry = &inner.ring[(start + offset) % RING_LEN];
            if LogLevel::parse(&entry.level) >= min && entry.time_ms > since {
                out.push(entry.clone());
            }
        }
        out
    }

    /// Go's `LogTrayList` (`logger.go:61`), which `/api/get_debug_logs` writes
    /// verbatim: every level, oldest first, each line already newline
    /// terminated.
    pub fn tray_text(&self) -> String {
        self.lock().tray.iter().map(String::as_str).collect()
    }

    /// Go's `LogList` (`logger.go:58`), which `/api/get_logs` writes verbatim:
    /// INFO and above.
    pub fn info_text(&self) -> String {
        self.lock().info.iter().map(String::as_str).collect()
    }

    /// How many lines the tray buffer holds, which settles at
    /// `TRAY_TRIM_AT - 1`.
    pub fn tray_len(&self) -> usize {
        self.lock().tray.len()
    }

    /// How many lines the INFO buffer holds, which settles at
    /// `INFO_TRIM_AT - 1`.
    pub fn info_len(&self) -> usize {
        self.lock().info.len()
    }

    /// How many entries the ring holds, which settles at [`RING_LEN`]. Go's
    /// `ringCount`.
    pub fn len(&self) -> usize {
        self.lock().count
    }

    /// Whether the ring has recorded anything yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> MutexGuard<'_, RingInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for LogRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        f.debug_struct("LogRing")
            .field("entries", &inner.count)
            .field("tray", &inner.tray.len())
            .field("info", &inner.info.len())
            .field("log_file", &inner.file.is_some())
            .finish()
    }
}

/// Opens the `LOG_FILE` sink the way Go does (`logger.go:84`):
/// `O_APPEND|O_CREATE|O_WRONLY` with mode 0644. A failure answers `None`, which
/// disables the sink silently and permanently.
fn open_sink(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Rust's default is 0o666 before the umask; Go asks for 0o644.
        options.mode(0o644);
    }
    options.open(path).ok()
}

/// Go's `legacyLine` (`logger.go:102-111`): the line both text buffers hold.
///
/// The stamp and `": "`, then `"[<comp>] "` when the component is set, then
/// `"<bot>: "` when the bot is set, then the message and a newline.
fn legacy_line(stamp: &str, comp: &str, bot: &str, msg: &str) -> String {
    let mut line = String::with_capacity(stamp.len() + msg.len() + 16);
    line.push_str(stamp);
    line.push_str(": ");
    push_comp_and_bot(&mut line, comp, bot);
    line.push_str(msg);
    line.push('\n');
    line
}

/// Go's `fileFormatLine` (`logger.go:113-122`): the line the `LOG_FILE` sink
/// gets.
///
/// The same shape as [`legacy_line`] with one difference, in the head: the
/// stamp is followed by a space and the level name rather than by a colon.
fn file_line(stamp: &str, level: LogLevel, comp: &str, bot: &str, msg: &str) -> String {
    let mut line = String::with_capacity(stamp.len() + msg.len() + 24);
    line.push_str(stamp);
    line.push(' ');
    line.push_str(level.as_str());
    line.push(' ');
    push_comp_and_bot(&mut line, comp, bot);
    line.push_str(msg);
    line.push('\n');
    line
}

/// The tail both layouts share (`logger.go:104-109`, `:115-120`): an empty
/// component or bot contributes nothing at all, not even its brackets or its
/// separator.
fn push_comp_and_bot(line: &mut String, comp: &str, bot: &str) {
    if !comp.is_empty() {
        line.push('[');
        line.push_str(comp);
        line.push_str("] ");
    }
    if !bot.is_empty() {
        line.push_str(bot);
        line.push_str(": ");
    }
}

/// Removes ANSI escape sequences from a message, reproducing Go's
/// `ansiRe.ReplaceAllString(msg, "")` (`logger.go:72`, `:139`).
///
/// Go's pattern is an escape, a `[`, any run of digits and semicolons, and one
/// ASCII letter. There is no regex crate in this workspace and this shape needs
/// none: scan for the escape and the bracket, consume the digits and
/// semicolons, and drop the run only if a letter closes it.
///
/// The failure case is the part worth stating. A run that never reaches a
/// letter is not a match, so it survives verbatim, and the scan resumes at the
/// byte *after* the escape rather than after the failed run. That is what Go's
/// leftmost-match search does, and it matters: in `ESC[1ESC[0mX` the first run
/// fails and the second still matches, leaving `ESC[1X`.
///
/// Every byte a match can consume is ASCII, so the retained ranges always fall
/// on character boundaries and non-ASCII text passes through untouched.
pub fn strip_ansi(text: &str) -> String {
    const ESC: u8 = 0x1b;

    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut kept = 0;
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == ESC && bytes.get(at + 1) == Some(&b'[') {
            let mut end = at + 2;
            while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
                end += 1;
            }
            if end < bytes.len() && bytes[end].is_ascii_alphabetic() {
                out.push_str(&text[kept..at]);
                at = end + 1;
                kept = at;
                continue;
            }
        }
        at += 1;
    }
    out.push_str(&text[kept..]);
    out
}

/// The component a `tracing` target names, or the empty string.
///
/// Go passes a component explicitly at every call site, so it has no target to
/// map. Here the target is the natural place for it: a module that always logs
/// as `jdocs` writes `target: "jdocs"` once per call site instead of repeating
/// a field, and `RUST_LOG=jdocs=debug` then filters exactly that module on the
/// formatting layer. Only the twelve names in [`COMPONENTS`] count, so the
/// default target, which is the emitting module path, answers empty rather than
/// leaking `wirepod_server::sdkapp::stim` into the web UI's component column.
pub fn component_for_target(target: &str) -> &'static str {
    COMPONENTS
        .iter()
        .copied()
        .find(|name| *name == target)
        .unwrap_or("")
}

/// The prefix a target inside this workspace begins with.
///
/// Cargo turns the hyphen in `wirepod-core` into an underscore, so the default
/// target of a `tracing` event, which is its module path, reads
/// `wirepod_core::logger`, and every other crate here is spelled the same way.
const CRATE_TARGET_PREFIX: &str = "wirepod_";

/// Whether an event's target names something this port wrote, which is the only
/// thing [`LogLayer`] admits into the ring.
///
/// Two spellings count, and they are the two a ported call site can have. One
/// of [`COMPONENTS`], which is a call site that named its Go component as its
/// target, and any module path inside this workspace, which always begins
/// [`CRATE_TARGET_PREFIX`]. Everything else belongs to a library, and Go's ring
/// never sees a line a library wrote: `logger.Debug` and its siblings are the
/// only doors into it (`logger.go:191-214`, `:234-238`).
///
/// This is a target test and not a level test. Go's ring takes every level, so
/// an admitted target's TRACE event is recorded exactly like its ERROR one.
pub fn is_wire_pod_target(target: &str) -> bool {
    target.starts_with(CRATE_TARGET_PREFIX) || COMPONENTS.contains(&target)
}

/// The `tracing` layer that fills a [`LogRing`].
///
/// It filters by target and by nothing else, as the module docs explain: an
/// event [`is_wire_pod_target`] rejects is dropped, every level of everything
/// it admits is kept, and any `EnvFilter` goes on the formatting layer beside
/// it rather than on this one:
///
/// ```no_run
/// # use std::sync::Arc;
/// # use wirepod_core::logger::{LogLayer, LogRing, ManualLogClock};
/// use tracing_subscriber::layer::SubscriberExt;
/// use tracing_subscriber::util::SubscriberInitExt;
/// use tracing_subscriber::{EnvFilter, Layer, fmt};
///
/// # let clock = Arc::new(ManualLogClock::new(0, "2026.01.02 03:04:05"));
/// let ring = Arc::new(LogRing::new(clock));
/// tracing_subscriber::registry()
///     .with(LogLayer::new(Arc::clone(&ring)))
///     .with(fmt::layer().with_filter(EnvFilter::from_default_env()))
///     .init();
/// ```
///
/// Each event becomes one [`LogRing::record`] call. The message is the event's
/// `message` field; any other field the event carries is dropped, because Go's
/// logger takes one already-formatted string and the web UI has nowhere to show
/// structured fields.
pub struct LogLayer {
    ring: Arc<LogRing>,
}

impl LogLayer {
    /// A layer writing into `ring`.
    pub fn new(ring: Arc<LogRing>) -> Self {
        Self { ring }
    }
}

impl fmt::Debug for LogLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogLayer")
            .field("ring", &self.ring)
            .finish()
    }
}

impl<S> Layer<S> for LogLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        // The whole filter, and the only one. A library's event is not one of
        // Go's log lines, and the web UI's two text buffers are small enough
        // that admitting one would bury wire-pod's own lines within seconds.
        if !is_wire_pod_target(metadata.target()) {
            return;
        }

        let mut fields = EventFields::default();
        event.record(&mut fields);

        let level = LogLevel::from_tracing(*metadata.level());
        // An explicit `comp` field wins even when it is empty, which is how a
        // call site says "this is one of Go's component-less lines" without
        // giving up its target and the `RUST_LOG` filtering that comes with it.
        let comp = fields
            .comp
            .unwrap_or_else(|| component_for_target(metadata.target()).to_owned());
        // Go's bot column is a serial. `esn` is what the rest of this crate
        // calls that, so it is accepted as an alias.
        let bot = fields.bot.or(fields.esn).unwrap_or_default();

        self.ring.record(level, &comp, &bot, &fields.message);
    }
}

/// The four event fields this layer reads.
#[derive(Debug, Default)]
struct EventFields {
    message: String,
    comp: Option<String>,
    bot: Option<String>,
    esn: Option<String>,
}

impl EventFields {
    fn store(&mut self, name: &str, value: String) {
        match name {
            "message" => self.message = value,
            "comp" => self.comp = Some(value),
            "bot" => self.bot = Some(value),
            "esn" => self.esn = Some(value),
            _ => {}
        }
    }
}

impl Visit for EventFields {
    /// A `&str` field, which is the common spelling of `comp`, `bot` and `esn`.
    /// Taken verbatim; going through the `Debug` arm below would wrap it in
    /// quotes.
    fn record_str(&mut self, field: &Field, value: &str) {
        self.store(field.name(), value.to_owned());
    }

    /// Everything else, the message included, which `tracing` records as
    /// `format_args!` output. `Debug` on `Arguments` renders the formatted text
    /// with no quoting, so the message arrives exactly as the call site wrote
    /// it. `Visit`'s other methods default to this one.
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.store(field.name(), format!("{value:?}"));
    }
}
