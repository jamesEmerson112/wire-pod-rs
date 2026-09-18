//! The three transient token stores, and the three walk quirks they carry.
//!
//! Every expectation about a walk was read off a Go program that copies
//! `chipper/pkg/servers/token/token.go:37-45`, `:130-146`, `:207-217` and
//! `chipper/pkg/servers/jdocs/server.go:81-149` verbatim, with `logger.Debug`
//! replaced by a print and each panicking case wrapped in a `recover`, run on
//! `go1.24.4 windows/amd64`. The targets and GUIDs below are that program's, so
//! an assertion here can be compared with its trace line by line. Its stdout is
//! quoted in the cases that need it and in `src/token/stores.rs`'s module
//! documentation.
//!
//! Nothing in this file writes a real GUID, hash or address. `g-a` and friends
//! are the recording's placeholders and the addresses are documentation-range
//! literals.
//!
//! The log assertions go through a real `tracing` subscriber rather than
//! driving [`LogRing::record`], so the layer's target filter, level folding and
//! component derivation are covered along with the lines themselves.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use tracing_subscriber::layer::SubscriberExt;
use wirepod_core::logger::{LogLayer, LogLevel, LogRing, ManualLogClock};
use wirepod_core::test_support::install_tracing_backstop;
use wirepod_core::token::stores::{
    PrimaryEntry, SecondaryEntry, SessionEntry, TokenStores, host_of,
};

/// The stamp the ring is fixed at. Nothing here reads it; it only has to be
/// stable so no assertion depends on the wall clock.
const STAMP: &str = "2026.01.02 03:04:05";

/// A generous real-clock ceiling for the one test that awaits anything. It is
/// far longer than the work needs, so a regression that deadlocks the store's
/// mutex fails the test instead of hanging the suite.
const CEILING: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Fixtures
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

/// The `(target, guid)` pairs of a primary list, which is how the Go
/// recording prints its store.
fn pairs(entries: &[PrimaryEntry]) -> Vec<(&str, &str)> {
    entries
        .iter()
        .map(|entry| (entry.target.as_str(), entry.guid.as_str()))
        .collect()
}

/// Just the GUIDs, for asserting a walk's visit order.
fn guids(entries: &[PrimaryEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.guid.as_str()).collect()
}

/// A store holding the recording's three-entry primary list, with `second`
/// deciding whether entry 1 matches `10.0.0.1`.
fn three_entry_store(second: &str) -> TokenStores {
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.1", "g-a"));
    stores.add_primary(primary(second, "g-b"));
    stores.add_primary(primary("10.0.0.9", "g-c"));
    stores
}

// ---------------------------------------------------------------------------
// The log ring
// ---------------------------------------------------------------------------

/// A ring on a clock fixed at [`STAMP`].
fn fixed_ring() -> Arc<LogRing> {
    Arc::new(LogRing::new(Arc::new(ManualLogClock::new(0, STAMP))))
}

/// Runs `body` with `ring` installed as this thread's subscriber.
///
/// [`install_tracing_backstop`] first, for the reason its own documentation
/// gives: a callsite first reached by a thread with no subscriber is cached as
/// never-interested for the rest of the process, and the harness decides which
/// test gets there first.
fn drive(ring: &Arc<LogRing>, body: impl FnOnce()) {
    install_tracing_backstop();
    let subscriber = tracing_subscriber::registry().with(LogLayer::new(Arc::clone(ring)));
    tracing::subscriber::with_default(subscriber, body);
}

/// Every entry the ring holds, as `(level, comp, bot, msg)`. The clock is fixed
/// at zero, so `-1` is the only `since` that admits anything.
fn lines(ring: &LogRing) -> Vec<(String, String, String, String)> {
    ring.get_entries(LogLevel::Debug, -1)
        .into_iter()
        .map(|entry| (entry.level, entry.comp, entry.bot, entry.msg))
        .collect()
}

// ---------------------------------------------------------------------------
// host_of
// ---------------------------------------------------------------------------

#[test]
fn host_of_cuts_at_the_first_colon() {
    assert_eq!(host_of("10.0.0.1:50000"), "10.0.0.1");
    assert_eq!(host_of("10.0.0.1"), "10.0.0.1", "no colon, no cut");
    assert_eq!(host_of(""), "", "Go's Split answers one empty element");
    assert_eq!(host_of(":50000"), "", "a leading colon leaves nothing");
}

#[test]
fn host_of_degenerates_on_an_ipv6_peer_the_way_go_does() {
    // The Go recording prints `split0="["` for the loopback address and
    // `"[fe80"` for a link-local one, so the result is never a host and two
    // different peers can collide on it. The port reproduces that rather than
    // fixing it, because the stored targets were cut the same way going in.
    assert_eq!(host_of("[::1]:50000"), "[");
    assert_eq!(host_of("[::2]:50000"), "[", "two peers, one key");
    assert_eq!(host_of("[fe80::1]:443"), "[fe80");
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
        pairs(&stores.primary_snapshot()),
        [("10.0.0.1", "g-a"), ("10.0.0.9", "g-c")],
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

#[test]
fn the_three_removals_write_gos_lines_at_debug_under_the_token_component() {
    let ring = fixed_ring();
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.1", "g-a"));
    stores.add_secondary(secondary("00aaaa02", "10.0.0.1", "g-y"));
    stores.add_session(session("10.0.0.1:50000", "Vector-AAA"));

    drive(&ring, || {
        assert!(stores.remove_from_primary_store(0));
        assert!(stores.remove_from_second_store(0));
        assert!(stores.remove_from_session_store(0));
    });

    assert_eq!(
        lines(&ring),
        [
            // `token.go:136`: the target is interpolated and the bot column is
            // empty.
            (
                "DEBUG".to_owned(),
                "token".to_owned(),
                String::new(),
                "Removing 10.0.0.1 from temporary token-hash store".to_owned(),
            ),
            // `token.go:131`: the only one of the three that fills the bot
            // column, and the only one whose message interpolates nothing.
            (
                "DEBUG".to_owned(),
                "token".to_owned(),
                "00aaaa02".to_owned(),
                "Removing from temporary token-hash store".to_owned(),
            ),
            // `token.go:143`: the whole peer address, port and all.
            (
                "DEBUG".to_owned(),
                "token".to_owned(),
                String::new(),
                "Removing 10.0.0.1:50000 from cert-write store".to_owned(),
            ),
        ]
    );

    assert_eq!(stores.primary_len(), 0);
    assert_eq!(stores.secondary_len(), 0);
    assert_eq!(stores.session_len(), 0);
}

#[test]
fn a_removal_takes_the_named_index_and_leaves_the_rest_in_order() {
    let stores = three_entry_store("10.0.0.8");
    assert!(stores.remove_from_primary_store(1));
    assert_eq!(
        pairs(&stores.primary_snapshot()),
        [("10.0.0.1", "g-a"), ("10.0.0.9", "g-c")]
    );
}

#[test]
fn an_index_past_the_end_removes_nothing_and_says_so() {
    // Go indexes the slice to build its log line and the runtime stops the
    // process: the recording shows `index out of range [1] with length 1` for
    // the primary store, `[5] with length 1` for the secondary and
    // `[1] with length 1` for the session store. Reserved deviation 31 is the
    // phase's standing decision to turn a Go panic into a log line, and this
    // follows it. Nothing is removed either way, so the surviving entries match
    // Go's exactly; only the process surviving differs.
    let ring = fixed_ring();
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.1", "g-a"));
    stores.add_secondary(secondary("00aaaa01", "10.0.0.7", "g-x"));
    stores.add_session(session("10.0.0.1:50000", "Vector-AAA"));

    drive(&ring, || {
        assert!(!stores.remove_from_primary_store(1));
        assert!(!stores.remove_from_second_store(5));
        assert!(!stores.remove_from_session_store(1));
    });

    assert_eq!(stores.primary_len(), 1, "nothing was removed");
    assert_eq!(stores.secondary_len(), 1);
    assert_eq!(stores.session_len(), 1);

    let written = lines(&ring);
    assert_eq!(written.len(), 3);
    for (level, comp, bot, _) in &written {
        assert_eq!(level, "WARN", "the web UI's INFO buffer has to show this");
        assert_eq!(comp, "token");
        assert_eq!(bot, "");
    }
    assert_eq!(
        written[0].3,
        "index 1 is out of range for TokenHashStore, which holds 1; \
         Go panics here, so nothing was removed"
    );
    assert!(
        written[1]
            .3
            .contains("index 5 is out of range for SecondaryTokenStore")
    );
    assert!(
        written[2]
            .3
            .contains("index 1 is out of range for SessionWriteStoreNames")
    );
}

#[test]
fn removing_a_session_entry_drops_the_name_and_the_certificate_together() {
    // Go shortens `SessionWriteStoreNames` and `SessionWriteStoreCerts` by the
    // same index (`token.go:144-145`), and every reader indexes the cert slice
    // with the name slice's index (`jdocs/server.go:121`). One list makes that
    // structural.
    let stores = TokenStores::new();
    stores.add_session(session("10.0.0.1:50000", "Vector-AAA"));
    stores.add_session(session("10.0.0.9:50001", "Vector-CCC"));

    assert!(stores.remove_from_session_store(0));

    let remaining = stores.session_snapshot();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].peer_addr, "10.0.0.9:50001");
    assert_eq!(remaining[0].name, "Vector-CCC");
    assert_eq!(
        remaining[0].cert, b"cert-Vector-CCC",
        "the surviving certificate is still the surviving name's"
    );
}

// ---------------------------------------------------------------------------
// The primary walk: the skip quirk
// ---------------------------------------------------------------------------

#[test]
fn the_primary_walk_skips_the_entry_the_removal_shifted_down() {
    // The Go recording, case `primary_skip`:
    //
    //   visit num=0 pair0="10.0.0.1" MATCH
    //   visit num=1 pair0="10.0.0.9" miss
    //   visit num=2 pair0="10.0.0.9" miss
    //   after=[[10.0.0.1 g-b h-b] [10.0.0.9 g-c h-c]] matched=true botGUID="g-a"
    //
    // Entry 1 matches and survives, because removing entry 0 shifted it into
    // index 0 after the loop had already passed it. Entry 2 is read twice, once
    // where it was shifted to and once from the stale tail of the backing
    // array.
    let stores = three_entry_store("10.0.0.1");
    let walk = stores.take_primary_matches("10.0.0.1");

    assert_eq!(
        guids(&walk.matches),
        ["g-a"],
        "only entry 0 was ever visited"
    );
    assert!(walk.matched());
    assert_eq!(walk.bot_guid(), "g-a");
    assert!(!walk.overran);
    assert_eq!(
        pairs(&stores.primary_snapshot()),
        [("10.0.0.1", "g-b"), ("10.0.0.9", "g-c")],
        "a matching entry survives the walk that was looking for it"
    );
}

#[test]
fn the_primary_walk_with_one_match_removes_exactly_it() {
    // The recording's `primary_first_only`: the same skipping happens, but the
    // entry that is skipped did not match, so the outcome is the obvious one.
    let stores = three_entry_store("10.0.0.8");
    let walk = stores.take_primary_matches("10.0.0.1");

    assert_eq!(guids(&walk.matches), ["g-a"]);
    assert!(!walk.overran);
    assert_eq!(
        pairs(&stores.primary_snapshot()),
        [("10.0.0.8", "g-b"), ("10.0.0.9", "g-c")]
    );
}

#[test]
fn the_primary_walk_matches_the_last_entry_without_skipping_anything() {
    // The recording's `primary_second_only`: nothing has shifted by the time
    // the loop reaches the match, so there is no stale read to make.
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.8", "g-a"));
    stores.add_primary(primary("10.0.0.1", "g-b"));

    let walk = stores.take_primary_matches("10.0.0.1");

    assert_eq!(guids(&walk.matches), ["g-b"]);
    assert_eq!(walk.bot_guid(), "g-b");
    assert!(!walk.overran);
    assert_eq!(pairs(&stores.primary_snapshot()), [("10.0.0.8", "g-a")]);
}

#[test]
fn the_primary_walk_folds_case_on_the_target() {
    // `jdocs/server.go:95` is `strings.EqualFold`. The recording's
    // `primary_equalfold` empties the store.
    let stores = TokenStores::new();
    stores.add_primary(primary("LOCALHOST", "g-a"));

    let walk = stores.take_primary_matches("localhost");

    assert!(walk.matched());
    assert!(stores.primary_snapshot().is_empty());
}

#[test]
fn the_primary_walk_with_no_match_changes_nothing() {
    let stores = three_entry_store("10.0.0.8");
    let walk = stores.take_primary_matches("10.0.0.2");

    assert!(walk.matches.is_empty());
    assert!(!walk.matched());
    assert_eq!(walk.bot_guid(), "", "Go's botGUID starts empty");
    assert!(!walk.overran);
    assert_eq!(
        pairs(&stores.primary_snapshot()),
        [
            ("10.0.0.1", "g-a"),
            ("10.0.0.8", "g-b"),
            ("10.0.0.9", "g-c")
        ]
    );
}

#[test]
fn the_primary_walk_on_an_empty_store_does_nothing() {
    let stores = TokenStores::new();
    let walk = stores.take_primary_matches("10.0.0.1");
    assert!(walk.matches.is_empty());
    assert!(!walk.overran);
    assert_eq!(stores.primary_len(), 0);
}

#[test]
fn the_primary_walk_logs_one_line_per_removal() {
    let ring = fixed_ring();
    let stores = three_entry_store("10.0.0.1");
    drive(&ring, || {
        let walk = stores.take_primary_matches("10.0.0.1");
        assert!(!walk.overran);
    });
    assert_eq!(
        lines(&ring),
        [(
            "DEBUG".to_owned(),
            "token".to_owned(),
            String::new(),
            "Removing 10.0.0.1 from temporary token-hash store".to_owned(),
        )],
        "one removal, one line, and the skipped match is never mentioned"
    );
}

#[test]
fn the_walks_removal_line_names_the_stored_target_not_the_peer() {
    // `token.go:136` interpolates `TokenHashStore[index][0]`, the entry's own
    // target, and the comparison that reached it folds case, so the two can
    // differ. Go writes what the store held.
    let ring = fixed_ring();
    let stores = TokenStores::new();
    stores.add_primary(primary("LOCALHOST", "g-a"));

    drive(&ring, || {
        assert!(stores.take_primary_matches("localhost").matched());
    });

    assert_eq!(
        lines(&ring),
        [(
            "DEBUG".to_owned(),
            "token".to_owned(),
            String::new(),
            "Removing LOCALHOST from temporary token-hash store".to_owned(),
        )]
    );
}

// ---------------------------------------------------------------------------
// The primary walk: where Go's process dies
// ---------------------------------------------------------------------------

#[test]
fn the_primary_walk_stops_where_two_duplicates_make_go_panic() {
    // The recording's `primary_two_duplicates`, which is the realistic shape:
    // one robot asked for a token twice before its `ReadDocs` arrived.
    //
    //   visit num=0 pair0="10.0.0.1" MATCH
    //   visit num=1 pair0="10.0.0.1" MATCH
    //   PANIC runtime error: index out of range [1] with length 1
    //   store after=[[10.0.0.1 g-b h-b]]
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.1", "g-a"));
    stores.add_primary(primary("10.0.0.1", "g-b"));

    let walk = stores.take_primary_matches("10.0.0.1");

    assert_eq!(
        guids(&walk.matches),
        ["g-a", "g-b"],
        "Go runs its loop body on both before the second removal dies"
    );
    assert!(walk.overran);
    assert_eq!(
        pairs(&stores.primary_snapshot()),
        [("10.0.0.1", "g-b")],
        "the store holds what Go's holds at the instant its process dies"
    );
}

#[test]
fn the_primary_walk_stops_after_reading_the_stale_tail_twice() {
    // The recording's `primary_zero_and_two`: entries 0 and 2 match, entry 1
    // does not.
    //
    //   visit num=0 pair0="10.0.0.1" MATCH
    //   visit num=1 pair0="10.0.0.1" MATCH
    //   visit num=2 pair0="10.0.0.1" MATCH
    //   PANIC runtime error: index out of range [2] with length 1
    //   store after=[[10.0.0.8 g-b h-b]]
    //
    // Three matches out of a two-match store: entry 2 is shifted into index 1
    // and then read again from the stale tail at index 2.
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.1", "g-a"));
    stores.add_primary(primary("10.0.0.8", "g-b"));
    stores.add_primary(primary("10.0.0.1", "g-c"));

    let walk = stores.take_primary_matches("10.0.0.1");

    assert_eq!(guids(&walk.matches), ["g-a", "g-c", "g-c"]);
    assert_eq!(walk.bot_guid(), "g-c", "Go's botGUID is the last match's");
    assert!(walk.overran);
    assert_eq!(pairs(&stores.primary_snapshot()), [("10.0.0.8", "g-b")]);
}

#[test]
fn the_primary_walk_stops_when_all_three_entries_match() {
    // The recording's `primary_all_three`, which reaches the same panic and
    // leaves the entry that was shifted past.
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.1", "g-a"));
    stores.add_primary(primary("10.0.0.1", "g-b"));
    stores.add_primary(primary("10.0.0.1", "g-c"));

    let walk = stores.take_primary_matches("10.0.0.1");

    assert_eq!(guids(&walk.matches), ["g-a", "g-c", "g-c"]);
    assert!(walk.overran);
    assert_eq!(pairs(&stores.primary_snapshot()), [("10.0.0.1", "g-b")]);
}

#[test]
fn an_overrun_walk_logs_the_removals_it_made_and_then_the_warning() {
    let ring = fixed_ring();
    let stores = TokenStores::new();
    stores.add_primary(primary("10.0.0.1", "g-a"));
    stores.add_primary(primary("10.0.0.1", "g-b"));

    drive(&ring, || {
        assert!(stores.take_primary_matches("10.0.0.1").overran);
    });

    let written = lines(&ring);
    assert_eq!(written.len(), 2);
    assert_eq!(written[0].0, "DEBUG");
    assert_eq!(
        written[0].3,
        "Removing 10.0.0.1 from temporary token-hash store"
    );
    assert_eq!(written[1].0, "WARN");
    assert_eq!(written[1].1, "token");
    assert!(
        written[1]
            .3
            .contains("index 1 is out of range for TokenHashStore")
    );
}

// ---------------------------------------------------------------------------
// The session store's two walks
// ---------------------------------------------------------------------------

#[test]
fn the_session_lookup_stops_at_the_first_match() {
    // `jdocs/server.go:128` breaks out of the loop after writing one
    // certificate, so a second entry for the same host is left behind. The
    // recording's `session_break` leaves
    // `[[10.0.0.1:50001 Vector-BBB] [10.0.0.9:50002 Vector-CCC]]`.
    let stores = TokenStores::new();
    stores.add_session(session("10.0.0.1:50000", "Vector-AAA"));
    stores.add_session(session("10.0.0.1:50001", "Vector-BBB"));
    stores.add_session(session("10.0.0.9:50002", "Vector-CCC"));

    let found = stores
        .find_session_match("10.0.0.1")
        .expect("the first entry matches");
    assert_eq!(found.index, 0);
    assert_eq!(found.entry.name, "Vector-AAA");
    assert_eq!(found.entry.cert, b"cert-Vector-AAA");

    assert!(stores.remove_from_session_store(found.index));

    let remaining = stores.session_snapshot();
    assert_eq!(
        remaining
            .iter()
            .map(|entry| entry.peer_addr.as_str())
            .collect::<Vec<_>>(),
        ["10.0.0.1:50001", "10.0.0.9:50002"],
        "the second match for the same host survives the break"
    );
}

#[test]
fn the_session_lookup_splits_the_stored_address_and_folds_case() {
    // `jdocs/server.go:112` is
    // `strings.EqualFold(ipAddr, strings.Split(pair[0], ":")[0])`.
    let stores = TokenStores::new();
    stores.add_session(session("LOCALHOST:50000", "Vector-AAA"));

    let found = stores
        .find_session_match("localhost")
        .expect("EqualFold on the split host");
    assert_eq!(found.index, 0);
    assert_eq!(found.entry.name, "Vector-AAA");

    assert!(
        stores.find_session_match("LOCALHOST:50000").is_none(),
        "the caller passes an already-split host, not a whole address"
    );
}

#[test]
fn the_session_lookup_misses_a_host_no_entry_carries() {
    let stores = TokenStores::new();
    stores.add_session(session("10.0.0.9:50002", "Vector-CCC"));
    assert!(stores.find_session_match("10.0.0.1").is_none());
    assert_eq!(stores.session_len(), 1, "a miss removes nothing");
}

#[test]
fn the_session_presence_check_is_case_sensitive_where_the_lookup_is_not() {
    // `jdocs/server.go:82` compares with `==` while `:112` folds case, twelve
    // lines apart and over the same store. The recording:
    // `presence(localhost)=false presence(LOCALHOST)=true`.
    let stores = TokenStores::new();
    stores.add_session(session("LOCALHOST:50000", "Vector-AAA"));

    assert!(!stores.session_holds("localhost"));
    assert!(stores.session_holds("LOCALHOST"));
    assert!(
        stores.find_session_match("localhost").is_some(),
        "the lookup twelve lines later finds what the presence check did not"
    );
}

#[test]
fn the_session_presence_check_removes_nothing() {
    let stores = TokenStores::new();
    stores.add_session(session("10.0.0.1:50000", "Vector-AAA"));
    assert!(stores.session_holds("10.0.0.1"));
    assert_eq!(stores.session_len(), 1);
}

// ---------------------------------------------------------------------------
// The secondary store
// ---------------------------------------------------------------------------

#[test]
fn the_secondary_scan_stops_at_the_first_match() {
    // The recording's `secondary_walk`: `token.go:216` breaks, so the second
    // entry for the same serial survives.
    let stores = TokenStores::new();
    stores.add_secondary(secondary("00aaaa01", "10.0.0.7", "g-x"));
    stores.add_secondary(secondary("00aaaa02", "10.0.0.1", "g-y"));
    stores.add_secondary(secondary("00aaaa02", "10.0.0.2", "g-z"));

    let found = stores
        .take_secondary_match("00aaaa02")
        .expect("the second entry matches");
    assert_eq!(found.guid, "g-y");
    assert_eq!(found.guid_hash, "g-y-hash");

    let remaining = stores.secondary_snapshot();
    assert_eq!(
        remaining
            .iter()
            .map(|entry| entry.guid.as_str())
            .collect::<Vec<_>>(),
        ["g-x", "g-z"]
    );
}

#[test]
fn the_secondary_scan_compares_the_serial_exactly() {
    // `token.go:208` is `robot[0] == esn`, where every other serial lookup in
    // the server folds case. The recording's `secondary_case_sensitive` finds
    // nothing.
    let stores = TokenStores::new();
    stores.add_secondary(secondary("00AAAA02", "10.0.0.1", "g-y"));

    assert!(stores.take_secondary_match("00aaaa02").is_none());
    assert_eq!(stores.secondary_len(), 1, "a miss removes nothing");
    assert!(stores.take_secondary_match("00AAAA02").is_some());
}

#[test]
fn the_secondary_scan_logs_the_serial_in_the_bot_column() {
    let ring = fixed_ring();
    let stores = TokenStores::new();
    stores.add_secondary(secondary("00aaaa02", "10.0.0.1", "g-y"));

    drive(&ring, || {
        assert!(stores.take_secondary_match("00aaaa02").is_some());
    });

    assert_eq!(
        lines(&ring),
        [(
            "DEBUG".to_owned(),
            "token".to_owned(),
            "00aaaa02".to_owned(),
            "Removing from temporary token-hash store".to_owned(),
        )]
    );
}

#[test]
fn the_dead_secondary_path_leaves_the_store_as_it_found_it() {
    // `jdocs/server.go:136` appends and `:149` removes the element it just
    // appended, with nothing in between that reads the store. The recording's
    // `secondary_dead_path` ends holding only the entry it started with, and
    // the removal's line names the serial that was appended.
    let ring = fixed_ring();
    let stores = TokenStores::new();
    stores.add_secondary(secondary("00aaaa01", "10.0.0.7", "g-x"));
    let before = stores.secondary_snapshot();

    drive(&ring, || {
        stores.add_secondary(secondary("00aaaa02", "10.0.0.1", "g-y"));
        assert_eq!(stores.secondary_len(), 2);
        assert!(stores.remove_from_second_store(stores.secondary_len() - 1));
    });

    assert_eq!(stores.secondary_snapshot(), before);
    assert_eq!(
        lines(&ring),
        [(
            "DEBUG".to_owned(),
            "token".to_owned(),
            "00aaaa02".to_owned(),
            "Removing from temporary token-hash store".to_owned(),
        )],
        "the dead path still writes one line into the ring the web UI reads"
    );
    assert!(
        stores.take_secondary_match("00aaaa02").is_none(),
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
