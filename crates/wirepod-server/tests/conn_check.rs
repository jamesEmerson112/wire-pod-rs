//! The conn check's side effects: the jdocs pull and the pinger bookkeeping.
//!
//! Nothing here can reach the network. The robot is a fake, every file lands in
//! a directory of this test's own, and the mDNS browse is left switched off, so
//! no test puts a packet on the LAN.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use http::{Method, StatusCode};
use wirepod_core::paths::{AssetDir, DataDir};
use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn, RobotCall};
use wirepod_core::{
    AppState, Jdoc, JdocKind, NamedJdoc, Paths, RobotConn, RobotConnFactory, SdkIniStore,
};
use wirepod_server::peer::PeerAddr;
use wirepod_server::test_support::{TEST_IP, one_robot, request, send_to};
use wirepod_server::{build_router, literals};

/// The document the fake robot hands back when its jdocs are pulled.
const SETTINGS: &str = r#"{"master_volume":"VOLUME_4"}"#;

fn temp_paths(label: &str) -> (Paths, SdkIniStore, PathBuf) {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(label);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("jdocs")).expect("create the jdocs directory");
    (
        Paths::new(DataDir::rooted(&root), AssetDir::new(&root)),
        SdkIniStore::new(format!("{}/anki/", root.display())),
        root,
    )
}

fn server(label: &str) -> (Router, Arc<AppState>, Arc<FakeRobotConn>, PathBuf) {
    let robot = Arc::new(FakeRobotConn::new().with_jdocs(Ok(vec![NamedJdoc {
        kind: JdocKind::RobotSettings,
        doc: Jdoc {
            doc_version: 7,
            fmt_version: 1,
            json_doc: SETTINGS.to_owned(),
            ..Jdoc::default()
        },
    }])));
    let conn: Arc<dyn RobotConn> = Arc::clone(&robot) as Arc<dyn RobotConn>;
    let factory: Arc<dyn RobotConnFactory> = Arc::new(FakeConnFactory::connecting_to(conn));
    let (paths, sdk_ini, root) = temp_paths(label);
    let state = AppState::builder(factory)
        .bot_info(one_robot())
        .paths(paths)
        .sdk_ini(sdk_ini)
        .build();
    (build_router(Arc::clone(&state)), state, robot, root)
}

/// A conn check as the robot sends it, with the address the listener would have
/// put in the extensions.
fn heartbeat(uri: &str, peer: &str) -> axum::extract::Request {
    let mut req = request(Method::GET, uri, None);
    req.extensions_mut()
        .insert(PeerAddr(peer.parse().expect("parse the peer address")));
    req
}

#[tokio::test]
async fn a_heartbeat_from_a_known_robot_pulls_its_jdocs_once() {
    let (router, state, robot, root) = server("conncheck-known");

    let reply = send_to(&router, heartbeat("/ok", &format!("{TEST_IP}:52301"))).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::OK);

    let pulls = robot.call_count(|call| matches!(call, RobotCall::PullJdocs(_)));
    assert_eq!(pulls, 1);
    let stored = state
        .jdocs()
        .get_jdoc("vic:00303f28", "vic.RobotSettings")
        .expect("the pulled document was not stored");
    assert_eq!(stored.json_doc, SETTINGS);

    // The robot is now running rather than stopped, so the next heartbeat only
    // resets its age.
    let reply = send_to(&router, heartbeat("/ok", &format!("{TEST_IP}:52301"))).await;
    assert_eq!(reply.body, literals::OK);
    assert_eq!(
        robot.call_count(|call| matches!(call, RobotCall::PullJdocs(_))),
        1
    );

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_heartbeat_from_an_unknown_peer_pulls_nothing() {
    let (router, _state, robot, root) = server("conncheck-unknown");

    // The peer is not in the bot-info file, so Go goes down the mDNS arm and
    // never reaches the pinger.
    let reply = send_to(&router, heartbeat("/ok", "192.168.8.99:52301")).await;
    assert_eq!(reply.body, literals::OK);
    assert_eq!(
        robot.call_count(|call| matches!(call, RobotCall::PullJdocs(_))),
        0
    );

    // `runMDNS=true` answers before any of that, on both conn-check paths.
    let reply = send_to(
        &router,
        heartbeat("/ok:80?runMDNS=true", &format!("{TEST_IP}:52301")),
    )
    .await;
    assert_eq!(reply.body, literals::MDNS_RAN);
    assert_eq!(
        robot.call_count(|call| matches!(call, RobotCall::PullJdocs(_))),
        0
    );

    let _ = fs::remove_dir_all(&root);
}
