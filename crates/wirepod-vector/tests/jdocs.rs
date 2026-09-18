//! `PullJdocs` across the seam, against a real gRPC server on loopback.
//!
//! This is the call the jdocs pinger makes to pull `vic.RobotSettings` off a
//! robot that has just reconnected (`sdkapp/jdocspinger.go:112-126`), and the
//! one `/api-sdk/get_sdk_settings` makes to read the same document
//! (`sdkapp/server.go:200-223`). Both Go sites index `NamedJdocs[0]` with no
//! length check and dereference the `Doc` pointer inside it without a nil
//! check, so the two answers a robot is free to send that Go cannot survive are
//! tested here as errors rather than as panics.
//!
//! Everything runs through `test_support::spawn_fake_robot` and its TLS twin,
//! both of which bind `127.0.0.1:0`, so no firewall prompt appears and nothing
//! here touches the robot or the Go server.
//!
//! Every test wraps its body in a real-clock `tokio::time::timeout`. Nothing
//! here pauses the clock: the ceiling exists to turn a regression into a
//! failure instead of a hung CI job.

use std::sync::Arc;
use std::time::Duration;

use wirepod_core::{ConnTarget, Esn, Jdoc, JdocKind, RobotConn, RobotConnFactory, StatusCode};
use wirepod_vector::test_support::{
    FakeRobotHandle, RecordedJdocsRequest, ScriptedDoc, ScriptedJdoc, spawn_fake_robot,
    spawn_fake_robot_tls,
};
use wirepod_vector::{TonicConnFactory, plaintext_builder};

/// The ceiling every test runs under.
const CEILING: Duration = Duration::from_secs(5);

/// The GUID the tests authenticate with. Never a real one.
const GUID: &str = "<guid>";

/// The document body the fake hands back. Not a robot's: a settings document
/// small enough to read, with a `<` in it so that nothing here can quietly
/// start depending on the escaping the store does on the way to disk.
const JSON_DOC: &str = "{\"locale\":\"en-US\",\"master_volume\":\"<unset>\"}";

/// The target every test dials, with the fake's address in it.
fn target(authority: &str) -> ConnTarget {
    ConnTarget {
        esn: Esn::new("00303F28"),
        ip: authority.to_owned(),
        guid: GUID.to_owned(),
    }
}

/// Spawns the plaintext fake and connects to it through the production factory.
async fn connect() -> (Arc<dyn RobotConn>, FakeRobotHandle) {
    let (addr, handle) = spawn_fake_robot().await;
    let conn = TonicConnFactory::with_endpoint_builder(plaintext_builder())
        .connect(&target(&addr.to_string()))
        .await
        .expect("dial the fake robot");
    (conn, handle)
}

/// One complete `vic.RobotSettings` entry, tagged with the kind `kind` names.
fn scripted(kind: JdocKind) -> ScriptedJdoc {
    ScriptedJdoc {
        jdoc_type: kind.as_wire(),
        doc: Some(ScriptedDoc {
            doc_version: 41,
            fmt_version: 1,
            client_metadata: "wirepod-new-token".to_owned(),
            json_doc: JSON_DOC.to_owned(),
        }),
    }
}

/// The domain document [`scripted`] should arrive as.
fn expected_doc() -> Jdoc {
    Jdoc {
        doc_version: 41,
        fmt_version: 1,
        client_metadata: "wirepod-new-token".to_owned(),
        json_doc: JSON_DOC.to_owned(),
        ..Jdoc::default()
    }
}

#[tokio::test]
async fn a_robot_settings_answer_arrives_with_all_four_fields() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        handle.set_jdocs(vec![scripted(JdocKind::RobotSettings)]);

        let jdocs = conn
            .pull_jdocs(&[JdocKind::RobotSettings])
            .await
            .expect("the scripted answer");

        assert_eq!(jdocs.len(), 1);
        assert_eq!(jdocs[0].kind, JdocKind::RobotSettings);
        // Compared whole, so a field that stops being copied fails here. These
        // are the four `pingJdocs` copies into `vars.AJdoc`
        // (`sdkapp/jdocspinger.go:122-125`), which is the struct this is.
        assert_eq!(jdocs[0].doc, expected_doc());

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn the_request_carries_exactly_the_kinds_asked_for() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        handle.set_jdocs(vec![scripted(JdocKind::RobotSettings)]);

        // What both Go call sites send (`sdkapp/jdocspinger.go:112-114`,
        // `sdkapp/server.go:200-202`).
        conn.pull_jdocs(&[JdocKind::RobotSettings])
            .await
            .expect("the scripted answer");
        assert_eq!(
            handle.last_jdocs_request(),
            Some(RecordedJdocsRequest {
                jdoc_types: vec![0]
            })
        );

        // And an order and a length nothing in the port uses yet, so that the
        // mapping is pinned for every value rather than for the one the pinger
        // happens to ask for.
        handle.set_jdocs(vec![
            scripted(JdocKind::UserEntitlements),
            scripted(JdocKind::AccountSettings),
        ]);
        conn.pull_jdocs(&[
            JdocKind::UserEntitlements,
            JdocKind::RobotLifetimeStats,
            JdocKind::AccountSettings,
        ])
        .await
        .expect("the scripted answer");
        assert_eq!(
            handle.last_jdocs_request(),
            Some(RecordedJdocsRequest {
                jdoc_types: vec![3, 1, 2]
            })
        );

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn every_entry_of_the_answer_is_converted_in_order() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        handle.set_jdocs(vec![
            scripted(JdocKind::AccountSettings),
            scripted(JdocKind::RobotLifetimeStats),
        ]);

        let jdocs = conn
            .pull_jdocs(&[JdocKind::AccountSettings, JdocKind::RobotLifetimeStats])
            .await
            .expect("the scripted answer");

        assert_eq!(
            jdocs.iter().map(|named| named.kind).collect::<Vec<_>>(),
            vec![JdocKind::AccountSettings, JdocKind::RobotLifetimeStats]
        );
        for named in &jdocs {
            assert_eq!(named.doc, expected_doc());
        }

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn an_answer_with_no_documents_is_an_error_where_go_panics() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        // The fake's default, said out loud: this is the answer Go's
        // `NamedJdocs[0]` takes the process down on.
        handle.set_jdocs(Vec::new());

        let err = conn
            .pull_jdocs(&[JdocKind::RobotSettings])
            .await
            .expect_err("an answer with no documents is not usable");

        assert_eq!(err.code, StatusCode::Internal);
        assert_eq!(
            err.to_string(),
            "rpc error: code = Internal desc = robot answered PullJdocs with no documents"
        );

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn an_entry_with_no_document_is_an_error_where_go_panics() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        handle.set_jdocs(vec![ScriptedJdoc {
            jdoc_type: JdocKind::RobotSettings.as_wire(),
            doc: None,
        }]);

        let err = conn
            .pull_jdocs(&[JdocKind::RobotSettings])
            .await
            .expect_err("an absent document is not usable");

        // The alternative, a default document, would replace a good
        // `vic.RobotSettings` on disk with an empty one and say nothing.
        assert_eq!(err.code, StatusCode::Internal);
        assert_eq!(
            err.to_string(),
            "rpc error: code = Internal desc = robot answered PullJdocs with an absent \
             RobotSettings document"
        );

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn a_failed_pull_renders_the_way_grpc_go_prints_it() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        // What the robot answers an SDK client it has not authenticated, which
        // is the failure the pinger logs as `pull jdocs: <err>`
        // (`sdkapp/jdocspinger.go:115-117`).
        handle.fail_jdocs(tonic::Status::unauthenticated("invalid token"));

        let err = conn
            .pull_jdocs(&[JdocKind::RobotSettings])
            .await
            .expect_err("scripted failure");

        assert_eq!(err.code, StatusCode::Unauthenticated);
        assert_eq!(
            err.to_string(),
            "rpc error: code = Unauthenticated desc = invalid token"
        );

        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn a_pull_round_trips_through_the_tls_channel() {
    tokio::time::timeout(CEILING, async {
        let (addr, handle) = spawn_fake_robot_tls().await;
        handle.set_jdocs(vec![scripted(JdocKind::RobotSettings)]);

        let conn = TonicConnFactory::insecure_tls()
            .connect(&target(&addr.to_string()))
            .await
            .expect("dial the fake robot over TLS");
        let jdocs = conn
            .pull_jdocs(&[JdocKind::RobotSettings])
            .await
            .expect("the scripted answer");

        assert_eq!(jdocs.len(), 1);
        assert_eq!(jdocs[0].doc, expected_doc());
        // The credential rides on this call as it does on every other, over the
        // transport the robot actually uses.
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
