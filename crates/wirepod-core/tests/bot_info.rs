//! The bot-info file: the disk round trip, the wire projection and the
//! serial-to-target resolve rules.

use std::fs;
use std::path::PathBuf;

use wirepod_core::{BotInfo, BotInfoWire, Esn};

/// Two robots, in the byte-exact shape Go's `json.Marshal` writes.
const TWO_ROBOTS: &str = concat!(
    r#"{"global_guid":"<guid>","robots":["#,
    r#"{"esn":"00303f28","ip_address":"192.168.8.203","guid":"<guid-a>","activated":true},"#,
    r#"{"esn":"00e20100","ip_address":"192.168.8.77","guid":"","activated":false}"#,
    "]}"
);

fn temp_path(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&dir).expect("create the target temp dir");
    dir.join(name)
}

#[test]
fn a_disk_round_trip_preserves_unknown_fields_in_sorted_order() {
    // `schema_version` and `zzz_last` bracket the known top-level keys
    // alphabetically, and the per-robot extras do the same, so a sorted
    // re-emission is visibly different from the input order.
    let on_disk = concat!(
        r#"{"zzz_last":true,"global_guid":"<guid>","schema_version":2,"robots":["#,
        r#"{"note":"fork only","esn":"00303f28","ip_address":"192.168.8.203",
             "guid":"<guid-a>","activated":true,"aaa_first":[1,2]}"#,
        "]}"
    );
    let info: BotInfo = serde_json::from_str(on_disk).expect("parse the bot info file");

    assert_eq!(info.global_guid, "<guid>");
    assert_eq!(info.robots.len(), 1);
    assert_eq!(info.extra.len(), 2);
    assert_eq!(info.extra["schema_version"], serde_json::json!(2));
    assert_eq!(info.extra["zzz_last"], serde_json::json!(true));
    assert_eq!(info.robots[0].extra.len(), 2);
    assert_eq!(info.robots[0].extra["note"], serde_json::json!("fork only"));

    let path = temp_path("bot_info_round_trip.json");
    fs::write(&path, serde_json::to_string(&info).expect("serialise")).expect("write the file");
    let reloaded: BotInfo =
        serde_json::from_str(&fs::read_to_string(&path).expect("read the file")).expect("reparse");
    fs::remove_file(&path).expect("remove the file");
    assert_eq!(reloaded, info);

    // Extras land in a BTreeMap, so they come back sorted rather than in file
    // order. That is the recorded deviation, and it is what this pins.
    let written = serde_json::to_string(&reloaded).expect("serialise");
    assert_eq!(
        written,
        concat!(
            r#"{"global_guid":"<guid>","robots":["#,
            r#"{"esn":"00303f28","ip_address":"192.168.8.203","guid":"<guid-a>","#,
            r#""activated":true,"aaa_first":[1,2],"note":"fork only"}"#,
            r#"],"schema_version":2,"zzz_last":true}"#
        )
    );
}

#[test]
fn the_wire_projection_drops_extras_and_matches_gos_marshal() {
    let mut info: BotInfo = serde_json::from_str(TWO_ROBOTS).expect("parse two robots");
    info.extra
        .insert("schema_version".into(), serde_json::json!(2));
    info.robots[0]
        .extra
        .insert("note".into(), serde_json::json!("fork only"));

    let wire = BotInfoWire::from(&info);
    let body = serde_json::to_string(&wire).expect("serialise the wire projection");

    assert_eq!(body, TWO_ROBOTS);
    assert!(!body.ends_with('\n'));
    assert!(!body.contains("schema_version"));
    assert!(!body.contains("note"));
}

#[test]
fn resolve_takes_the_last_match_and_ignores_case() {
    // Go's scan in newRobot has no break, so a duplicated serial resolves to
    // the entry furthest down the file, and the match is EqualFold.
    let info: BotInfo = serde_json::from_str(concat!(
        r#"{"global_guid":"<guid>","robots":["#,
        r#"{"esn":"00303F28","ip_address":"192.168.8.1","guid":"<guid-first>","activated":true},"#,
        r#"{"esn":"00303f28","ip_address":"192.168.8.203","guid":"<guid-last>","activated":true}"#,
        "]}"
    ))
    .expect("parse duplicated serials");

    let target = info
        .resolve(&Esn::new("00303F28"))
        .expect("the serial is in the file");
    assert_eq!(target.ip, "192.168.8.203");
    assert_eq!(target.guid, "<guid-last>");
    // The resolved ESN is the normalized request, not the file's spelling.
    assert_eq!(target.esn, Esn::new("00303f28"));
    assert_eq!(target.grpc_target(), "192.168.8.203:443");

    assert_eq!(info.resolve(&Esn::new(" 00303f28 ")), Some(target));
}

#[test]
fn resolve_falls_back_to_the_global_guid_for_an_empty_one() {
    let info: BotInfo = serde_json::from_str(TWO_ROBOTS).expect("parse two robots");

    let with_own = info.resolve(&Esn::new("00303f28")).expect("first robot");
    assert_eq!(with_own.guid, "<guid-a>");

    let without = info.resolve(&Esn::new("00E20100")).expect("second robot");
    assert_eq!(without.guid, "<guid>");
    assert_eq!(without.ip, "192.168.8.77");
}

#[test]
fn resolve_answers_none_for_an_unknown_serial() {
    let info: BotInfo = serde_json::from_str(TWO_ROBOTS).expect("parse two robots");

    assert_eq!(info.resolve(&Esn::new("00000000")), None);
    assert_eq!(info.resolve(&Esn::new("")), None);
}

#[test]
fn an_empty_robot_list_parses_and_resolves_nothing() {
    let info: BotInfo =
        serde_json::from_str(r#"{"global_guid":"","robots":[]}"#).expect("parse an empty file");
    assert!(info.robots.is_empty());
    assert_eq!(info.resolve(&Esn::new("00303f28")), None);

    // Every field defaults, so a file missing keys entirely still loads.
    let bare: BotInfo = serde_json::from_str("{}").expect("parse an empty object");
    assert_eq!(bare, BotInfo::default());
    assert_eq!(
        serde_json::to_string(&BotInfoWire::from(&bare)).expect("serialise"),
        r#"{"global_guid":"","robots":[]}"#
    );
}
