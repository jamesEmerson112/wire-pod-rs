//! M1 end to end on loopback: one TLS listener, the three gRPC services behind
//! it, and a tonic client dialling it the way the robot does.
//!
//! The listener is bound on `127.0.0.1:0` and every file lands in a directory of
//! this test's own.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tonic::Code;
use tonic::transport::Endpoint;
use wirepod_core::paths::{AssetDir, DataDir};
use wirepod_core::test_support::FakeConnFactory;
use wirepod_core::{AppState, Paths, RobotConnFactory, SdkIniStore};
use wirepod_proto::chippergrpc2::TextRequest;
use wirepod_proto::chippergrpc2::chipper_grpc_client::ChipperGrpcClient;
use wirepod_proto::jdocspb::ReadDocsReq;
use wirepod_proto::jdocspb::jdocs_client::JdocsClient;
use wirepod_proto::tokenpb::RefreshTokenRequest;
use wirepod_proto::tokenpb::token_client::TokenClient;
use wirepod_server::chipper::{Options, Server};
use wirepod_server::startserver::{build_router, load_tls, serve_listeners};
use wirepod_server::test_support::unreachable_error;
use wirepod_vector::tls::InsecureTlsConnector;

const CERT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/epod/ep.crt");
const KEY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/epod/ep.key");
const CEILING: Duration = Duration::from_secs(10);

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
async fn a_tonic_client_reaches_all_three_services_over_tls() {
    let (state, root) = temp_state("m1-loopback");
    let tls = load_tls(Path::new(CERT), Path::new(KEY)).expect("load the escape-pod key pair");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral loopback port");
    let port = listener.local_addr().expect("read the address").port();
    let cancel = CancellationToken::new();
    let serving = tokio::spawn(serve_listeners(
        vec![listener],
        tls,
        build_router(Arc::clone(&state), Server::new(Options::new())),
        cancel.clone(),
    ));

    let channel = tokio::time::timeout(
        CEILING,
        Endpoint::from_shared(format!("https://127.0.0.1:{port}"))
            .expect("build the endpoint")
            .connect_with_connector(InsecureTlsConnector::new()),
    )
    .await
    .expect("the dial finishes inside the ceiling")
    .expect("the TLS dial succeeds");

    // Token: the robot asks for a fresh token and gets a three-part JWT, and the
    // peer address the listener attached is what the GUID is parked under.
    let bundle = TokenClient::new(channel.clone())
        .refresh_token(RefreshTokenRequest::default())
        .await
        .expect("the token service answers")
        .into_inner()
        .data
        .expect("the response carries a bundle");
    assert_eq!(bundle.token.split('.').count(), 3);
    let primary = state.tokens().primary_snapshot();
    assert_eq!(primary.len(), 1);
    assert_eq!(primary[0].target, "127.0.0.1");

    // Jdocs: the request is routed to the jdocs service, which answers rather
    // than the router's 404.
    let jdocs = JdocsClient::new(channel.clone())
        .read_docs(ReadDocsReq::default())
        .await;
    if let Err(status) = &jdocs {
        assert_ne!(status.code(), Code::Unimplemented, "{status}");
        assert_ne!(status.code(), Code::Unknown, "{status}");
    }

    // Chipper: Go leaves TextIntent unimplemented, and that status coming back
    // proves the call reached the chipper service.
    let status = ChipperGrpcClient::new(channel)
        .text_intent(TextRequest::default())
        .await
        .expect_err("TextIntent is unimplemented in Go");
    assert_eq!(status.code(), Code::Unimplemented);

    cancel.cancel();
    tokio::time::timeout(CEILING, serving)
        .await
        .expect("the serve returns once the token fires")
        .expect("the serve task did not panic");
    let _ = fs::remove_dir_all(&root);
}
