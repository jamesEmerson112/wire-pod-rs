//! `serve_plain` over a real socket.
//!
//! Every other test in this crate drives the router through `tower`'s
//! `oneshot`, which never opens a connection. This one exists for the two
//! things that cannot prove themselves that way: that an actual HTTP/1.1
//! request arriving on a TCP socket reaches the router and comes back with the
//! body Go writes, and that cancelling the token ends the call rather than
//! leaving the listener accepting.
//!
//! The request is written by hand. No HTTP client crate is in `Cargo.lock`, and
//! adding one to prove that axum speaks HTTP would be a strange trade; the
//! three lines of a request and a read to end-of-response are enough, because
//! the response has `Content-Length` and the connection closes on `Connection:
//! close`.
//!
//! `/ok` is the path under test because it is the one route with no state
//! behind it at all, so a failure here is the listener rather than a handler.
//! The state still has to exist, and it is built over the same
//! `FakeConnFactory` the rest of the suite uses, so nothing in this file can
//! dial anything.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use wirepod_server::test_support::{TestServer, one_robot};
use wirepod_server::{literals, serve_plain};

/// A real-clock bound on both waits. Nothing here sleeps, so this only fires on
/// a regression: a request that never gets answered, or a shutdown that never
/// completes.
const CEILING: Duration = Duration::from_secs(5);

/// Sends one HTTP/1.1 request over a fresh connection and returns everything
/// the server wrote back.
///
/// `Connection: close` is what makes the read terminate: the server closes once
/// the response is written, so `read_to_end` sees end-of-file rather than
/// waiting for a keep-alive connection to time out.
async fn round_trip(port: u16, path: &str) -> String {
    let mut socket = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to the listener");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    socket
        .write_all(request.as_bytes())
        .await
        .expect("write the request");
    let mut response = Vec::new();
    socket
        .read_to_end(&mut response)
        .await
        .expect("read the response");
    String::from_utf8_lossy(&response).into_owned()
}

#[tokio::test]
async fn a_request_over_a_real_socket_reaches_the_router_and_a_cancel_ends_the_serve() {
    let server = TestServer::connected(one_robot());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let port = listener
        .local_addr()
        .expect("read the bound address")
        .port();

    let cancel = CancellationToken::new();
    let serving = tokio::spawn(serve_plain(listener, server.router.clone(), cancel.clone()));

    let response = tokio::time::timeout(CEILING, round_trip(port, "/ok"))
        .await
        .expect("the request is answered inside the ceiling");

    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "the heartbeat answers 200: {response:?}"
    );
    let body = response
        .split_once("\r\n\r\n")
        .expect("the response has a header block")
        .1;
    assert_eq!(
        body,
        literals::OK,
        "the body is Go's `ok` with no trailing newline"
    );

    cancel.cancel();
    tokio::time::timeout(CEILING, serving)
        .await
        .expect("the serve returns inside the ceiling once the token fires")
        .expect("the serve task did not panic")
        .expect("the accept loop reported no error");

    // The socket is released, not merely stopped being polled, which a
    // returning future alone does not prove. Rebinding the same port is the
    // instant way to say so: a listening socket still open would fail this bind
    // with `address in use` on both CI platforms, since neither tokio's Windows
    // bind nor `SO_REUSEADDR` on Linux lets a second listener join a live one.
    //
    // Connecting to the port and expecting a refusal would say the same thing
    // and costs two seconds of real clock on Windows, which retries the SYN
    // before it reports the refusal.
    TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("the port is free once the serve has returned");
}
