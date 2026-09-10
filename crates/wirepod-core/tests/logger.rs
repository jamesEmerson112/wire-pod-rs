//! The logger ring, its two text buffers and the tracing layer that fills it.
//!
//! Every expectation about a line layout or a level name is read from
//! `docs/phases/P1-robot-connect-auth/go-probe/expected.txt`, which is Go's own
//! stdout, rather than written out here. Nothing in this file drives
//! `LogRing::record` directly: the events go through a real `tracing`
//! subscriber, so the layer's level folding, component derivation and bot
//! derivation are covered along with the ring itself.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tracing_subscriber::layer::SubscriberExt;
use wirepod_core::logger::{
    COMPONENTS, INFO_TRIM_AT, LogLayer, LogLevel, LogRing, ManualLogClock, RING_LEN, TRAY_TRIM_AT,
    component_for_target, strip_ansi,
};

const EXPECTED: &str =
    include_str!("../../../docs/phases/P1-robot-connect-auth/go-probe/expected.txt");

/// The stamp every `legacystamp` line layout case in the probe was recorded
/// with, so a clock fixed here reproduces the recorded bytes exactly.
const STAMP: &str = "2026.01.02 03:04:05";

// ---------------------------------------------------------------------------
// Reading the probe recording
// ---------------------------------------------------------------------------

/// Unquotes a Go `%q` string literal.
///
/// Only the five escapes the recording can contain are accepted. Anything else
/// panics rather than being passed through, so a probe change that starts
/// emitting a new escape fails loudly instead of being silently mis-parsed into
/// a value that still compares equal.
fn unquote(literal: &str, line: usize) -> String {
    let inner = literal
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("line {line}: the output column is not a quoted literal"));

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            other => panic!("line {line}: unsupported escape {other:?} in {literal}"),
        }
    }
    out
}

/// One case from the recording: its line number for a failure message, its
/// input pairs, and Go's output already unquoted.
struct Case {
    line: usize,
    pairs: Vec<(&'static str, &'static str)>,
    want: String,
}

impl Case {
    /// The `kind=` value, which the format guarantees is the first pair.
    fn kind(&self) -> &'static str {
        self.pairs[0].1
    }

    fn need(&self, key: &str) -> &'static str {
        self.pairs
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
            .unwrap_or_else(|| panic!("line {}: no {key}= pair", self.line))
    }
}

/// Every case in the recording's `legacystamp` section, which is the section
/// `pkg/logger/logger.go` produced.
fn legacystamp_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (index, raw) in EXPECTED.lines().enumerate() {
        let line = index + 1;
        let text = raw.strip_suffix('\r').unwrap_or(raw);
        if text.starts_with('#') || text.is_empty() {
            continue;
        }
        let mut columns = text.split('\t');
        let section = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: no section column"));
        if section != "legacystamp" {
            continue;
        }
        let input = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: no input column"));
        let output = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: no output column"));
        assert!(
            columns.next().is_none(),
            "line {line}: more than three columns"
        );

        let pairs: Vec<(&str, &str)> = input
            .split(' ')
            .map(|pair| {
                pair.split_once('=')
                    .unwrap_or_else(|| panic!("line {line}: {pair} is not key=value"))
            })
            .collect();
        assert_eq!(
            pairs[0].0, "kind",
            "line {line}: the first pair is not kind="
        );

        cases.push(Case {
            line,
            pairs,
            want: unquote(output, line),
        });
    }
    assert!(
        !cases.is_empty(),
        "the recording has no legacystamp section"
    );
    cases
}

/// Every kind the section can contain is either asserted below or explicitly
/// declared as belonging to another module, so a new probe case cannot slip
/// through untested.
#[test]
fn every_recorded_kind_is_accounted_for() {
    for case in legacystamp_cases() {
        match case.kind() {
            // Asserted by the tests below.
            "legacy_line" | "file_line" | "level_string" => {}
            // The `2006.01.02 15:04:05` stamp itself, and the layout constant
            // that produces it, belong to the wall clock and `timefmt`. This
            // module never formats a time: it takes an already-formatted stamp
            // from its `LogClock`.
            "stamp" | "const" => {}
            other => panic!("line {}: unhandled kind {other}", case.line),
        }
    }
}

// ---------------------------------------------------------------------------
// Driving the layer
// ---------------------------------------------------------------------------

/// Runs `body` with a real subscriber whose only layer is the one under test,
/// installed unfiltered exactly as the binary installs it.
fn drive(ring: &Arc<LogRing>, body: impl FnOnce()) {
    let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(ring)));
    tracing::subscriber::with_default(subscriber, body);
}

/// A ring on a clock fixed at `STAMP`, and the clock, so a test can move the
/// millisecond reading.
fn fixed_ring(unix_millis: i64) -> (Arc<LogRing>, Arc<ManualLogClock>) {
    let clock = Arc::new(ManualLogClock::new(unix_millis, STAMP));
    let ring = Arc::new(LogRing::new(Arc::clone(&clock) as Arc<_>));
    (ring, clock)
}

/// Emits one event at a level named the way the probe names it. The level has
/// to be a literal in the macro, so the recording's string picks the arm.
fn emit(level: &str, comp: &str, bot: &str, msg: &str) {
    match level {
        "DEBUG" => tracing::debug!(comp = comp, bot = bot, "{msg}"),
        "INFO" => tracing::info!(comp = comp, bot = bot, "{msg}"),
        "WARN" => tracing::warn!(comp = comp, bot = bot, "{msg}"),
        "ERROR" => tracing::error!(comp = comp, bot = bot, "{msg}"),
        other => panic!("the recording names an unknown level {other}"),
    }
}

fn messages(ring: &LogRing) -> Vec<String> {
    ring.get_entries(LogLevel::Debug, 0)
        .into_iter()
        .map(|entry| entry.msg)
        .collect()
}

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "wirepod-logger-{}-{tag}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).expect("the temporary directory is creatable");
    dir
}

// ---------------------------------------------------------------------------
// The JSON contract
// ---------------------------------------------------------------------------

/// The five tags, their order and their types, against literal JSON.
///
/// Go's `Entry` has six fields and the sixth, `t time.Time`, is unexported, so
/// `encoding/json` never writes it. The key count is what pins that.
#[test]
fn an_entry_carries_gos_five_tags_in_gos_order() {
    let (ring, _clock) = fixed_ring(1_757_000_000_123);
    drive(&ring, || {
        tracing::info!(target: "web", bot = "00000000", "hello");
    });

    let json = serde_json::to_string(&ring.get_entries(LogLevel::Debug, 0))
        .expect("the entries serialise");
    assert_eq!(
        json,
        r#"[{"t":1757000000123,"level":"INFO","comp":"web","bot":"00000000","msg":"hello"}]"#
    );

    let value: serde_json::Value = serde_json::from_str(&json).expect("the JSON parses back");
    let object = value[0].as_object().expect("an entry is an object");
    assert_eq!(
        object.len(),
        5,
        "the unexported sixth Go field must not appear"
    );
    assert!(
        object["t"].is_i64(),
        "t is an integer, not a float or a string"
    );
}

/// Go builds its result with `make([]Entry, 0, ringCount)`, so an empty result
/// marshals as `[]`. A nil slice would marshal as `null` and the web UI would
/// have to guard for it.
#[test]
fn an_empty_result_serialises_as_an_empty_array() {
    let (ring, _clock) = fixed_ring(1);
    assert!(ring.is_empty());
    assert_eq!(
        serde_json::to_string(&ring.get_entries(LogLevel::Debug, 0)).expect("it serialises"),
        "[]"
    );

    // Also when the ring holds entries but none of them pass the filters.
    drive(&ring, || tracing::debug!("only a debug line"));
    assert_eq!(
        serde_json::to_string(&ring.get_entries(LogLevel::Info, 0)).expect("it serialises"),
        "[]"
    );
}

// ---------------------------------------------------------------------------
// The ring
// ---------------------------------------------------------------------------

/// The ring wraps at 500 and `get_entries` walks from the write index, so
/// entries come out oldest first with the overwritten ones gone.
#[test]
fn the_ring_wraps_and_walks_in_insertion_order() {
    assert_eq!(RING_LEN, 500);
    let total = RING_LEN + 100;
    let (ring, _clock) = fixed_ring(1);
    drive(&ring, || {
        for index in 0..total {
            tracing::info!("m{index}");
        }
    });

    assert_eq!(ring.len(), RING_LEN);
    let entries = ring.get_entries(LogLevel::Debug, 0);
    assert_eq!(entries.len(), RING_LEN);
    let first = total - RING_LEN;
    for (offset, entry) in entries.iter().enumerate() {
        assert_eq!(entry.msg, format!("m{}", first + offset));
    }
    assert_eq!(entries[0].msg, format!("m{first}"));
    assert_eq!(entries[RING_LEN - 1].msg, format!("m{}", total - 1));
}

/// Before the ring wraps the walk starts at zero, which is the other arm of the
/// same `if`.
#[test]
fn a_partly_filled_ring_walks_from_the_start() {
    let (ring, _clock) = fixed_ring(1);
    drive(&ring, || {
        for index in 0..3 {
            tracing::info!("m{index}");
        }
    });
    assert_eq!(ring.len(), 3);
    assert_eq!(messages(&ring), ["m0", "m1", "m2"]);
}

/// `since` is strictly greater, tested at the exact boundary in both
/// directions. The web UI polls with the largest `t` it has already shown, so
/// an inclusive test would re-deliver that entry forever.
#[test]
fn since_is_strictly_greater_at_the_boundary() {
    let (ring, clock) = fixed_ring(10);
    drive(&ring, || {
        tracing::info!("a");
        clock.set_unix_millis(20);
        tracing::info!("b");
        clock.set_unix_millis(30);
        tracing::info!("c");
    });

    assert_eq!(messages(&ring), ["a", "b", "c"]);
    // One millisecond below the middle entry: it is included.
    let below: Vec<String> = ring
        .get_entries(LogLevel::Debug, 19)
        .into_iter()
        .map(|entry| entry.msg)
        .collect();
    assert_eq!(below, ["b", "c"]);
    // Exactly the middle entry's own stamp: it is excluded.
    let at: Vec<String> = ring
        .get_entries(LogLevel::Debug, 20)
        .into_iter()
        .map(|entry| entry.msg)
        .collect();
    assert_eq!(at, ["c"]);
    // The newest entry's own stamp answers nothing at all.
    assert!(ring.get_entries(LogLevel::Debug, 30).is_empty());
}

/// The minimum level is compared against the entry's stored level string,
/// re-parsed, and `DEBUG` lets everything through.
#[test]
fn the_minimum_level_filters_the_walk() {
    let (ring, _clock) = fixed_ring(1);
    drive(&ring, || {
        tracing::debug!("d");
        tracing::info!("i");
        tracing::warn!("w");
        tracing::error!("e");
    });

    assert_eq!(ring.get_entries(LogLevel::Debug, 0).len(), 4);
    assert_eq!(ring.get_entries(LogLevel::Info, 0).len(), 3);
    assert_eq!(ring.get_entries(LogLevel::Warn, 0).len(), 2);
    assert_eq!(ring.get_entries(LogLevel::Error, 0).len(), 1);
}

// ---------------------------------------------------------------------------
// The two legacy text buffers
// ---------------------------------------------------------------------------

/// The tray buffer takes every level and settles one short of its bound,
/// because Go appends first and only then tests `len >= 200`.
#[test]
fn the_tray_buffer_settles_one_short_of_its_bound() {
    let (ring, _clock) = fixed_ring(1);
    // Spelled out, so moving the constant cannot move the expectation with it.
    assert_eq!(TRAY_TRIM_AT, 200);
    let steady = TRAY_TRIM_AT - 1;
    assert_eq!(steady, 199);

    // One below the bound: nothing has been dropped yet.
    drive(&ring, || {
        for index in 0..steady {
            tracing::debug!("m{index}");
        }
    });
    assert_eq!(ring.tray_len(), steady);
    let lines: Vec<String> = ring.tray_text().lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), steady);
    assert!(lines[0].ends_with(": m0"), "{}", lines[0]);

    // One more append reaches the bound, the test fires, the front line goes.
    drive(&ring, || tracing::debug!("m{steady}"));
    assert_eq!(ring.tray_len(), steady);
    let lines: Vec<String> = ring.tray_text().lines().map(str::to_owned).collect();
    assert!(lines[0].ends_with(": m1"), "{}", lines[0]);
    assert!(lines[steady - 1].ends_with(&format!(": m{steady}")));

    // And it stays there however many more arrive.
    drive(&ring, || {
        for index in 0..250 {
            tracing::debug!("later {index}");
        }
    });
    assert_eq!(ring.tray_len(), steady);
    assert_eq!(ring.tray_text().lines().count(), steady);

    // None of that reached the INFO buffer, which takes INFO and above.
    assert_eq!(ring.info_len(), 0);
    assert!(ring.info_text().is_empty());
}

/// The INFO buffer follows the same rule with a bound of 50, so it settles at
/// 49.
#[test]
fn the_info_buffer_settles_one_short_of_its_bound() {
    let (ring, _clock) = fixed_ring(1);
    // Spelled out, so moving the constant cannot move the expectation with it.
    assert_eq!(INFO_TRIM_AT, 50);
    let steady = INFO_TRIM_AT - 1;
    assert_eq!(steady, 49);

    drive(&ring, || {
        for index in 0..steady {
            tracing::info!("m{index}");
        }
    });
    assert_eq!(ring.info_len(), steady);
    let lines: Vec<String> = ring.info_text().lines().map(str::to_owned).collect();
    assert!(lines[0].ends_with(": m0"), "{}", lines[0]);

    drive(&ring, || tracing::info!("m{steady}"));
    assert_eq!(ring.info_len(), steady);
    let lines: Vec<String> = ring.info_text().lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), steady);
    assert!(lines[0].ends_with(": m1"), "{}", lines[0]);

    drive(&ring, || {
        for index in 0..250 {
            tracing::warn!("later {index}");
        }
    });
    assert_eq!(ring.info_len(), steady);
}

/// Both buffers hold Go's `legacyLine` bytes, recorded by the probe.
#[test]
fn the_text_buffers_use_gos_legacy_line_layout() {
    let mut seen = 0;
    for case in legacystamp_cases()
        .iter()
        .filter(|case| case.kind() == "legacy_line")
    {
        let (ring, _clock) = fixed_ring(1);
        // The recording carries no level for this layout, because `legacyLine`
        // does not take one. INFO puts the line in both buffers at once.
        drive(&ring, || {
            emit(
                "INFO",
                case.need("comp"),
                case.need("bot"),
                case.need("msg"),
            );
        });
        assert_eq!(ring.tray_text(), case.want, "line {}", case.line);
        assert_eq!(ring.info_text(), case.want, "line {}", case.line);
        seen += 1;
    }
    assert!(seen > 0, "the recording has no legacy_line cases");
}

// ---------------------------------------------------------------------------
// The file sink
// ---------------------------------------------------------------------------

/// The sink writes Go's `fileFormatLine` bytes, recorded by the probe, one line
/// per event in the order they were recorded.
#[test]
fn the_file_sink_writes_gos_line_layout() {
    let dir = temp_dir("file-line");
    let path = dir.join("wire-pod.log");
    let clock = Arc::new(ManualLogClock::new(1, STAMP));
    let ring = Arc::new(LogRing::with_log_file(clock, &path));
    assert!(ring.has_log_file());

    let cases: Vec<Case> = legacystamp_cases()
        .into_iter()
        .filter(|case| case.kind() == "file_line")
        .collect();
    assert!(!cases.is_empty(), "the recording has no file_line cases");

    let mut want = String::new();
    drive(&ring, || {
        for case in &cases {
            emit(
                case.need("level"),
                case.need("comp"),
                case.need("bot"),
                case.need("msg"),
            );
            want.push_str(&case.want);
        }
    });

    let got = fs::read_to_string(&path).expect("the sink file is readable");
    assert_eq!(got, want);

    drop(ring);
    fs::remove_dir_all(&dir).expect("the temporary directory is removable");
}

/// A sink that cannot be opened is disabled for good, with no panic and no
/// effect on the ring. A directory is a path no platform will open for writing.
#[test]
fn an_unopenable_sink_is_disabled_silently() {
    let dir = temp_dir("bad-sink");
    let clock = Arc::new(ManualLogClock::new(1, STAMP));
    let ring = Arc::new(LogRing::with_log_file(clock, &dir));
    assert!(!ring.has_log_file());

    drive(&ring, || tracing::info!("hello"));
    assert_eq!(messages(&ring), ["hello"]);

    drop(ring);
    fs::remove_dir_all(&dir).expect("the temporary directory is removable");
}

// ---------------------------------------------------------------------------
// The layer's three derivations
// ---------------------------------------------------------------------------

/// Go's `Level.String` for each level, recorded by the probe. Go's level is an
/// `int`, so the recording also carries an out-of-range value; `LogLevel` is a
/// closed enum, and the only place Go's default arm survives here is `parse`.
#[test]
fn the_level_names_match_gos_level_string() {
    let mut seen = 0;
    for case in legacystamp_cases()
        .iter()
        .filter(|case| case.kind() == "level_string")
    {
        let go_level: i64 = case
            .need("level")
            .parse()
            .unwrap_or_else(|_| panic!("line {}: the level is not an integer", case.line));
        let got = match go_level {
            0 => LogLevel::Debug.as_str(),
            1 => LogLevel::Info.as_str(),
            2 => LogLevel::Warn.as_str(),
            3 => LogLevel::Error.as_str(),
            _ => LogLevel::parse("out of range").as_str(),
        };
        assert_eq!(got, case.want, "line {}", case.line);
        seen += 1;
    }
    assert!(seen > 0, "the recording has no level_string cases");
}

/// TRACE and DEBUG both land on Go's DEBUG, and the other three map straight
/// across. This is also what proves the layer carries no filter of its own: an
/// `EnvFilter` on this layer with `RUST_LOG` unset admits ERROR only, so four
/// of these five entries would be missing.
#[test]
fn trace_and_debug_both_fold_onto_gos_debug() {
    let (ring, _clock) = fixed_ring(1);
    drive(&ring, || {
        tracing::trace!("t");
        tracing::debug!("d");
        tracing::info!("i");
        tracing::warn!("w");
        tracing::error!("e");
    });

    let entries = ring.get_entries(LogLevel::Debug, 0);
    let levels: Vec<&str> = entries.iter().map(|entry| entry.level.as_str()).collect();
    let msgs: Vec<&str> = entries.iter().map(|entry| entry.msg.as_str()).collect();
    assert_eq!(msgs, ["t", "d", "i", "w", "e"]);
    assert_eq!(levels, ["DEBUG", "DEBUG", "INFO", "WARN", "ERROR"]);

    assert_eq!(
        LogLevel::from_tracing(tracing::Level::TRACE),
        LogLevel::Debug
    );
    assert_eq!(
        LogLevel::from_tracing(tracing::Level::DEBUG),
        LogLevel::Debug
    );
}

/// The component is the explicit field, else the target when the target is one
/// of Go's twelve, else empty.
#[test]
fn the_component_comes_from_the_field_then_the_target_then_nothing() {
    let (ring, _clock) = fixed_ring(1);
    drive(&ring, || {
        // An explicit field beats the target, even a target that is a component.
        tracing::info!(target: "sdkapp", comp = "llm", "one");
        // No field: a target that is one of the twelve becomes the component.
        tracing::info!(target: "mdns", "two");
        // No field and a target that is not one of the twelve: empty, so a
        // module path never reaches the web UI's component column.
        tracing::info!(target: "wirepod_server::sdkapp::stim", "three");
        // The default target, which is this test binary's module path.
        tracing::info!("four");
        // An explicit empty field wins too, which is how a call site keeps its
        // target for `RUST_LOG` while reproducing one of Go's component-less
        // lines.
        tracing::info!(target: "mdns", comp = "", "five");
    });

    let comps: Vec<String> = ring
        .get_entries(LogLevel::Debug, 0)
        .into_iter()
        .map(|entry| entry.comp)
        .collect();
    assert_eq!(comps, ["llm", "mdns", "", "", ""]);
}

/// Every one of Go's twelve names is recognised as a target, and nothing else
/// is.
#[test]
fn only_gos_twelve_component_names_are_recognised() {
    assert_eq!(COMPONENTS.len(), 12);
    for name in COMPONENTS {
        assert_eq!(component_for_target(name), name);
    }
    for target in [
        "",
        "wirepod_core::logger",
        "sdkapps",
        "JDOCS",
        "jdocs::inner",
        "http",
    ] {
        assert_eq!(component_for_target(target), "", "{target}");
    }
}

/// The bot column is the explicit `bot` field, else the `esn` alias, else
/// empty.
#[test]
fn the_bot_comes_from_bot_then_esn_then_nothing() {
    let (ring, _clock) = fixed_ring(1);
    drive(&ring, || {
        tracing::info!(bot = "00000001", esn = "00000002", "one");
        tracing::info!(esn = "00000002", "two");
        tracing::info!("three");
    });

    let bots: Vec<String> = ring
        .get_entries(LogLevel::Debug, 0)
        .into_iter()
        .map(|entry| entry.bot)
        .collect();
    assert_eq!(bots, ["00000001", "00000002", ""]);
}

// ---------------------------------------------------------------------------
// ANSI stripping
// ---------------------------------------------------------------------------

/// The stripped text is what reaches both the entry and the legacy line, which
/// is Go's order: strip once, then build both from the clean string.
#[test]
fn ansi_escapes_are_stripped_from_the_message() {
    let (ring, _clock) = fixed_ring(1);
    drive(&ring, || {
        tracing::info!("\u{1b}[31mred\u{1b}[0m and \u{1b}[1;32mgreen\u{1b}[0m");
    });

    assert_eq!(messages(&ring), ["red and green"]);
    assert_eq!(ring.info_text(), format!("{STAMP}: red and green\n"));
}

/// The scanner reproduces `\x1b\[[0-9;]*[A-Za-z]` under Go's leftmost-match
/// search, including what happens when a run never reaches its letter.
#[test]
fn strip_ansi_reproduces_gos_pattern() {
    assert_eq!(strip_ansi(""), "");
    assert_eq!(strip_ansi("plain"), "plain");
    assert_eq!(strip_ansi("\u{1b}[0m"), "");
    // An empty parameter run still matches.
    assert_eq!(strip_ansi("\u{1b}[K"), "");
    assert_eq!(strip_ansi("\u{1b}[1;31;4mx"), "x");
    assert_eq!(strip_ansi("a\u{1b}[31mb\u{1b}[0mc"), "abc");

    // No closing letter, so no match, so the bytes survive verbatim.
    assert_eq!(strip_ansi("\u{1b}"), "\u{1b}");
    assert_eq!(strip_ansi("\u{1b}x"), "\u{1b}x");
    assert_eq!(strip_ansi("\u{1b}["), "\u{1b}[");
    assert_eq!(strip_ansi("\u{1b}[12"), "\u{1b}[12");
    assert_eq!(strip_ansi("\u{1b}[12!"), "\u{1b}[12!");
    // The search restarts after the escape, not after the failed run, so the
    // second sequence is still found.
    assert_eq!(strip_ansi("\u{1b}[1\u{1b}[0mX"), "\u{1b}[1X");

    // Non-ASCII text passes through untouched.
    assert_eq!(strip_ansi("\u{1b}[31mré\u{1b}[0m"), "ré");
    assert_eq!(strip_ansi("héllo"), "héllo");
}
