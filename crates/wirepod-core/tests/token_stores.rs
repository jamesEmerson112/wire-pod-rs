//! The three transient token stores, and the three walk quirks they carry.
//!
//! Every expectation about a store's contents, a walk's outcome or a removal's
//! log text is read from
//! `docs/phases/P1-robot-connect-auth/store-probe/expected.txt`, which is the
//! recorded stdout of a Go program that copies
//! `chipper/pkg/servers/token/token.go:36-45`, `:130-146`, `:207-217` and
//! `chipper/pkg/servers/jdocs/server.go:81-130` verbatim, with `logger.Debug`
//! captured into a slice and each panicking case wrapped in a `recover`. The
//! probe's own `main.go` names every Go line it reproduces. Nothing in this
//! file writes down a store shape or a Go log line of its own: a case is named
//! and the recording decides what it should produce, so a disagreement with Go
//! cannot be resolved by editing a literal here.
//!
//! [`replay_split`], [`replay_remove`], [`replay_primary_walk`],
//! [`replay_session`] and [`replay_secondary`] are the five drivers.
//! [`every_recorded_case_replays_against_the_port`] runs every case through
//! them, and the named tests below each drive one case so that the quirk it
//! records keeps a name and a paragraph of prose. The census test pins the
//! exact number of cases and the exact set of kinds per section, so a case the
//! recording gains or loses fails a test rather than quietly going unreplayed.
//!
//! Nothing here writes a real GUID, hash or address. `g-a` and friends are the
//! recording's placeholders and the addresses are documentation-range literals.
//!
//! The log assertions go through a real `tracing` subscriber rather than
//! driving [`LogRing::record`], so the layer's target filter, level folding and
//! component derivation are covered along with the lines themselves.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::time::timeout;
use tracing_subscriber::layer::SubscriberExt;
use wirepod_core::logger::{LogLayer, LogLevel, LogRing, ManualLogClock};
use wirepod_core::test_support::install_tracing_backstop;
use wirepod_core::token::stores::{
    PrimaryEntry, SecondaryEntry, SessionEntry, TokenStores, host_of,
};

/// The Go recording every expectation below is read from.
const EXPECTED: &str = include_str!("data/store-probe/expected.txt");

/// The stamp the ring is fixed at. Nothing here reads it; it only has to be
/// stable so no assertion depends on the wall clock.
const STAMP: &str = "2026.01.02 03:04:05";

/// A generous real-clock ceiling for the two tests that await anything. It is
/// far longer than the work needs, so a regression that deadlocks the store's
/// mutex fails the test instead of hanging the suite.
const CEILING: Duration = Duration::from_secs(10);

/// How many non-comment lines the recording holds, and how many cases each
/// section holds. The counts are literals rather than a second count of the
/// same filtered list, so a case the probe stops producing fails here instead
/// of quietly running one test fewer.
const RECORDED_LINES: usize = 152;
/// Cases in the `split` section.
const SPLIT_CASES: usize = 7;
/// Cases in the `remove` section.
const REMOVE_CASES: usize = 8;
/// Cases in the `primary_walk` section.
const PRIMARY_WALK_CASES: usize = 9;
/// Cases in the `session` section: four lookups and two presence checks.
const SESSION_CASES: usize = 6;
/// Cases in the `secondary` section.
const SECONDARY_CASES: usize = 2;

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

/// One case of the recording: every line that shares a section and an input
/// column apart from its `kind=` pair.
///
/// The probe writes one line per observable of a case, so a case is assembled
/// from several lines rather than parsed out of one. [`Self::key`] is the input
/// column with the `kind=` pair removed, which the probe's format guarantees is
/// unique within a section.
struct Recorded {
    /// The line the first of the case's lines appeared on, for a failure
    /// message that points at the recording.
    line: usize,
    /// The input column without its `kind=` pair, e.g.
    /// `case=primary_skip peer=10.0.0.1`.
    key: String,
    /// The input pairs other than `kind=`.
    inputs: BTreeMap<String, String>,
    /// `kind=` value to that line's unquoted output.
    fields: BTreeMap<String, String>,
}

impl Recorded {
    /// One recorded observable of this case, which must be present.
    fn field(&self, kind: &str) -> &str {
        self.fields
            .get(kind)
            .unwrap_or_else(|| panic!("line {}: case {} has no {kind} line", self.line, self.key))
            .as_str()
    }

    /// One recorded observable, or `None` where the section does not record it
    /// for this case.
    fn opt_field(&self, kind: &str) -> Option<&str> {
        self.fields.get(kind).map(String::as_str)
    }

    /// One input of this case, which must be present.
    fn input(&self, name: &str) -> &str {
        self.inputs
            .get(name)
            .unwrap_or_else(|| panic!("line {}: case {} has no {name}= pair", self.line, self.key))
            .as_str()
    }

    /// The `index=` input as a `usize`.
    fn index(&self) -> usize {
        self.input("index")
            .parse()
            .unwrap_or_else(|e| panic!("line {}: index= is not a number: {e}", self.line))
    }
}

/// Every case of one section, keyed by [`Recorded::key`] so the map's order is
/// the section's own and a named test can ask for one case by name.
fn recorded(section: &str) -> BTreeMap<String, Recorded> {
    let mut cases: BTreeMap<String, Recorded> = BTreeMap::new();
    for (index, raw) in EXPECTED.lines().enumerate() {
        let line = index + 1;
        let text = raw.strip_suffix('\r').unwrap_or(raw);
        if text.starts_with('#') || text.is_empty() {
            continue;
        }
        let mut columns = text.split('\t');
        let found = columns
            .next()
            .unwrap_or_else(|| panic!("line {line}: no section column"));
        if found != section {
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

        let mut kind = None;
        let mut inputs = BTreeMap::new();
        let mut key = Vec::new();
        for pair in input.split(' ') {
            let (name, value) = pair
                .split_once('=')
                .unwrap_or_else(|| panic!("line {line}: {pair} is not key=value"));
            if name == "kind" {
                assert!(kind.is_none(), "line {line}: two kind= pairs");
                assert!(key.is_empty(), "line {line}: the first pair is not kind=");
                kind = Some(value.to_owned());
                continue;
            }
            inputs.insert(name.to_owned(), value.to_owned());
            key.push(pair);
        }
        let kind = kind.unwrap_or_else(|| panic!("line {line}: no kind= pair"));
        let key = key.join(" ");

        let case = cases.entry(key.clone()).or_insert_with(|| Recorded {
            line,
            key: key.clone(),
            inputs,
            fields: BTreeMap::new(),
        });
        assert!(
            case.fields
                .insert(kind.clone(), unquote(output, line))
                .is_none(),
            "line {line}: {key} records {kind} twice"
        );
    }
    assert!(!cases.is_empty(), "the recording has no {section} section");
    cases
}

/// One case by name, for a named test that drives exactly one of them.
fn recorded_case(section: &str, key: &str) -> Recorded {
    recorded(section)
        .remove(key)
        .unwrap_or_else(|| panic!("the recording's {section} section has no case {key}"))
}

/// The index and the length Go's `index out of range [N] with length M` names.
fn panic_bounds(text: &str, line: usize) -> (usize, usize) {
    let (_, rest) = text
        .split_once('[')
        .unwrap_or_else(|| panic!("line {line}: {text} is not an index-out-of-range panic"));
    let (index, rest) = rest
        .split_once(']')
        .unwrap_or_else(|| panic!("line {line}: {text} has no closing bracket"));
    let length = rest
        .rsplit(' ')
        .next()
        .unwrap_or_else(|| panic!("line {line}: {text} names no length"));
    (
        index.parse().expect("the panic's index is a number"),
        length.parse().expect("the panic's length is a number"),
    )
}

// ---------------------------------------------------------------------------
// The recording's store encoding
// ---------------------------------------------------------------------------

/// Splits the recording's `a;b;c` list, answering nothing for the empty string
/// rather than one empty element.
fn entries(encoded: &str) -> Vec<&str> {
    if encoded.is_empty() {
        Vec::new()
    } else {
        encoded.split(';').collect()
    }
}

/// The slots of one recorded entry, which must be exactly `want` of them.
fn slots(entry: &str, want: usize) -> Vec<&str> {
    let parts: Vec<&str> = entry.split('/').collect();
    assert_eq!(parts.len(), want, "{entry} does not hold {want} slots");
    parts
}

fn decode_primary(encoded: &str) -> Vec<PrimaryEntry> {
    entries(encoded)
        .into_iter()
        .map(|entry| {
            let slot = slots(entry, 3);
            PrimaryEntry {
                target: slot[0].to_owned(),
                guid: slot[1].to_owned(),
                guid_hash: slot[2].to_owned(),
            }
        })
        .collect()
}

fn encode_primary(store: &[PrimaryEntry]) -> String {
    store
        .iter()
        .map(|e| format!("{}/{}/{}", e.target, e.guid, e.guid_hash))
        .collect::<Vec<_>>()
        .join(";")
}

fn decode_secondary(encoded: &str) -> Vec<SecondaryEntry> {
    entries(encoded)
        .into_iter()
        .map(|entry| {
            let slot = slots(entry, 4);
            SecondaryEntry {
                esn: slot[0].to_owned(),
                target: slot[1].to_owned(),
                guid: slot[2].to_owned(),
                guid_hash: slot[3].to_owned(),
            }
        })
        .collect()
}

fn encode_secondary(store: &[SecondaryEntry]) -> String {
    store
        .iter()
        .map(|e| format!("{}/{}/{}/{}", e.esn, e.target, e.guid, e.guid_hash))
        .collect::<Vec<_>>()
        .join(";")
}

fn decode_session(encoded: &str) -> Vec<SessionEntry> {
    entries(encoded)
        .into_iter()
        .map(|entry| {
            let slot = slots(entry, 3);
            SessionEntry {
                peer_addr: slot[0].to_owned(),
                name: slot[1].to_owned(),
                cert: slot[2].as_bytes().to_vec(),
            }
        })
        .collect()
}

fn encode_session(store: &[SessionEntry]) -> String {
    store
        .iter()
        .map(|e| {
            let cert =
                std::str::from_utf8(&e.cert).expect("the recording's certificates are ASCII");
            format!("{}/{}/{}", e.peer_addr, e.name, cert)
        })
        .collect::<Vec<_>>()
        .join(";")
}

// ---------------------------------------------------------------------------
// The log ring
// ---------------------------------------------------------------------------

/// A ring on a clock fixed at [`STAMP`].
fn fixed_ring() -> Arc<LogRing> {
    Arc::new(LogRing::new(Arc::new(ManualLogClock::new(0, STAMP))))
}

/// Runs `body` with `ring` installed as this thread's subscriber, answering
/// whatever it answered.
///
/// [`install_tracing_backstop`] first, for the reason its own documentation
/// gives: a callsite first reached by a thread with no subscriber is cached as
/// never-interested for the rest of the process, and the harness decides which
/// test gets there first.
fn drive<T>(ring: &Arc<LogRing>, body: impl FnOnce() -> T) -> T {
    install_tracing_backstop();
    let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(ring)));
    tracing::subscriber::with_default(subscriber, body)
}

/// Every entry the ring holds, as `(level, comp, bot, msg)`. The clock is fixed
/// at zero, so `-1` is the only `since` that admits anything.
fn lines(ring: &LogRing) -> Vec<(String, String, String, String)> {
    ring.get_entries(LogLevel::Debug, -1)
        .into_iter()
        .map(|entry| (entry.level, entry.comp, entry.bot, entry.msg))
        .collect()
}

/// The `(bot, msg)` of every `DEBUG` line, asserting each is under the token
/// component.
fn debug_lines(written: &[(String, String, String, String)]) -> Vec<(&str, &str)> {
    written
        .iter()
        .filter(|(level, ..)| level == "DEBUG")
        .map(|(_, comp, bot, msg)| {
            assert_eq!(comp, "token", "every line here is the token server's");
            (bot.as_str(), msg.as_str())
        })
        .collect()
}

/// The message of every `WARN` line, asserting each is the port's own overrun
/// report: token component, empty bot column.
fn warn_lines(written: &[(String, String, String, String)]) -> Vec<&str> {
    written
        .iter()
        .filter(|(level, ..)| level == "WARN")
        .map(|(_, comp, bot, msg)| {
            assert_eq!(comp, "token");
            assert_eq!(bot, "", "the overrun report fills no bot column");
            msg.as_str()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Fixtures for the tests the recording does not drive
// ---------------------------------------------------------------------------

/// A primary entry with the recording's shape: a bare host and a placeholder
/// GUID, with the hash derived from it so the two slots can never be confused.
fn primary(target: &str, guid: &str) -> PrimaryEntry {
    PrimaryEntry {
        target: target.to_owned(),
        guid: guid.to_owned(),
        guid_hash: format!("{guid}-hash"),
    }
}

/// A secondary entry, whose four slots are all distinguishable.
fn secondary(esn: &str, target: &str, guid: &str) -> SecondaryEntry {
    SecondaryEntry {
        esn: esn.to_owned(),
        target: target.to_owned(),
        guid: guid.to_owned(),
        guid_hash: format!("{guid}-hash"),
    }
}

/// A session entry whose certificate bytes name the entry, so a removal that
/// took the wrong one is visible.
fn session(peer_addr: &str, name: &str) -> SessionEntry {
    SessionEntry {
        peer_addr: peer_addr.to_owned(),
        name: name.to_owned(),
        cert: format!("cert-{name}").into_bytes(),
    }
}

// ---------------------------------------------------------------------------
// The census
// ---------------------------------------------------------------------------

/// The recording's exact shape.
///
/// Every section's case count and every kind it records is pinned here, so a
/// case the probe gains and nothing replays, or a kind that stopped being
/// produced, fails this test rather than silently shrinking the coverage below.
#[test]
fn the_recording_holds_exactly_the_cases_this_file_replays() {
    let counted = EXPECTED
        .lines()
        .filter(|raw| {
            let text = raw.strip_suffix('\r').unwrap_or(raw);
            !text.starts_with('#') && !text.is_empty()
        })
        .count();
    assert_eq!(
        counted, RECORDED_LINES,
        "non-comment lines in the recording"
    );

    let census = |section: &str| -> (usize, BTreeMap<String, usize>) {
        let cases = recorded(section);
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for case in cases.values() {
            for kind in case.fields.keys() {
                *kinds.entry(kind.clone()).or_default() += 1;
            }
        }
        (cases.len(), kinds)
    };
    let kinds = |pairs: &[(&str, usize)]| -> BTreeMap<String, usize> {
        pairs
            .iter()
            .map(|(name, count)| ((*name).to_owned(), *count))
            .collect()
    };

    assert_eq!(
        census("split"),
        (SPLIT_CASES, kinds(&[("split", SPLIT_CASES)]))
    );
    assert_eq!(
        census("remove"),
        (
            REMOVE_CASES,
            kinds(&[
                ("after", REMOVE_CASES),
                ("log_bot", REMOVE_CASES),
                ("log_msg", REMOVE_CASES),
                ("panic", REMOVE_CASES),
                ("store", REMOVE_CASES),
            ])
        )
    );
    assert_eq!(
        census("primary_walk"),
        (
            PRIMARY_WALK_CASES,
            kinds(&[
                ("after", PRIMARY_WALK_CASES),
                ("bot_guid", PRIMARY_WALK_CASES),
                ("log_msgs", PRIMARY_WALK_CASES),
                ("matched", PRIMARY_WALK_CASES),
                ("matches", PRIMARY_WALK_CASES),
                ("panic", PRIMARY_WALK_CASES),
                ("store", PRIMARY_WALK_CASES),
            ])
        )
    );
    // Four lookup cases record six observables each; the two presence cases
    // record the store they ran against and the answer.
    assert_eq!(
        census("session"),
        (
            SESSION_CASES,
            kinds(&[
                ("after", 4),
                ("found_cert", 4),
                ("found_index", 4),
                ("found_name", 4),
                ("log_msg", 4),
                ("presence", 2),
                ("store", SESSION_CASES),
            ])
        )
    );
    assert_eq!(
        census("secondary"),
        (
            SECONDARY_CASES,
            kinds(&[
                ("after", SECONDARY_CASES),
                ("found_guid", SECONDARY_CASES),
                ("found_hash", SECONDARY_CASES),
                ("found_index", SECONDARY_CASES),
                ("log_bot", SECONDARY_CASES),
                ("log_msg", SECONDARY_CASES),
                ("store", SECONDARY_CASES),
            ])
        )
    );
}

/// Every case in the recording, through the driver its section belongs to.
///
/// The named tests below drive one case each and say what it records; this one
/// exists so that a case none of them names is still replayed.
#[test]
fn every_recorded_case_replays_against_the_port() {
    for case in recorded("split").values() {
        replay_split(case);
    }
    for case in recorded("remove").values() {
        replay_remove(case);
    }
    for case in recorded("primary_walk").values() {
        replay_primary_walk(case);
    }
    for case in recorded("session").values() {
        replay_session(case);
    }
    for case in recorded("secondary").values() {
        replay_secondary(case);
    }
}

// ---------------------------------------------------------------------------
// host_of
// ---------------------------------------------------------------------------

/// Go's `strings.Split(addr, ":")[0]` against [`host_of`].
fn replay_split(case: &Recorded) {
    assert_eq!(
        host_of(case.input("addr")),
        case.field("split"),
        "line {}: {}",
        case.line,
        case.key
    );
}

#[test]
fn host_of_cuts_at_the_first_colon() {
    // The recording's four ASCII cases: a host and port, a bare host, the empty
    // string, and a leading colon. Go's `Split` on a separator that is absent
    // answers a one-element slice, so its `[0]` never panics and neither does
    // this.
    for key in [
        "addr=10.0.0.1:50000",
        "addr=10.0.0.1",
        "addr=",
        "addr=:50000",
    ] {
        replay_split(&recorded_case("split", key));
    }
}

#[test]
fn host_of_degenerates_on_an_ipv6_peer_the_way_go_does() {
    // For an IPv6 peer Go's own `net.Addr.String()` writes `[::1]:50000`, whose
    // first colon is inside the address, so the split answers `[` rather than a
    // host and two different peers share one key. The port reproduces that
    // rather than fixing it, because the stored targets were cut the same way
    // going in and the two halves have to agree.
    let loopback = recorded_case("split", "addr=[::1]:50000");
    let other = recorded_case("split", "addr=[::2]:50000");
    replay_split(&loopback);
    replay_split(&other);
    replay_split(&recorded_case("split", "addr=[fe80::1]:443"));
    assert_eq!(
        loopback.field("split"),
        other.field("split"),
        "two peers, one key"
    );
}

// ---------------------------------------------------------------------------
// The appends
// ---------------------------------------------------------------------------

#[test]
fn the_three_appends_keep_every_slot_and_their_order() {
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.1", "g-a"));
    stores.add_primary(primary("10.0.0.9", "g-c"));
    stores.add_secondary(secondary("00aaaa01", "10.0.0.7", "g-x"));
    stores.add_session(session("10.0.0.1:50000", "Vector-AAA"));

    assert_eq!(
        encode_primary(&stores.primary_snapshot()),
        "10.0.0.1/g-a/g-a-hash;10.0.0.9/g-c/g-c-hash",
        "appends keep insertion order, which every walk's index depends on"
    );
    assert_eq!(stores.primary_len(), 2);

    let secondaries = stores.secondary_snapshot();
    assert_eq!(secondaries.len(), 1);
    assert_eq!(secondaries[0].esn, "00aaaa01", "slot 0 is the serial");
    assert_eq!(secondaries[0].target, "10.0.0.7", "slot 1 is the host");
    assert_eq!(secondaries[0].guid, "g-x", "slot 2 is the GUID");
    assert_eq!(secondaries[0].guid_hash, "g-x-hash", "slot 3 is the hash");

    let sessions = stores.session_snapshot();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].peer_addr, "10.0.0.1:50000",
        "the session store keeps the port, where the other two do not"
    );
    assert_eq!(sessions[0].name, "Vector-AAA");
    assert_eq!(sessions[0].cert, b"cert-Vector-AAA");
}

#[test]
fn the_appends_write_no_log_line() {
    // Go's only log line near an append is `CreateJWT`'s
    // `Adding <ip> to TokenHashStore` at `token.go:234`, which sits at the call
    // site rather than in a helper and belongs to the commit that ports
    // `CreateJWT`.
    let ring = fixed_ring();
    let stores = TokenStores::new();
    drive(&ring, || {
        stores.add_primary(primary("10.0.0.1", "g-a"));
        stores.add_secondary(secondary("00aaaa01", "10.0.0.7", "g-x"));
        stores.add_session(session("10.0.0.1:50000", "Vector-AAA"));
    });
    assert!(lines(&ring).is_empty());
}

// ---------------------------------------------------------------------------
// The removals by index
// ---------------------------------------------------------------------------

/// One `remove` case: the named removal helper called with the recorded index
/// against the recorded store.
///
/// The survivors are compared as the recording's own encoding, which is what
/// pins Go's order-preserving `append(s[:i], s[i+1:]...)`: a removal that swaps
/// the last element into the hole leaves the same entries in another order and
/// fails here.
///
/// Where Go panicked, the port logs and answers `false`, so the recorded panic
/// text decides which of the two arms below runs. The index and the length Go's
/// panic names are compared with the port's own warning, so the two cannot
/// drift apart.
fn replay_remove(case: &Recorded) {
    let ring = fixed_ring();
    let stores = TokenStores::new();
    let index = case.index();
    let panicked = case.field("panic");

    let (removed, after, go_name, length) = match case.input("list") {
        "primary" => {
            let store = decode_primary(case.field("store"));
            let length = store.len();
            for entry in store {
                stores.add_primary(entry);
            }
            let removed = drive(&ring, || stores.remove_from_primary_store(index));
            (
                removed,
                encode_primary(&stores.primary_snapshot()),
                "TokenHashStore",
                length,
            )
        }
        "secondary" => {
            let store = decode_secondary(case.field("store"));
            let length = store.len();
            for entry in store {
                stores.add_secondary(entry);
            }
            let removed = drive(&ring, || stores.remove_from_second_store(index));
            (
                removed,
                encode_secondary(&stores.secondary_snapshot()),
                "SecondaryTokenStore",
                length,
            )
        }
        "session" => {
            let store = decode_session(case.field("store"));
            let length = store.len();
            for entry in store {
                stores.add_session(entry);
            }
            let removed = drive(&ring, || stores.remove_from_session_store(index));
            (
                removed,
                encode_session(&stores.session_snapshot()),
                "SessionWriteStoreNames",
                length,
            )
        }
        other => panic!("line {}: unknown list {other}", case.line),
    };

    assert_eq!(
        removed,
        panicked.is_empty(),
        "line {}: Go removes an entry exactly where it does not panic",
        case.line
    );
    assert_eq!(
        after,
        case.field("after"),
        "line {}: {} left the wrong survivors, or left them in the wrong order",
        case.line,
        case.key
    );

    let written = lines(&ring);
    if panicked.is_empty() {
        assert_eq!(
            debug_lines(&written),
            [(case.field("log_bot"), case.field("log_msg"))],
            "line {}: one removal, one line, both columns Go's",
            case.line
        );
        assert!(warn_lines(&written).is_empty());
    } else {
        // Go indexes the slice to build its log line and the runtime stops the
        // process, so it writes nothing.
        assert_eq!(case.field("log_msg"), "", "Go logged nothing before dying");
        assert_eq!(case.field("log_bot"), "");
        assert!(debug_lines(&written).is_empty());
        let (go_index, go_length) = panic_bounds(panicked, case.line);
        assert_eq!((go_index, go_length), (index, length));
        assert_eq!(
            warn_lines(&written),
            [format!(
                "index {go_index} is out of range for {go_name}, which holds {go_length}; \
                 Go panics here, so nothing was removed"
            )
            .as_str()],
            "line {}: the port's line names Go's own index and length",
            case.line
        );
    }
}

#[test]
fn a_removal_takes_the_named_index_and_leaves_the_rest_in_order() {
    // Index 0 of three is the only index that separates Go's
    // `append(s[:i], s[i+1:]...)`, which shifts every later element down and
    // keeps their order, from a removal that swaps the last element into the
    // hole. The recording holds one of these per list.
    replay_remove(&recorded_case(
        "remove",
        "case=primary_remove_first list=primary index=0",
    ));
    replay_remove(&recorded_case(
        "remove",
        "case=secondary_remove_first list=secondary index=0",
    ));
    replay_remove(&recorded_case(
        "remove",
        "case=session_remove_first list=session index=0",
    ));
    replay_remove(&recorded_case(
        "remove",
        "case=primary_remove_middle list=primary index=1",
    ));
}

#[test]
fn the_three_removals_write_gos_lines_at_debug_under_the_token_component() {
    // `token.go:136` interpolates the target and passes `""` for the bot
    // column, `token.go:131` is the only one of the three that fills the bot
    // column and the only one whose message interpolates nothing, and
    // `token.go:143` names the whole peer address, port and all. All three
    // columns come from the recording; [`replay_remove`] asserts them.
    for key in [
        "case=primary_remove_first list=primary index=0",
        "case=secondary_remove_first list=secondary index=0",
        "case=session_remove_first list=session index=0",
    ] {
        let case = recorded_case("remove", key);
        replay_remove(&case);
        assert!(
            !case.field("log_msg").is_empty(),
            "every successful removal writes a line"
        );
    }
}

#[test]
fn an_index_past_the_end_removes_nothing_and_says_so() {
    // Go indexes the slice to build its log line and the runtime stops the
    // process; the recording holds the panic text for each of the three
    // helpers. This follows the same policy as reserved deviation 31, which is
    // the phase's standing decision to turn a Go panic into a log line but does
    // not itself list this panic among the four it names. Nothing is removed
    // either way, so the surviving entries match Go's exactly; only the process
    // surviving differs.
    for key in [
        "case=primary_remove_out_of_range list=primary index=1",
        "case=secondary_remove_out_of_range list=secondary index=5",
        "case=session_remove_out_of_range list=session index=1",
    ] {
        let case = recorded_case("remove", key);
        replay_remove(&case);
        assert!(
            case.field("panic")
                .starts_with("runtime error: index out of range"),
            "the recording holds Go's own panic for {key}"
        );
        assert_eq!(
            case.field("after"),
            case.field("store"),
            "Go panics before it removes anything"
        );
    }
}

#[test]
fn removing_a_session_entry_drops_the_name_and_the_certificate_together() {
    // Go shortens `SessionWriteStoreNames` and `SessionWriteStoreCerts` by the
    // same index (`token.go:144-145`), and every reader indexes the cert slice
    // with the name slice's index (`jdocs/server.go:121`). One list makes that
    // structural, and the recording's encoding carries each entry's certificate
    // beside its name, so a surviving pair that had come apart fails.
    replay_remove(&recorded_case(
        "remove",
        "case=session_remove_first list=session index=0",
    ));
}

// ---------------------------------------------------------------------------
// The primary walk: the skip quirk, and where Go's process dies
// ---------------------------------------------------------------------------

/// One `primary_walk` case: `ReadDocs`'s walk over the recorded store.
///
/// `matches` is the GUID of every entry Go's loop body ran on, in visit order,
/// which is neither the set of entries removed nor deduplicated. `panic` is
/// non-empty exactly where Go's removal helper would have indexed past the end
/// of the shortened slice, which is [`PrimaryWalk::overran`] here.
fn replay_primary_walk(case: &Recorded) {
    let ring = fixed_ring();
    let stores = TokenStores::new();
    let store = decode_primary(case.field("store"));
    let length = store.len();
    for entry in store {
        stores.add_primary(entry);
    }

    let peer = case.input("peer");
    let walk = drive(&ring, || stores.take_primary_matches(peer));

    assert_eq!(
        walk.matches
            .iter()
            .map(|entry| entry.guid.as_str())
            .collect::<Vec<_>>()
            .join(";"),
        case.field("matches"),
        "line {}: {} ran the loop body on the wrong entries",
        case.line,
        case.key
    );
    assert_eq!(
        encode_primary(&stores.primary_snapshot()),
        case.field("after"),
        "line {}: {} left the wrong store",
        case.line,
        case.key
    );
    assert_eq!(walk.matched().to_string(), case.field("matched"));
    assert_eq!(walk.bot_guid(), case.field("bot_guid"));

    let written = lines(&ring);
    let recorded_msgs: Vec<(&str, &str)> = if case.field("log_msgs").is_empty() {
        Vec::new()
    } else {
        // Every line the walk writes comes from `token.go:136`, which passes the
        // empty string for the bot column.
        case.field("log_msgs")
            .split('\n')
            .map(|msg| ("", msg))
            .collect()
    };
    assert_eq!(
        debug_lines(&written),
        recorded_msgs,
        "line {}: {} wrote the wrong removal lines",
        case.line,
        case.key
    );

    let panicked = case.field("panic");
    assert_eq!(
        walk.overran,
        !panicked.is_empty(),
        "line {}: the walk overran exactly where Go's process died",
        case.line
    );
    if panicked.is_empty() {
        assert!(warn_lines(&written).is_empty());
    } else {
        let (go_index, go_length) = panic_bounds(panicked, case.line);
        assert!(
            go_index < length,
            "the index Go died on is one the loop could reach"
        );
        assert_eq!(
            warn_lines(&written),
            [format!(
                "index {go_index} is out of range for TokenHashStore, which holds {go_length}; \
                 Go panics here, so nothing was removed"
            )
            .as_str()],
            "line {}: the port's warning names Go's own index and length",
            case.line
        );
    }
}

#[test]
fn the_primary_walk_skips_the_entry_the_removal_shifted_down() {
    // The recording's `primary_skip`: entries 0 and 1 both match and entry 2
    // does not. A Go `range` evaluates its expression once, so removing entry 0
    // shifts entry 1 into index 0 and the loop, already past index 0, never sees
    // it. Entry 1 therefore survives even though it matched, because the removal
    // shifted it into an index the loop had already passed, and entry 2 is read
    // twice: once where it was shifted to and once from the stale tail of the
    // backing array. Go's `botGUID` is entry 0's.
    replay_primary_walk(&recorded_case(
        "primary_walk",
        "case=primary_skip peer=10.0.0.1",
    ));
}

#[test]
fn the_primary_walk_with_one_match_removes_exactly_it() {
    // The recording's `primary_first_only`: the same shift happens, but the
    // entry shifted past did not match, so the outcome is the obvious one.
    replay_primary_walk(&recorded_case(
        "primary_walk",
        "case=primary_first_only peer=10.0.0.1",
    ));
}

#[test]
fn the_primary_walk_matches_the_last_entry_without_skipping_anything() {
    // The recording's `primary_second_only`: nothing has shifted by the time the
    // loop reaches the match, so there is no stale read to make.
    replay_primary_walk(&recorded_case(
        "primary_walk",
        "case=primary_second_only peer=10.0.0.1",
    ));
}

#[test]
fn the_primary_walk_folds_case_on_the_target() {
    // `jdocs/server.go:95` is `strings.EqualFold`.
    replay_primary_walk(&recorded_case(
        "primary_walk",
        "case=primary_equalfold peer=localhost",
    ));
}

#[test]
fn the_primary_walk_with_no_match_changes_nothing() {
    // The recording's `primary_no_match`, whose `bot_guid` is the empty string
    // Go's `botGUID` starts at.
    replay_primary_walk(&recorded_case(
        "primary_walk",
        "case=primary_no_match peer=10.0.0.2",
    ));
}

#[test]
fn the_primary_walk_on_an_empty_store_does_nothing() {
    replay_primary_walk(&recorded_case(
        "primary_walk",
        "case=primary_empty peer=10.0.0.1",
    ));
}

#[test]
fn the_primary_walk_logs_one_line_per_removal() {
    // The recording's `primary_first_only`, whose entry 1 carries a different
    // target from entry 0. Go builds the line before the shift
    // (`token.go:136-137`), so the line names the entry being removed rather
    // than the one shifted into its index; against this store those two differ,
    // and a line written after the shift would name entry 1's target.
    let case = recorded_case("primary_walk", "case=primary_first_only peer=10.0.0.1");
    replay_primary_walk(&case);
    let store = decode_primary(case.field("store"));
    assert_ne!(
        store[0].target, store[1].target,
        "the shifted-in entry has to differ, or this pins nothing"
    );
}

#[test]
fn the_walks_removal_line_names_the_stored_target_not_the_peer() {
    // `token.go:136` interpolates `TokenHashStore[index][0]`, the entry's own
    // target, and the comparison that reached it folds case, so the two can
    // differ. Go writes what the store held, which the recording's
    // `primary_equalfold` case shows: the peer is `localhost` and the line names
    // `LOCALHOST`.
    let case = recorded_case("primary_walk", "case=primary_equalfold peer=localhost");
    replay_primary_walk(&case);
    assert!(
        case.field("log_msgs").contains("LOCALHOST"),
        "the recorded line names the stored spelling, not the peer's"
    );
}

#[test]
fn the_primary_walk_stops_where_two_duplicates_make_go_panic() {
    // The recording's `primary_two_duplicates`, which is the realistic shape:
    // one robot asked for a token twice before its `ReadDocs` arrived. Once the
    // length has shrunk below the loop's, the second match calls the removal
    // helper with an index the package variable no longer has, and the helper
    // indexes it to build its log line and dies. The port stops there instead,
    // which leaves the store holding exactly what Go's holds at that instant.
    replay_primary_walk(&recorded_case(
        "primary_walk",
        "case=primary_two_duplicates peer=10.0.0.1",
    ));
}

#[test]
fn the_primary_walk_stops_after_reading_the_stale_tail_twice() {
    // The recording's `primary_zero_and_two`: entries 0 and 2 match and entry 1
    // does not, so entry 2 is shifted into index 1, read there, and read again
    // from the stale tail at index 2. Three matches out of a two-match store,
    // and Go's `botGUID` is the last one's.
    replay_primary_walk(&recorded_case(
        "primary_walk",
        "case=primary_zero_and_two peer=10.0.0.1",
    ));
}

#[test]
fn the_primary_walk_stops_when_all_three_entries_match() {
    // The recording's `primary_all_three`, which reaches the same panic and
    // leaves the entry that was shifted past.
    replay_primary_walk(&recorded_case(
        "primary_walk",
        "case=primary_all_three peer=10.0.0.1",
    ));
}

#[test]
fn an_overrun_walk_logs_the_removals_it_made_and_then_the_warning() {
    // The removals Go made are written first and the port's own `WARN` follows,
    // which is what [`replay_primary_walk`] asserts: Go's recorded messages in
    // order, then one warning naming Go's own index and length.
    let case = recorded_case("primary_walk", "case=primary_two_duplicates peer=10.0.0.1");
    replay_primary_walk(&case);

    let ring = fixed_ring();
    let stores = TokenStores::new();
    for entry in decode_primary(case.field("store")) {
        stores.add_primary(entry);
    }
    drive(&ring, || {
        assert!(stores.take_primary_matches(case.input("peer")).overran);
    });
    let written = lines(&ring);
    assert_eq!(written.len(), 2);
    assert_eq!(written[0].0, "DEBUG", "the removal Go made comes first");
    assert_eq!(written[1].0, "WARN", "and the port's own report follows it");
}

// ---------------------------------------------------------------------------
// The session store's two walks
// ---------------------------------------------------------------------------

/// One `session` case: either the lookup at `jdocs/server.go:111-130` followed
/// by the caller's removal, or the presence check at `:81-86`.
///
/// The lookup and the removal are separate calls here because Go writes two
/// files and two more records between finding the entry and removing it, each
/// with its own log line, so a lookup that removed as it found would put the
/// removal's line in the wrong place.
fn replay_session(case: &Recorded) {
    let stores = TokenStores::new();
    for entry in decode_session(case.field("store")) {
        stores.add_session(entry);
    }
    let peer = case.input("peer");

    if let Some(presence) = case.opt_field("presence") {
        assert_eq!(
            stores.session_holds(peer).to_string(),
            presence,
            "line {}: {} answered the wrong presence",
            case.line,
            case.key
        );
        assert_eq!(
            encode_session(&stores.session_snapshot()),
            case.field("store"),
            "the presence check removes nothing"
        );
        return;
    }

    let ring = fixed_ring();
    let found = drive(&ring, || {
        let found = stores.find_session_match(peer);
        if let Some(matched) = &found {
            assert!(stores.remove_from_session_store(matched.index));
        }
        found
    });

    let recorded_index: i64 = case
        .field("found_index")
        .parse()
        .expect("found_index is a number");
    match (&found, recorded_index) {
        (None, -1) => {
            assert_eq!(case.field("found_name"), "");
            assert_eq!(case.field("found_cert"), "");
            assert!(debug_lines(&lines(&ring)).is_empty(), "a miss logs nothing");
        }
        (Some(matched), index) if index >= 0 => {
            assert_eq!(matched.index, index as usize, "line {}", case.line);
            assert_eq!(matched.entry.name, case.field("found_name"));
            assert_eq!(matched.entry.cert, case.field("found_cert").as_bytes());
            assert_eq!(
                debug_lines(&lines(&ring)),
                [("", case.field("log_msg"))],
                "line {}: the removal's line names the whole stored address",
                case.line
            );
        }
        (found, index) => panic!(
            "line {}: the recording says {index} and the port answered {found:?}",
            case.line
        ),
    }

    assert_eq!(
        encode_session(&stores.session_snapshot()),
        case.field("after"),
        "line {}: {} left the wrong store",
        case.line,
        case.key
    );
}

#[test]
fn the_session_lookup_stops_at_the_first_match() {
    // `jdocs/server.go:128` breaks out of the loop after writing one
    // certificate, so a second entry for the same host is left behind. The
    // recording's `session_break` holds two entries for one host and a third for
    // another, and its survivors are the last two in order.
    replay_session(&recorded_case(
        "session",
        "case=session_break peer=10.0.0.1",
    ));
}

#[test]
fn the_session_lookup_splits_the_stored_address_and_folds_case() {
    // `jdocs/server.go:112` is
    // `strings.EqualFold(ipAddr, strings.Split(pair[0], ":")[0])`.
    let case = recorded_case("session", "case=session_equalfold peer=localhost");
    replay_session(&case);

    let stores = TokenStores::new();
    for entry in decode_session(case.field("store")) {
        stores.add_session(entry);
    }
    assert!(
        stores.find_session_match("LOCALHOST:50000").is_none(),
        "the caller passes an already-split host, not a whole address"
    );
}

#[test]
fn the_session_lookup_misses_a_host_no_entry_carries() {
    replay_session(&recorded_case("session", "case=session_miss peer=10.0.0.1"));
}

#[test]
fn the_session_lookup_keys_an_ipv6_peer_on_the_split_that_degenerated() {
    // The stored address `[::1]:50000` splits to `[`, so that is the key the
    // lookup compares and the key the caller arrived with. Both halves were cut
    // the same way, so the entry is still found.
    replay_session(&recorded_case("session", "case=session_ipv6 peer=["));
}

#[test]
fn the_session_presence_check_is_case_sensitive_where_the_lookup_is_not() {
    // `jdocs/server.go:82` compares with `==` while `:112` folds case, twelve
    // lines apart and over the same store, so a peer whose host is spelled in
    // another case is found by one and not the other. The recording holds both
    // spellings against one stored address.
    let lower = recorded_case("session", "case=session_presence peer=localhost");
    let upper = recorded_case("session", "case=session_presence peer=LOCALHOST");
    replay_session(&lower);
    replay_session(&upper);
    assert_ne!(
        lower.field("presence"),
        upper.field("presence"),
        "the recording pins the disagreement"
    );

    let stores = TokenStores::new();
    for entry in decode_session(lower.field("store")) {
        stores.add_session(entry);
    }
    assert!(
        stores.find_session_match(lower.input("peer")).is_some(),
        "the lookup twelve lines later finds what the presence check did not"
    );
}

// ---------------------------------------------------------------------------
// The secondary store
// ---------------------------------------------------------------------------

/// One `secondary` case: `CreateJWT`'s scan at `token.go:207-217`, which
/// compares serials with `==`, breaks at its first match and removes it.
fn replay_secondary(case: &Recorded) {
    let ring = fixed_ring();
    let stores = TokenStores::new();
    let store = decode_secondary(case.field("store"));
    for entry in store.clone() {
        stores.add_secondary(entry);
    }

    let esn = case.input("esn");
    let found = drive(&ring, || stores.take_secondary_match(esn));

    let recorded_index: i64 = case
        .field("found_index")
        .parse()
        .expect("found_index is a number");
    match (&found, recorded_index) {
        (None, -1) => {
            assert_eq!(case.field("found_guid"), "");
            assert_eq!(case.field("found_hash"), "");
            assert!(debug_lines(&lines(&ring)).is_empty(), "a miss logs nothing");
        }
        (Some(entry), index) if index >= 0 => {
            assert_eq!(
                entry, &store[index as usize],
                "line {}: the entry Go's `num` names",
                case.line
            );
            assert_eq!(entry.guid, case.field("found_guid"));
            assert_eq!(entry.guid_hash, case.field("found_hash"));
            assert_eq!(
                debug_lines(&lines(&ring)),
                [(case.field("log_bot"), case.field("log_msg"))],
                "line {}: the one removal line that fills the bot column",
                case.line
            );
        }
        (found, index) => panic!(
            "line {}: the recording says {index} and the port answered {found:?}",
            case.line
        ),
    }

    assert_eq!(
        encode_secondary(&stores.secondary_snapshot()),
        case.field("after"),
        "line {}: {} left the wrong survivors, or left them in the wrong order",
        case.line,
        case.key
    );
}

#[test]
fn the_secondary_scan_stops_at_the_first_match() {
    // The recording's `secondary_walk`: four entries with the match at index 1
    // and a duplicate serial after it. `token.go:214` breaks, so the duplicate
    // survives, and the match is neither the last entry nor the second to last,
    // so the survivors separate Go's order-preserving removal from one that
    // swaps the last element into the hole.
    let case = recorded_case("secondary", "case=secondary_walk esn=00aaaa02");
    replay_secondary(&case);
    let store = decode_secondary(case.field("store"));
    assert_eq!(store.len(), 4, "a shorter store would pin less");
    assert_eq!(
        store[1].esn, store[2].esn,
        "the entry after the match carries the same serial, so the break is visible"
    );
}

#[test]
fn the_secondary_scan_compares_the_serial_exactly() {
    // `token.go:208` is `robot[0] == esn`, where every other serial lookup in
    // the server folds case. The recording's `secondary_case_sensitive` finds
    // nothing against a store holding the same serial in upper case.
    let case = recorded_case("secondary", "case=secondary_case_sensitive esn=00aaaa02");
    replay_secondary(&case);

    let stores = TokenStores::new();
    let store = decode_secondary(case.field("store"));
    for entry in store.clone() {
        stores.add_secondary(entry);
    }
    assert!(
        stores.take_secondary_match(&store[0].esn).is_some(),
        "the stored spelling is found where the folded one was not"
    );
}

#[test]
fn the_secondary_scan_logs_the_serial_in_the_bot_column() {
    // `token.go:131` passes `SecondaryTokenStore[index][0]`, the serial, as the
    // bot column and leaves the message with nothing interpolated into it.
    let case = recorded_case("secondary", "case=secondary_walk esn=00aaaa02");
    replay_secondary(&case);
    assert_eq!(
        case.field("log_bot"),
        case.input("esn"),
        "the bot column is the serial that was matched"
    );
}

#[test]
fn the_dead_secondary_path_leaves_the_store_as_it_found_it() {
    // `jdocs/server.go:136` appends and `:149` removes the element it just
    // appended, with nothing in between that reads the store, so the store ends
    // the request as it began and nothing can ever find what was appended. The
    // recording's `secondary_dead_path` is that removal, against the two-entry
    // store the append leaves.
    let case = recorded_case("remove", "case=secondary_dead_path list=secondary index=1");
    replay_remove(&case);

    let store = decode_secondary(case.field("store"));
    let appended = store.last().expect("the appended entry is the last one");
    let stores = TokenStores::new();
    for entry in decode_secondary(case.field("after")) {
        stores.add_secondary(entry);
    }
    stores.add_secondary(appended.clone());
    assert_eq!(
        stores.secondary_len() - 1,
        case.index(),
        "`ReadDocs` names the element it appended by `len(store) - 1`"
    );
    assert!(stores.remove_from_second_store(stores.secondary_len() - 1));
    assert_eq!(
        encode_secondary(&stores.secondary_snapshot()),
        case.field("after")
    );
    assert!(
        stores.take_secondary_match(&appended.esn).is_none(),
        "nothing can ever find what the dead path appended"
    );
}

// ---------------------------------------------------------------------------
// Secrets and concurrency
// ---------------------------------------------------------------------------

#[test]
fn debug_prints_no_guid_no_hash_and_no_certificate() {
    // A `{:?}` anywhere in a handler reaches the log ring the web UI serves, so
    // the three entry types and the store itself print lengths instead of
    // values.
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.1", "g-a"));
    stores.add_secondary(secondary("00aaaa01", "10.0.0.7", "g-x"));
    stores.add_session(session("10.0.0.1:50000", "Vector-AAA"));

    let rendered = format!(
        "{:?} {:?} {:?} {:?}",
        stores,
        stores.primary_snapshot(),
        stores.secondary_snapshot(),
        stores.session_snapshot()
    );

    for secret in ["g-a", "g-a-hash", "g-x", "g-x-hash", "cert-Vector-AAA"] {
        assert!(
            !rendered.contains(secret),
            "{secret} reached a Debug rendering: {rendered}"
        );
    }
    for shown in ["10.0.0.1", "00aaaa01", "Vector-AAA", "<3 bytes>"] {
        assert!(
            rendered.contains(shown),
            "{shown} should still be visible: {rendered}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_keep_every_entry() {
    // Go's four slices are appended to from different connections' goroutines
    // with nothing between them. The ceiling is a real-clock timeout rather
    // than a paused clock, so a regression that deadlocks the store's mutex
    // fails here instead of hanging the suite.
    const TASKS: u32 = 8;
    const PER_TASK: u32 = 64;

    let stores = Arc::new(TokenStores::new());
    let mut handles = Vec::new();
    for task in 0..TASKS {
        let stores = Arc::clone(&stores);
        handles.push(tokio::spawn(async move {
            for step in 0..PER_TASK {
                let target = format!("10.0.{task}.{step}");
                stores.add_primary(primary(&target, "g-a"));
                stores.add_secondary(secondary("00aaaa01", &target, "g-x"));
                stores.add_session(session(&format!("{target}:50000"), "Vector-AAA"));
            }
        }));
    }

    timeout(CEILING, async {
        for handle in handles {
            handle.await.expect("no appending task panicked");
        }
    })
    .await
    .expect("the appends finished well inside the ceiling");

    let expected = (TASKS * PER_TASK) as usize;
    assert_eq!(stores.primary_len(), expected);
    assert_eq!(stores.secondary_len(), expected);
    assert_eq!(stores.session_len(), expected);

    let mut targets: Vec<String> = stores
        .primary_snapshot()
        .into_iter()
        .map(|entry| entry.target)
        .collect();
    targets.sort();
    targets.dedup();
    assert_eq!(
        targets.len(),
        expected,
        "every append landed exactly once and none was overwritten"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_walk_is_atomic_against_concurrent_appends_and_reads() {
    // Go's slices have no lock at all, so its walk interleaves with every other
    // connection's append. The port runs the whole walk under one guard, which
    // is what makes the skip quirk reproducible, and that atomicity is a
    // difference from Go worth pinning rather than assuming.
    //
    // The store starts with `PRE` entries whose last two both match the peer,
    // which is the shape that overruns, and everything appended alongside
    // misses. A walk that ran before any append saw exactly two matches and
    // overran; a walk that ran after at least one append saw one, because the
    // entry shifted into the index after the match is then an appended entry
    // that does not match. Those are the only two outcomes a walk under one
    // guard can have, and a walk that released the lock part way through would
    // also lose the appends that landed while the store was emptied, which the
    // exact count and the reader below both catch.
    const ROUNDS: usize = 8;
    const PRE: usize = 256;
    const APPEND_TASKS: usize = 4;
    const APPENDS_PER_TASK: usize = 64;
    const READ_TASKS: usize = 2;
    const MAX_READS: usize = 1 << 20;
    const APPENDED: usize = APPEND_TASKS * APPENDS_PER_TASK;

    timeout(CEILING, async {
        for round in 0..ROUNDS {
            let stores = Arc::new(TokenStores::new());
            for slot in 0..PRE - 2 {
                stores.add_primary(primary(
                    &format!("10.9.{}.{}", slot / 250, slot % 250),
                    "g-pre",
                ));
            }
            stores.add_primary(primary("10.0.0.1", "g-a"));
            stores.add_primary(primary("10.0.0.1", "g-b"));

            let done = Arc::new(AtomicBool::new(false));
            let mut readers = Vec::new();
            for _ in 0..READ_TASKS {
                let stores = Arc::clone(&stores);
                let done = Arc::clone(&done);
                readers.push(tokio::spawn(async move {
                    let mut reads = 0usize;
                    while !done.load(Ordering::Acquire) && reads < MAX_READS {
                        // The walk removes exactly one entry and everything else
                        // only grows the store, so no observer can ever see it
                        // shorter than this unless the walk let go of the lock.
                        assert!(
                            stores.primary_len() >= PRE - 1,
                            "round {round}: a reader saw {} entries part way through the walk",
                            stores.primary_len()
                        );
                        reads += 1;
                        if reads.is_multiple_of(64) {
                            tokio::task::yield_now().await;
                        }
                    }
                }));
            }

            let mut appenders = Vec::new();
            for task in 0..APPEND_TASKS {
                let stores = Arc::clone(&stores);
                appenders.push(tokio::spawn(async move {
                    for step in 0..APPENDS_PER_TASK {
                        stores.add_primary(primary(&format!("10.8.{task}.{step}"), "g-late"));
                    }
                }));
            }

            let walker = {
                let stores = Arc::clone(&stores);
                tokio::spawn(async move { stores.take_primary_matches("10.0.0.1") })
            };

            let walk = walker.await.expect("the walking task did not panic");
            for appender in appenders {
                appender.await.expect("no appending task panicked");
            }
            done.store(true, Ordering::Release);
            for reader in readers {
                reader.await.expect("no reading task panicked");
            }

            let guids: Vec<&str> = walk.matches.iter().map(|e| e.guid.as_str()).collect();
            assert!(
                (guids == ["g-a", "g-b"] && walk.overran) || (guids == ["g-a"] && !walk.overran),
                "round {round}: a walk under one guard has two outcomes, not {guids:?} \
                 with overran={}",
                walk.overran
            );

            let after = stores.primary_snapshot();
            assert_eq!(
                after.len(),
                PRE - 1 + APPENDED,
                "round {round}: one entry was removed and every append survived"
            );
            assert_eq!(after.iter().filter(|e| e.guid == "g-a").count(), 0);
            assert_eq!(
                after.iter().filter(|e| e.guid == "g-b").count(),
                1,
                "round {round}: the second match survives either outcome"
            );
            let mut targets: Vec<&str> = after.iter().map(|e| e.target.as_str()).collect();
            targets.sort_unstable();
            targets.dedup();
            assert_eq!(
                targets.len(),
                after.len(),
                "round {round}: no entry was duplicated or overwritten"
            );
        }
    })
    .await
    .expect("the rounds finished well inside the ceiling");
}
