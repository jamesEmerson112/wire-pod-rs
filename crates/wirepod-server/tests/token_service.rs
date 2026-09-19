//! The token service: what a robot gets back from a token request.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use tonic::{Code, Request};
use wirepod_core::paths::{AssetDir, DataDir};
use wirepod_core::test_support::FakeConnFactory;
use wirepod_core::{AppState, Paths, RobotConnFactory, SdkIniStore};
use wirepod_proto::tokenpb::token_server::Token;
use wirepod_proto::tokenpb::{AssociatePrimaryUserRequest, RefreshTokenRequest};
use wirepod_server::peer::PeerAddr;
use wirepod_server::test_support::unreachable_error;
use wirepod_server::token::new_token_server;

/// A state whose every file lands in a directory of this test's own, so nothing
/// here can reach the live `%APPDATA%\wire-pod` or `~/.anki_vector`.
fn temp_state(label: &str) -> (Arc<AppState>, PathBuf) {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(label);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("jdocs")).expect("create the jdocs directory");
    let factory: Arc<dyn RobotConnFactory> =
        Arc::new(FakeConnFactory::failing(unreachable_error()));
    let state = AppState::builder(factory)
        .paths(Paths::new(DataDir::rooted(&root), AssetDir::new(&root)))
        .sdk_ini(SdkIniStore::new(format!("{}/anki/", root.display())))
        .build();
    (state, root)
}

#[tokio::test]
async fn refresh_token_answers_a_jwt_and_remembers_the_peer() {
    let (state, root) = temp_state("token-refresh");
    let server = new_token_server(Arc::clone(&state));
    let mut request = Request::new(RefreshTokenRequest::default());
    request.extensions_mut().insert(PeerAddr(
        "192.168.8.203:52301"
            .parse()
            .expect("parse the peer address"),
    ));

    let bundle = server
        .refresh_token(request)
        .await
        .expect("the token service answered an error")
        .into_inner()
        .data
        .expect("the response carries no bundle");

    assert_eq!(bundle.token.split('.').count(), 3, "{}", bundle.token);
    assert!(!bundle.client_token.is_empty());

    // The robot is not in the bot-info file, so the GUID is parked under the
    // peer's host for the `ReadDocs` that follows to claim.
    let primary = state.tokens().primary_snapshot();
    assert_eq!(primary.len(), 1);
    assert_eq!(primary[0].target, "192.168.8.203");
    assert_eq!(primary[0].guid, bundle.client_token);

    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn an_unparseable_session_certificate_is_an_error_rather_than_a_crash() {
    let (state, root) = temp_state("token-associate");
    let server = new_token_server(state);

    let status = server
        .associate_primary_user(Request::new(AssociatePrimaryUserRequest::default()))
        .await
        .expect_err("an empty certificate must not be accepted");

    assert_eq!(status.code(), Code::InvalidArgument);

    let _ = fs::remove_dir_all(&root);
}
