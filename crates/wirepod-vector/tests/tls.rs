//! The TLS dialler against a real gRPC server on loopback.
//!
//! `spawn_fake_robot` speaks plaintext, so nothing in `loopback.rs` exercises a
//! handshake. These tests use its TLS twin, which serves the vendored
//! escape-pod certificate. That certificate is self-signed and no root store
//! trusts it, which is the point: every handshake that completes here completes
//! because [`InsecureTlsConnector`]'s accept-all verifier accepted a chain a
//! verifying client would have rejected, exactly as the Go SDK's
//! `WithInsecureSkipVerify` arranges
//! (`vector-go-sdk@v0.0.0-20231108155304-62168f3595d6/pkg/vector/vector.go:44`).
//!
//! Nothing here dials the robot. The only address these tests use is
//! `127.0.0.1` on an ephemeral port.
//!
//! Every test wraps its body in a real-clock `tokio::time::timeout`. Nothing
//! here pauses the clock: the ceiling exists to turn a regression into a
//! failure instead of a hung CI job.

use std::net::SocketAddr;
use std::time::Duration;

use rustls::ProtocolVersion;
use rustls::pki_types::CertificateDer;
use tonic::transport::Uri;
use tower::ServiceExt;
use wirepod_core::{BatteryLevel, ConnTarget, Esn, RobotConnFactory, StatusCode};
use wirepod_vector::test_support::{
    TlsVersions, epod_certificate_chain, spawn_fake_robot_tls, spawn_fake_robot_tls_with,
};
use wirepod_vector::{InsecureTlsConnector, TonicConnFactory};

/// The ceiling every test runs under.
const CEILING: Duration = Duration::from_secs(5);

/// The GUID the tests authenticate with. Never a real one.
const GUID: &str = "<guid>";

/// The target that names a loopback address, port and all.
fn target(authority: &str) -> ConnTarget {
    ConnTarget {
        esn: Esn::new("00303F28"),
        ip: authority.to_owned(),
        guid: GUID.to_owned(),
    }
}

/// What one handshake settled on.
struct Negotiated {
    alpn: Option<Vec<u8>>,
    version: Option<ProtocolVersion>,
    chain: Vec<CertificateDer<'static>>,
}

/// Runs the production connector against `addr` and reports what it agreed to.
///
/// The stream is dropped before returning. A raw connection that never sends an
/// HTTP/2 preface would otherwise sit in the fake's accepted set and hold up
/// its graceful shutdown.
async fn negotiate(addr: SocketAddr) -> Negotiated {
    let uri: Uri = format!("https://{addr}").parse().expect("a loopback uri");
    let io = InsecureTlsConnector::new()
        .oneshot(uri)
        .await
        .expect("the handshake completes");
    let stream = io.into_inner();
    let (_socket, session) = stream.get_ref();
    Negotiated {
        alpn: session.alpn_protocol().map(<[u8]>::to_vec),
        version: session.protocol_version(),
        chain: session
            .peer_certificates()
            .unwrap_or_default()
            .iter()
            .map(|cert| cert.clone().into_owned())
            .collect(),
    }
}

#[tokio::test]
async fn the_handshake_accepts_a_certificate_no_root_would_trust() {
    tokio::time::timeout(CEILING, async {
        let (addr, handle) = spawn_fake_robot_tls().await;

        let negotiated = negotiate(addr).await;

        assert_eq!(
            negotiated.chain,
            epod_certificate_chain(),
            "the chain that was accepted is the self-signed escape-pod one"
        );
        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn the_handshake_negotiates_h2() {
    tokio::time::timeout(CEILING, async {
        let (addr, handle) = spawn_fake_robot_tls().await;

        let negotiated = negotiate(addr).await;

        // grpc-go offers exactly this, by way of `AppendH2ToNextProtos` inside
        // `credentials.NewTLS`
        // (`google.golang.org/grpc@v1.82.1/credentials/tls.go:239`). Without it
        // the robot's gateway has no way to know an HTTP/2 preface is coming.
        assert_eq!(negotiated.alpn.as_deref(), Some(&b"h2"[..]));
        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn an_rpc_round_trips_through_the_tls_channel() {
    tokio::time::timeout(CEILING, async {
        let (addr, handle) = spawn_fake_robot_tls().await;
        handle.set_battery(3, 4.11);

        let conn = TonicConnFactory::insecure_tls()
            .connect(&target(&addr.to_string()))
            .await
            .expect("dial the fake robot over TLS");
        let battery = conn.battery_state().await.expect("battery state");

        assert_eq!(battery.level, BatteryLevel::Full);
        assert!((battery.volts - 4.11).abs() < f32::EPSILON, "{battery:?}");
        // The credential still rides on the call: the handshake is a new
        // transport underneath the same client, not a new client.
        assert_eq!(
            handle
                .calls()
                .first()
                .and_then(|call| call.authorization.clone())
                .as_deref(),
            Some("Bearer <guid>")
        );
        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn a_tls12_only_robot_still_answers() {
    tokio::time::timeout(CEILING, async {
        let (addr, handle) = spawn_fake_robot_tls_with(TlsVersions::Tls12Only).await;

        let negotiated = negotiate(addr).await;
        assert_eq!(negotiated.version, Some(ProtocolVersion::TLSv1_2));
        assert_eq!(negotiated.alpn.as_deref(), Some(&b"h2"[..]));

        // And the whole channel works over it, because TLS 1.2 is the floor
        // grpc-go sets and the version an old Vector gateway is likeliest to
        // offer.
        let conn = TonicConnFactory::insecure_tls()
            .connect(&target(&addr.to_string()))
            .await
            .expect("dial the TLS 1.2 fake robot");
        conn.battery_state().await.expect("battery state");
        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn a_tls_dial_to_a_closed_port_is_unavailable() {
    tokio::time::timeout(CEILING, async {
        // Bind, read the port back, then drop the listener. Nothing answers on
        // that port afterwards, which is the same shape as a robot that is not
        // on the network.
        let closed = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback");
            listener.local_addr().expect("loopback address")
        };

        let Err(err) = TonicConnFactory::insecure_tls()
            .connect(&target(&closed.to_string()))
            .await
        else {
            panic!("nothing is listening on that port, so the dial must fail");
        };

        assert_eq!(err.code, StatusCode::Unavailable);
        // The prefix is the contract, not the text after it. Deviation 23
        // records why the wording differs from grpc-go's and why no test pins
        // it.
        assert!(
            err.to_string()
                .starts_with("rpc error: code = Unavailable desc = "),
            "{err}"
        );
    })
    .await
    .expect("within the ceiling");
}
