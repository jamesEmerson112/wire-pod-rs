//! Helpers the router tests share, behind the non-default `test-util` feature.
//!
//! Items behind `#[cfg(test)]` are not visible from an integration-test binary,
//! and four of them need the same three things: an [`AppState`] over
//! `wirepod-core`'s fakes, a way to send a request through the router without
//! binding a port, and a response collected into something comparable. So they
//! live here, as `wirepod-core` and `wirepod-vector` do it, and the crate lists
//! itself as a dev-dependency with this feature on.
//!
//! Nothing here opens a socket. Requests go through `tower`'s `oneshot`, which
//! calls the router as the service it is.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use http::{HeaderMap, Method, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;
use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn};
use wirepod_core::{
    AppState, BotInfo, ConnError, ManualClock, RobotConn, RobotConnFactory, StatusCode as ConnCode,
    Timings,
};

use crate::router;

/// The serial of the robot on this machine, which every fixture uses.
pub const TEST_ESN: &str = "00303f28";

/// The two headers `apiHandler` sets before it dispatches
/// (`webserver.go:28-29`), and which nothing else on this surface sets.
///
/// Half the header contract is an absence: `/api-sdk/*`, `/ok` and the file
/// server carry neither of these, which is what tells an unknown `/api/*` path
/// apart from a path that missed the prefix entirely.
pub const CORS_HEADERS: [&str; 2] = [
    "access-control-allow-origin",
    "access-control-allow-headers",
];

/// The three cache headers only the two file-server mounts set
/// (`webserver.go:415-421`, `server.go:791-798`).
///
/// No API route on either prefix sets any of them, and the file server's own
/// 404 loses `Cache-Control` on the way out, so this whole list is absent from
/// every response the slice serves except the file-server 404's `Pragma` and
/// `Expires`.
pub const CACHE_HEADERS: [&str; 3] = ["cache-control", "pragma", "expires"];

/// That robot's address.
pub const TEST_IP: &str = "192.168.8.203";

/// A bot-info file holding one authenticated robot.
///
/// The GUID is a placeholder. A real robot GUID never goes into a fixture, a
/// document or a log line.
pub fn one_robot() -> BotInfo {
    serde_json::from_str(concat!(
        r#"{"global_guid":"global-guid-placeholder","robots":[{"esn":"00303f28","#,
        r#""ip_address":"192.168.8.203","guid":"robot-guid-placeholder","activated":true}]}"#
    ))
    .expect("parse the one-robot fixture")
}

/// A bot-info file with no robots, which is what a fresh install has.
pub fn no_robots() -> BotInfo {
    BotInfo::default()
}

/// The failure a robot that is not answering produces.
pub fn unreachable_error() -> ConnError {
    ConnError::new(
        ConnCode::Unavailable,
        "connection error: desc = \"transport: Error while dialing: dial tcp 192.168.8.203:443: connect: connection refused\"",
    )
}

/// A router, the state behind it, and the handles a test drives them with.
pub struct TestServer {
    /// The router under test, already carrying its state.
    pub router: Router,
    /// The state the handlers read.
    pub state: Arc<AppState>,
    /// The clock the idle timer and the pinger measure against.
    pub clock: Arc<ManualClock>,
    /// The dial seam, for asserting how many connects a request cost.
    pub factory: Arc<FakeConnFactory>,
}

impl TestServer {
    /// A server whose robot answers every call.
    pub fn connected(bot_info: BotInfo) -> Self {
        let conn: Arc<dyn RobotConn> = Arc::new(FakeRobotConn::new());
        Self::with_robot(bot_info, conn)
    }

    /// A server dialling `conn`, so a test can script the robot's answers and
    /// read back what it was asked.
    ///
    /// The caller keeps its own `Arc<FakeRobotConn>` and clones it in here, so
    /// it can arm a gate or queue a receiver while the request is in flight.
    pub fn with_robot(bot_info: BotInfo, conn: Arc<dyn RobotConn>) -> Self {
        Self::with_robot_and_timings(bot_info, conn, Timings::default())
    }

    /// The same, waiting on `timings` rather than on the Go defaults.
    ///
    /// The probe deadline is the one a handler test wants to move: setting it
    /// to zero against a robot that has not answered yet is how the lost-probe
    /// body is driven without waiting five seconds for it.
    pub fn with_robot_and_timings(
        bot_info: BotInfo,
        conn: Arc<dyn RobotConn>,
        timings: Timings,
    ) -> Self {
        Self::build(
            bot_info,
            Arc::new(FakeConnFactory::connecting_to(conn)),
            timings,
        )
    }

    /// A server whose robot never answers, so every dial fails.
    ///
    /// This is what drives the preamble: a serial the bot-info file knows, and
    /// a connect that then fails, is the only way to tell the exemption apart
    /// from an unknown serial.
    pub fn unreachable(bot_info: BotInfo) -> Self {
        Self::build(
            bot_info,
            Arc::new(FakeConnFactory::failing(unreachable_error())),
            Timings::default(),
        )
    }

    fn build(bot_info: BotInfo, factory: Arc<FakeConnFactory>, timings: Timings) -> Self {
        let clock = Arc::new(ManualClock::new());
        let dialler: Arc<dyn RobotConnFactory> = Arc::clone(&factory) as Arc<dyn RobotConnFactory>;
        let state = AppState::builder(dialler)
            .bot_info(bot_info)
            .timings(timings)
            .clock(Arc::clone(&clock) as Arc<dyn wirepod_core::Clock>)
            .build();
        Self {
            router: router::build_router(Arc::clone(&state)),
            state,
            clock,
            factory,
        }
    }

    /// Sends a GET, which is how the dashboard fetches `get_stim_status`,
    /// `net_probe` and the images.
    pub async fn get(&self, uri: &str) -> Reply {
        self.send(request(Method::GET, uri, None)).await
    }

    /// Sends the POST the dashboard actually sends: the urlencoded content
    /// type, an empty body, and every parameter in the query string
    /// (`webroot/sdkapp/js/main.js:150-152`).
    pub async fn post(&self, uri: &str) -> Reply {
        self.send(request(Method::POST, uri, Some(""))).await
    }

    /// Sends a POST with a urlencoded body, which is what `custom_eye_color`
    /// does (`main.js:248-258`).
    pub async fn post_form(&self, uri: &str, body: &str) -> Reply {
        self.send(request(Method::POST, uri, Some(body))).await
    }

    /// Sends an already-built request.
    pub async fn send(&self, req: Request) -> Reply {
        send_to(&self.router, req).await
    }
}

/// The real-clock ceiling every asynchronous helper and test runs under.
///
/// Nothing here pauses the clock. The ceiling exists to turn a regression into
/// a failure rather than a hung CI job, and it is generous enough that a slow
/// runner never trips it.
pub const CEILING: std::time::Duration = std::time::Duration::from_secs(5);

/// Yields until `ready` answers true, or fails at [`CEILING`].
///
/// A detached receiver publishes its first reading on its own schedule, so a
/// test that asserted straight after a `begin_event_stream` would be racing the
/// task it just spawned. Yielding rather than sleeping keeps the wait as short
/// as the runtime allows and needs no chosen interval.
///
/// `what` names the condition, because a timeout here reads as a hang and the
/// message is the only clue about which one.
pub async fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(CEILING, async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// Sends one request through a router and collects the response.
///
/// Free rather than a method because the listener test drives the same router
/// value once per listener spec, and because C11's live-seam test will build
/// its router from a different state.
pub async fn send_to(router: &Router, req: Request) -> Reply {
    let response = router
        .clone()
        .oneshot(req)
        .await
        .expect("the router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("collect the response body")
        .to_bytes();
    Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

/// Builds a request, with the urlencoded content type when there is a body.
pub fn request(method: Method, uri: &str, body: Option<&str>) -> Request {
    let builder = http::Request::builder().method(method).uri(uri);
    match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
            .expect("build a request with a body"),
        None => builder.body(Body::empty()).expect("build a request"),
    }
}

/// A response, collected.
#[derive(Clone, Debug)]
pub struct Reply {
    /// The status code.
    pub status: StatusCode,
    /// Every response header.
    pub headers: HeaderMap,
    /// The body, as text. Every body on this surface is text or JSON.
    pub body: String,
}

impl Reply {
    /// One header's value, or `None` when it is absent.
    ///
    /// The absence matters as much as the value: the fallback 404 is defined
    /// partly by the `Cache-Control` it does **not** carry.
    pub fn header(&self, name: header::HeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// One header's value, looked up by name rather than by [`header`] const.
    ///
    /// [`CORS_HEADERS`] and [`CACHE_HEADERS`] are lists of names, and this is
    /// what lets a test loop over one of them.
    ///
    /// [`header`]: http::header
    pub fn header_str(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// The `Content-Type`, or `None` for the zero-byte bodies that carry none.
    pub fn content_type(&self) -> Option<&str> {
        self.header(header::CONTENT_TYPE)
    }

    /// Asserts that none of `names` is on the response.
    ///
    /// `context` names the request, because every caller loops over several.
    pub fn assert_absent(&self, names: &[&str], context: &str) {
        for name in names {
            assert!(
                !self.headers.contains_key(*name),
                "{context} must not carry {name}"
            );
        }
    }
}
