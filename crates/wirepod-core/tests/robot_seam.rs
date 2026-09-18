//! The seam calls the core fake answers that nothing in this crate drives yet.
//!
//! `FakeRobotConn` is what `wirepod-server`'s handler tests stand a robot up
//! with, so a call the fake answers but no code here makes has nothing
//! asserting it until the handler lands. `pull_jdocs` is in that position: the
//! jdocs pinger and the two `/api-sdk` handlers that make it
//! (`sdkapp/jdocspinger.go:112-126`, `sdkapp/server.go:200-223`,
//! `sdkapp/server.go:591-600`) are all later commits' work. What the fake
//! promises those commits is asserted here instead: that the call is recorded
//! with the kinds it was asked for, that `with_jdocs` is what it answers, and
//! what it answers when nothing scripted it.
//!
//! Everything goes through `Arc<dyn RobotConn>`, which is how the registry
//! hands a connection out, so the object safety the seam depends on is
//! exercised rather than assumed. The ceiling is real-clock and only fires on a
//! regression; nothing here pauses the clock.

use std::sync::Arc;
use std::time::Duration;

use wirepod_core::test_support::{FakeRobotConn, RobotCall};
use wirepod_core::{ConnError, Jdoc, JdocKind, NamedJdoc, RobotConn, StatusCode};

/// Nothing here waits on anything, so this only ever fires on a regression.
const CEILING: Duration = Duration::from_secs(2);

/// A scripted answer that is nothing's default: a lifetime-stats entry with
/// every field of the document set, so a fake that ignored the script and
/// handed back `NamedJdoc::default()` would be visible in any of the four
/// fields.
fn scripted() -> NamedJdoc {
    NamedJdoc {
        kind: JdocKind::RobotLifetimeStats,
        doc: Jdoc {
            doc_version: 12,
            fmt_version: 1,
            client_metadata: "wirepod-new-token".to_owned(),
            json_doc: "{\"Alive.seconds\":1000}".to_owned(),
            ..Jdoc::default()
        },
    }
}

async fn within<F: std::future::Future>(operation: F) -> F::Output {
    tokio::time::timeout(CEILING, operation)
        .await
        .expect("the operation did not finish inside the ceiling")
}

/// The request is recorded whole and in order, because that is what a handler
/// test asserts against: Go's three call sites each send exactly one kind
/// (`sdkapp/jdocspinger.go:113`, `sdkapp/server.go:201`,
/// `sdkapp/server.go:594`), and a handler that sent the wrong one would
/// otherwise pass.
#[tokio::test]
async fn the_fake_records_the_kinds_the_call_asked_for() {
    let fake = Arc::new(FakeRobotConn::new());
    let conn: Arc<dyn RobotConn> = fake.clone();

    within(conn.pull_jdocs(&[JdocKind::RobotSettings]))
        .await
        .expect("the default answer");
    within(conn.pull_jdocs(&[
        JdocKind::UserEntitlements,
        JdocKind::RobotLifetimeStats,
        JdocKind::AccountSettings,
    ]))
    .await
    .expect("the default answer");

    assert_eq!(
        fake.calls(),
        vec![
            RobotCall::PullJdocs(vec![JdocKind::RobotSettings]),
            RobotCall::PullJdocs(vec![
                JdocKind::UserEntitlements,
                JdocKind::RobotLifetimeStats,
                JdocKind::AccountSettings,
            ]),
        ]
    );
}

/// `with_jdocs` is what the call answers, on every call rather than once, and
/// the whole entry arrives rather than a default beside the right kind.
#[tokio::test]
async fn the_fake_answers_what_with_jdocs_scripted() {
    let fake = Arc::new(FakeRobotConn::new().with_jdocs(Ok(vec![scripted()])));
    let conn: Arc<dyn RobotConn> = fake.clone();

    let first = within(conn.pull_jdocs(&[JdocKind::RobotLifetimeStats]))
        .await
        .expect("the scripted answer");
    let second = within(conn.pull_jdocs(&[JdocKind::RobotLifetimeStats]))
        .await
        .expect("the scripted answer a second time");

    assert_eq!(first, vec![scripted()]);
    assert_eq!(second, vec![scripted()]);
}

/// The failure arm of the same setter, which is what a handler's error branch
/// is driven by. The rendering is the seam's, so the text a handler writes into
/// the body is pinned here too.
#[tokio::test]
async fn a_scripted_failure_reaches_the_caller_whole() {
    let err = ConnError::new(StatusCode::Unauthenticated, "invalid token");
    let fake = Arc::new(FakeRobotConn::new().with_jdocs(Err(err.clone())));
    let conn: Arc<dyn RobotConn> = fake.clone();

    let got = within(conn.pull_jdocs(&[JdocKind::RobotSettings]))
        .await
        .expect_err("the scripted failure");

    assert_eq!(got, err);
    assert_eq!(
        got.to_string(),
        "rpc error: code = Unauthenticated desc = invalid token"
    );
    assert_eq!(
        fake.calls(),
        vec![RobotCall::PullJdocs(vec![JdocKind::RobotSettings])],
        "a failing call is recorded like any other"
    );
}

/// The unscripted answer is one default `vic.RobotSettings` document rather
/// than an empty list, because the seam's contract is that an answer is either
/// usable or an error and Go's unchecked `NamedJdocs[0]` has nothing to index
/// in an empty one (`sdkapp/jdocspinger.go:122-125`).
#[tokio::test]
async fn the_unscripted_answer_is_one_default_document() {
    let fake = Arc::new(FakeRobotConn::new());
    let conn: Arc<dyn RobotConn> = fake.clone();

    let jdocs = within(conn.pull_jdocs(&[JdocKind::RobotSettings]))
        .await
        .expect("the default answer");

    assert_eq!(jdocs, vec![NamedJdoc::default()]);
    assert_eq!(jdocs[0].kind, JdocKind::RobotSettings);
    assert_eq!(jdocs[0].doc, Jdoc::default());
}
