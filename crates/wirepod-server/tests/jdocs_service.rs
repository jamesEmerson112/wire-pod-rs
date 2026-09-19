//! The jdocs service: what a robot gets back from `WriteDoc` and `ReadDocs`.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use tonic::{Code, Request};
use wirepod_core::paths::{AssetDir, DataDir};
use wirepod_core::test_support::FakeConnFactory;
use wirepod_core::{
    AppState, Paths, PrimaryEntry, RobotConnFactory, SdkIniStore, create_token_and_hashed_token,
};
use wirepod_proto::jdocspb::jdocs_server::Jdocs;
use wirepod_proto::jdocspb::{
    Jdoc as JdocMsg, ReadDocsReq, WriteDocReq, read_docs_req, write_doc_resp,
};
use wirepod_server::jdocs::new_jdocs_server;
use wirepod_server::peer::PeerAddr;
use wirepod_server::test_support::{TEST_ESN, TEST_IP, unreachable_error};

/// The robot's serial as its `thing` spells it.
const THING: &str = "vic:00303f28";

/// A state whose every file lands in a directory of this test's own, so nothing
/// here can reach the live `%APPDATA%\wire-pod` or `~/.anki_vector`.
fn temp_state(label: &str) -> (Arc<AppState>, PathBuf) {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(label);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("jdocs")).expect("create the jdocs directory");
    fs::create_dir_all(root.join("session-certs")).expect("create the session-certs directory");
    let factory: Arc<dyn RobotConnFactory> =
        Arc::new(FakeConnFactory::failing(unreachable_error()));
    let state = AppState::builder(factory)
        .paths(Paths::new(DataDir::rooted(&root), AssetDir::new(&root)))
        .sdk_ini(SdkIniStore::new(format!("{}/anki/", root.display())))
        .build();
    (state, root)
}

fn from_robot<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.extensions_mut().insert(PeerAddr(
        format!("{TEST_IP}:52301")
            .parse()
            .expect("parse the peer address"),
    ));
    request
}

#[tokio::test]
async fn write_doc_stores_the_document_and_a_missing_one_is_an_error() {
    let (state, root) = temp_state("jdocs-write");
    let server = new_jdocs_server(Arc::clone(&state));

    let response = server
        .write_doc(from_robot(WriteDocReq {
            user_id: String::new(),
            thing: THING.to_owned(),
            doc_name: "vic.RobotSettings".to_owned(),
            doc: Some(JdocMsg {
                doc_version: 4,
                fmt_version: 1,
                client_metadata: String::new(),
                json_doc: r#"{"master_volume":3}"#.to_owned(),
            }),
        }))
        .await
        .expect("the jdocs service answered an error")
        .into_inner();

    assert_eq!(response.status, write_doc_resp::Status::Accepted as i32);
    // A document the store had not seen appends, and Go answers zero for that.
    assert_eq!(response.latest_doc_version, 0);
    let stored = state
        .jdocs()
        .get_jdoc(THING, "vic.RobotSettings")
        .expect("the document was not stored");
    assert_eq!(stored.json_doc, r#"{"master_volume":3}"#);
    assert_eq!(stored.doc_version, 4);

    // Go reads four fields off a nil `req.Doc` and dies.
    let status = server
        .write_doc(Request::new(WriteDocReq {
            user_id: String::new(),
            thing: THING.to_owned(),
            doc_name: "vic.RobotSettings".to_owned(),
            doc: None,
        }))
        .await
        .expect_err("a request with no doc must not be accepted");
    assert_eq!(status.code(), Code::InvalidArgument);

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn read_docs_claims_the_parked_guid_and_activates_the_robot() {
    let (state, root) = temp_state("jdocs-read");
    let pair = create_token_and_hashed_token().expect("draw a token");
    state.tokens().add_primary(PrimaryEntry {
        target: TEST_IP.to_owned(),
        guid: pair.guid.clone(),
        guid_hash: pair.guid_hash.clone(),
    });
    let server = new_jdocs_server(Arc::clone(&state));

    let response = server
        .read_docs(from_robot(ReadDocsReq {
            user_id: String::new(),
            thing: THING.to_owned(),
            items: vec![read_docs_req::Item {
                doc_name: "vic.AppTokens".to_owned(),
                my_doc_version: 0,
            }],
        }))
        .await
        .expect("the jdocs service answered an error")
        .into_inner();

    let doc = response
        .items
        .first()
        .and_then(|item| item.doc.as_ref())
        .expect("the response carries no document");
    assert!(doc.json_doc.contains(&pair.guid_hash), "{}", doc.json_doc);

    // The parked entry is consumed and the robot is now an authenticated one.
    assert!(state.tokens().primary_snapshot().is_empty());
    let bot_info = state.bot_info_snapshot();
    assert_eq!(bot_info.robots.len(), 1);
    assert_eq!(bot_info.robots[0].esn, TEST_ESN);
    assert_eq!(bot_info.robots[0].ip_address, TEST_IP);
    assert_eq!(bot_info.robots[0].guid, pair.guid);
    assert!(bot_info.robots[0].activated);

    let _ = fs::remove_dir_all(&root);
}
