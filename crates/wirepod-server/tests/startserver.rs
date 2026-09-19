//! The chipper listener: its two conn-check routes, and one TLS round trip on
//! an ephemeral loopback port.
//!
//! Nothing here calls `start_chipper`, which binds the configured port and
//! 8084; only `serve_listeners`, which takes listeners the test bound itself.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use http::StatusCode;
use http_body_util::BodyExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use wirepod_server::chipper::{Options, Server};
use wirepod_server::startserver::{build_router, load_tls, serve_listeners};

const CERT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/epod/ep.crt");
const KEY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/epod/ep.key");

/// A real-clock bound on the waits. Nothing here sleeps, so it only fires on a
/// regression.
const CEILING: Duration = Duration::from_secs(5);

async fn body_of(router: axum::Router, path: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("build the request");
    let response = router.oneshot(request).await.expect("the router answers");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn both_conn_check_paths_answer_ok_and_anything_else_is_a_404() {
    let router = build_router(Server::new(Options::new()));

    assert_eq!(
        body_of(router.clone(), "/ok").await,
        (StatusCode::OK, "ok".to_owned())
    );
    assert_eq!(
        body_of(router.clone(), "/ok:80").await,
        (StatusCode::OK, "ok".to_owned())
    );
    assert_eq!(
        body_of(router, "/nothing").await.0,
        StatusCode::NOT_FOUND,
        "an unrouted path is Go's file-server 404"
    );
}

#[tokio::test]
async fn a_tls_request_reaches_the_router_and_a_cancel_frees_the_port() {
    let tls = load_tls(Path::new(CERT), Path::new(KEY)).expect("load the escape-pod key pair");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral loopback port");
    let port = listener
        .local_addr()
        .expect("read the bound address")
        .port();

    let cancel = CancellationToken::new();
    let serving = tokio::spawn(serve_listeners(
        vec![listener],
        tls,
        build_router(Server::new(Options::new())),
        cancel.clone(),
    ));

    let mut config = wirepod_vector::tls::insecure_client_config();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to the listener");
    let name = rustls::pki_types::ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into());
    let mut stream = connector
        .connect(name, tcp)
        .await
        .expect("the TLS handshake completes");

    stream
        .write_all(b"GET /ok HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .expect("write the request");
    let mut raw = Vec::new();
    tokio::time::timeout(CEILING, stream.read_to_end(&mut raw))
        .await
        .expect("the response arrives inside the ceiling")
        .expect("read the response");
    let response = String::from_utf8_lossy(&raw);

    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "the heartbeat answers 200: {response:?}"
    );
    assert!(
        response.ends_with("ok"),
        "the body is Go's `ok` with no trailing newline: {response:?}"
    );

    cancel.cancel();
    tokio::time::timeout(CEILING, serving)
        .await
        .expect("the serve returns inside the ceiling once the token fires")
        .expect("the serve task did not panic");

    TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("the port is free once the serve has returned");
}
