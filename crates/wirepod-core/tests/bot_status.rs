//! The jdocs pinger: the conn-check bookkeeping, the status vocabulary and
//! its thresholds, and the `/api/get_bot_status` body.

use std::sync::Arc;
use std::time::Duration;

use wirepod_core::{BotInfo, BotStatusKind, Clock, ManualClock, PingerState};

const ROBOT_IP: &str = "192.168.8.203";

fn two_robots() -> BotInfo {
    serde_json::from_str(concat!(
        r#"{"global_guid":"<guid>","robots":["#,
        r#"{"esn":"00303F28","ip_address":"192.168.8.203","guid":"<guid-a>","activated":true},"#,
        r#"{"esn":"00e20100","ip_address":"192.168.8.77","guid":"","activated":false}"#,
        "]}"
    ))
    .expect("parse two robots")
}

/// Records a check, then reports the first robot's status `elapsed` seconds on.
fn status_after(elapsed_secs: u64) -> (BotStatusKind, i64) {
    let info = two_robots();
    let clock = ManualClock::new();
    let pinger = PingerState::new();

    assert!(pinger.note_check(&info, ROBOT_IP, &clock));
    clock.advance_secs(elapsed_secs);

    let statuses = pinger.snapshot(&info, &clock);
    (statuses[0].status, statuses[0].timesince)
}

#[test]
fn the_manual_clock_is_settable_advanceable_and_shareable() {
    let clock = Arc::new(ManualClock::at(Duration::from_secs(10)));
    let shared = Arc::clone(&clock);

    assert_eq!(clock.now(), Duration::from_secs(10));
    shared.advance(Duration::from_millis(1500));
    assert_eq!(clock.now(), Duration::from_millis(11_500));

    // Whole seconds only, so a sub-second remainder does not round up.
    assert_eq!(clock.secs_since(Duration::from_secs(10)), 1);
    clock.set(Duration::from_secs(4));
    assert_eq!(clock.now(), Duration::from_secs(4));
    // A reading from the future counts as zero, never as a negative age.
    assert_eq!(clock.secs_since(Duration::from_secs(9)), 0);
}

#[test]
fn the_status_vocabulary_switches_at_gos_thresholds() {
    assert_eq!(status_after(0).0, BotStatusKind::Online);
    // online is `TimeSinceLastCheck <= 15`, so 15 is still online and 16 is not.
    assert_eq!(status_after(15).0, BotStatusKind::Online);
    assert_eq!(status_after(16).0, BotStatusKind::Offline);
    // offline is `Stopped && TimeSinceLastCheck < 120`, so 119 is the last one.
    assert_eq!(status_after(119).0, BotStatusKind::Offline);
    assert_eq!(status_after(120).0, BotStatusKind::Disconnected);
    assert_eq!(status_after(6000).0, BotStatusKind::Disconnected);

    assert_eq!(BotStatusKind::Online.to_string(), "online");
    assert_eq!(BotStatusKind::Offline.to_string(), "offline");
    assert_eq!(BotStatusKind::Disconnected.to_string(), "disconnected");
}

#[test]
fn timesince_is_the_age_in_seconds_or_minus_one() {
    assert_eq!(status_after(0).1, 0);
    assert_eq!(status_after(15).1, 15);
    assert_eq!(status_after(120).1, 120);

    // A robot the pinger has never heard from reports -1, not 0.
    let info = two_robots();
    let clock = ManualClock::new();
    let pinger = PingerState::new();
    pinger.note_check(&info, ROBOT_IP, &clock);

    let statuses = pinger.snapshot(&info, &clock);
    assert_eq!(statuses[1].timesince, -1);
    assert_eq!(statuses[1].status, BotStatusKind::Disconnected);
}

#[test]
fn an_empty_snapshot_serialises_to_an_empty_array() {
    let info = BotInfo::default();
    let clock = ManualClock::new();
    let pinger = PingerState::new();

    let statuses = pinger.snapshot(&info, &clock);
    assert!(statuses.is_empty());
    assert_eq!(serde_json::to_string(&statuses).expect("serialise"), "[]");
}

#[test]
fn a_populated_snapshot_keeps_gos_key_order_and_stored_case() {
    let info = two_robots();
    let clock = ManualClock::new();
    let pinger = PingerState::new();
    pinger.note_check(&info, ROBOT_IP, &clock);
    clock.advance_secs(7);

    let body = serde_json::to_string(&pinger.snapshot(&info, &clock)).expect("serialise");
    assert_eq!(
        body,
        concat!(
            r#"[{"esn":"00303F28","ip":"192.168.8.203","status":"online","timesince":7},"#,
            r#"{"esn":"00e20100","ip":"192.168.8.77","status":"disconnected","timesince":-1}]"#
        )
    );
}

#[test]
fn note_check_matches_the_peer_ip_exactly_and_reports_the_stopped_transition() {
    let info = two_robots();
    let clock = ManualClock::new();
    let pinger = PingerState::new();

    // An unknown peer records nothing and pulls no jdocs.
    assert!(!pinger.note_check(&info, "192.168.8.9", &clock));
    // The match is byte equality, not a substring or a prefix.
    assert!(!pinger.note_check(&info, "192.168.8.20", &clock));
    assert!(pinger.snapshot(&info, &clock)[0].timesince < 0);

    // The first check for a robot pulls jdocs.
    assert!(pinger.note_check(&info, ROBOT_IP, &clock));
    // A check while the robot is still running does not, but resets the age.
    clock.advance_secs(10);
    assert!(!pinger.note_check(&info, ROBOT_IP, &clock));
    assert_eq!(pinger.snapshot(&info, &clock)[0].timesince, 0);

    // Only the stopped-to-running transition pulls again.
    clock.advance_secs(15);
    assert!(!pinger.note_check(&info, ROBOT_IP, &clock));
    clock.advance_secs(16);
    assert!(pinger.note_check(&info, ROBOT_IP, &clock));
    assert_eq!(
        pinger.snapshot(&info, &clock)[0].status,
        BotStatusKind::Online
    );
}

#[test]
fn a_disabled_pinger_records_nothing_and_reports_everything_disconnected() {
    let info = two_robots();
    let clock = ManualClock::new();
    let pinger = PingerState::new();

    assert!(pinger.is_enabled());
    pinger.set_enabled(false);
    assert!(!pinger.is_enabled());

    assert!(!pinger.note_check(&info, ROBOT_IP, &clock));
    for status in pinger.snapshot(&info, &clock) {
        assert_eq!(status.status, BotStatusKind::Disconnected);
        assert_eq!(status.timesince, -1);
    }

    pinger.set_enabled(true);
    assert!(pinger.note_check(&info, ROBOT_IP, &clock));
}

#[test]
fn the_snapshot_serial_lookup_is_case_sensitive() {
    // Go matches the pinger list against the bot-info file with `==` rather
    // than EqualFold, so a serial whose case changed under it silently reports
    // disconnected. Only a rewrite of the file can produce this.
    let clock = ManualClock::new();
    let pinger = PingerState::new();
    assert!(pinger.note_check(&two_robots(), ROBOT_IP, &clock));

    let recased: BotInfo = serde_json::from_str(concat!(
        r#"{"global_guid":"<guid>","robots":["#,
        r#"{"esn":"00303f28","ip_address":"192.168.8.203","guid":"<guid-a>","activated":true}"#,
        "]}"
    ))
    .expect("parse the recased file");

    let statuses = pinger.snapshot(&recased, &clock);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].status, BotStatusKind::Disconnected);
    assert_eq!(statuses[0].timesince, -1);
}
