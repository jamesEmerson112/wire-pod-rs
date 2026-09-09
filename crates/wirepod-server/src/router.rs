//! The router: one value, served by both listeners.
//!
//! Go puts every route on `http.DefaultServeMux` and then serves that same mux
//! from two listeners, port 80 (`server.go:824`) and the configurable web port
//! (`webserver.go:441`), so every route answers on both. [`build_router`]
//! builds the union once and [`listener_specs`] describes the two listeners
//! that will serve it. Nothing here binds anything; the listeners arrive with
//! the TLS work in P1.
//!
//! Two of Go's mux behaviours have to be built by hand. A subtree pattern such
//! as `/api-sdk/` matches the bare prefix as well as everything under it, while
//! axum's wildcard does not match the bare prefix, so each prefix is registered
//! twice against the same handler. And the bare `/api-sdk`, without the
//! trailing slash, never reaches the handler at all: the mux answers a 301 to
//! `/api-sdk/`.

use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::response::Response;
use axum::routing::any;
use wirepod_core::AppState;

use crate::{api, conncheck, reply, sdkapp};

/// The port `BeginServer` serves the mux from, for the robot's conn check
/// (`server.go:824`).
pub const CONN_CHECK_PORT: u16 = 80;

/// The default `vars.WebPort`, which `StartWebServer` serves the same mux from
/// (`vars.go:57`, `webserver.go:441`). `WEBSERVER_PORT` overrides it, which
/// arrives with the config work in P1.
pub const DEFAULT_WEB_PORT: u16 = 8080;

/// The conn-check path carrying a literal colon (`server.go:813`).
///
/// `matchit` 0.7, which axum 0.7 uses, reads a colon as the path-parameter
/// sigil, so this cannot be registered as a route. It is matched as a literal
/// inside the fallback instead, which is the trick spike S1 proved against the
/// real robot. When tonic is eventually upgraded and `matchit` moves to 0.8 a
/// literal colon becomes legal and this can become an ordinary route; keeping
/// the handling in one place is what makes that a local change, and the test
/// asserts the behaviour rather than the mechanism.
const OK_COLON_80: &str = "/ok:80";

/// The plain conn-check path (`server.go:814`).
const OK: &str = "/ok";

/// One listener the finished server will bind.
///
/// This is a description, not a socket. It exists so that the fact that both
/// listeners serve the same routes is written down and tested before anything
/// binds a port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenerSpec {
    /// A short name for logs and errors.
    pub name: &'static str,
    /// The TCP port.
    pub port: u16,
    /// What the port is for, in Go's terms.
    pub purpose: &'static str,
}

/// The two listeners that serve [`build_router`]'s value.
///
/// Go's own comment for port 80 is "Starting server at port 80 for connCheck"
/// (`server.go:822`), and the web port is what the dashboard is served from.
/// Both serve the union of every route, so a route reachable on one is
/// reachable on the other.
pub fn listener_specs() -> [ListenerSpec; 2] {
    [
        ListenerSpec {
            name: "conn-check",
            port: CONN_CHECK_PORT,
            purpose: "the robot's liveness heartbeat, /ok and /ok:80",
        },
        ListenerSpec {
            name: "web",
            port: DEFAULT_WEB_PORT,
            purpose: "the dashboard and the whole HTTP API",
        },
    ]
}

/// The whole HTTP surface the slice serves.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // A Go subtree pattern matches its own bare prefix; axum's wildcard
        // does not, so both forms go to the same handler. `GET /api-sdk/` with
        // an empty rest is a real request: it pays for the preamble with an
        // empty serial and answers the doubled not-found error at 200.
        .route(sdkapp::PREFIX, any(sdkapp::handle))
        .route("/api-sdk/*rest", any(sdkapp::handle))
        .route("/api-sdk", any(moved_to_sdkapp))
        .route(api::PREFIX, any(api::handle))
        .route("/api/*rest", any(api::handle))
        .route("/api", any(moved_to_api))
        .route(OK, any(conncheck::handle))
        .fallback(fallback)
        .with_state(state)
}

/// `/api-sdk` without its trailing slash.
async fn moved_to_sdkapp(req: Request) -> Response {
    reply::moved_permanently(sdkapp::PREFIX, req.method(), req.uri().query())
}

/// `/api` without its trailing slash.
async fn moved_to_api(req: Request) -> Response {
    reply::moved_permanently(api::PREFIX, req.method(), req.uri().query())
}

/// Everything none of the registered patterns matched.
///
/// Two paths land here. `/ok:80` is a route in Go and is served as one, for the
/// `matchit` reason above. Everything else is Go's root file server, which for
/// a path with no file behind it answers `404 page not found\n` with four
/// headers and no `Cache-Control`. Static file serving itself is P4 work; until
/// then every path that would have hit a file gets the same 404 a missing file
/// gets, which is the one thing about this fallback that is a stub rather than
/// a contract.
async fn fallback(req: Request) -> Response {
    if req.uri().path() == OK_COLON_80 {
        return conncheck::handle(req).await;
    }
    reply::file_not_found()
}
