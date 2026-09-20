//! The router: one value, served by both listeners.
//!
//! Go puts every route on `http.DefaultServeMux` and then serves that same mux
//! from two listeners, port 80 (`server.go:824`) and the configurable web port
//! (`webserver.go:441`), so every route answers on both. [`build_router`]
//! builds the union once and [`listener_specs`] describes the two listeners
//! that will serve it. Nothing here binds anything; the listeners arrive with
//! the TLS work in P1.
//!
//! Three of Go's mux behaviours have to be built by hand. A subtree pattern
//! such as `/api-sdk/` matches the bare prefix as well as everything under it,
//! while axum's wildcard does not match the bare prefix, so each prefix is
//! registered twice against the same handler. The bare `/api-sdk`, without the
//! trailing slash, never reaches the handler at all: the mux answers a 301 to
//! `/api-sdk/`. And every request path is canonicalised before a pattern is
//! considered, which [`mux`](crate::mux) reproduces and [`build_router`] runs
//! as a layer in front of the route table.

use std::borrow::Cow;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::any;
use http::Uri;
use wirepod_core::AppState;

use crate::sdkapp::cam::CAM_STREAM_PATH;
use crate::{api, conncheck, initweb, mux, reply, sdkapp, ssh_api, webroot};

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

/// The two subtree prefixes without their trailing slash.
///
/// Go's mux answers these with a 301 to the prefix itself, and it does so ahead
/// of the redirect a path that needed cleaning would otherwise get
/// (`net/http/server.go:2686-2698`). That order is why `//api-sdk` answers
/// `Location: /api-sdk/` on the live server rather than `Location: /api-sdk`.
/// The routes below serve the already-clean form; [`canonicalise`] needs the
/// list for the other one.
const BARE_SUBTREE_PREFIXES: [&str; 2] = ["/api-sdk", "/api"];

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
    let routes = Router::new()
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
        .route("/api-chipper/", any(initweb::chipper_http_api))
        .route("/api-chipper/*rest", any(initweb::chipper_http_api))
        // `RegisterSSHAPI` (`webserver.go:426`) and `RegisterScriptingAPI`
        // (`server.go:801`) add their subtrees to the same mux, so they are
        // part of this union too.
        .route(ssh_api::PREFIX, any(ssh_api::ssh_setup))
        .route("/api-ssh/*rest", any(ssh_api::ssh_setup))
        .merge(wirepod_plugins::scripting::register_scripting_api())
        .route(OK, any(conncheck::handle))
        .route(CAM_STREAM_PATH, any(sdkapp::cam_stream::handle))
        // An exact pattern, so only this literal path reaches the file server
        // (`server.go:811`).
        .route(webroot::SDK_APP_PATH, any(webroot::sdk_app))
        // A subtree pattern, so the bare prefix reaches the handler too
        // (`webserver.go:429`).
        .route(webroot::SESSION_CERTS_PREFIX, any(webroot::cert_handler))
        .route("/session-certs/*rest", any(webroot::cert_handler))
        .fallback(fallback)
        .with_state(state);

    // `Router::layer` wraps each route's own service, so a layer added there
    // runs *after* the match and cannot change which pattern is chosen. Making
    // the whole route table the fallback of an empty router and layering that
    // puts the canonicalisation in front of the match, which is where Go has
    // it.
    Router::new()
        .fallback_service(routes)
        .layer(middleware::from_fn(canonicalise))
}

/// Go's `findHandler` preamble: clean the path, answer a 301 when that changed
/// it, and otherwise route on the unescaped form
/// (`net/http/server.go:2677-2699`).
///
/// The rewrite is what makes the rest of the crate see `r.URL.Path` rather than
/// the escaped request target. The route table then matches the decoded path,
/// [`fallback`] compares its literal against the decoded path, and the two
/// dispatch switches strip their prefix from the decoded path, all of which is
/// what Go's handlers do by reading `r.URL.Path`.
///
/// A path that is not rooted is left alone. The only one HTTP produces is the
/// `*` of `OPTIONS *`, which Go answers from `globalOptionsHandler` before the
/// mux ever sees it, so cleaning it here would invent a redirect Go does not
/// send.
async fn canonicalise(mut req: Request, next: Next) -> Response {
    if !req.uri().path().starts_with('/') {
        return next.run(req).await;
    }

    if let Cow::Owned(cleaned) = mux::clean(req.uri().path()) {
        // Go runs the trailing-slash redirect ahead of this one, so a path that
        // cleans to a bare subtree prefix goes to the prefix with its slash
        // rather than to the cleaned path.
        let target = if BARE_SUBTREE_PREFIXES.contains(&cleaned.as_str()) {
            format!("{cleaned}/")
        } else {
            cleaned
        };
        return reply::moved_permanently(&target, req.method(), req.uri().query());
    }

    let decoded = mux::unescape_segments(req.uri().path());
    if let Some(decoded) = decoded {
        let rewritten = with_path(req.uri(), &decoded);
        if let Some(rewritten) = rewritten {
            *req.uri_mut() = rewritten;
        }
    }
    next.run(req).await
}

/// The same URI with a different path, or `None` when that will not parse.
///
/// [`mux::unescape_segments`] already refuses every byte that would fail here
/// or change how the path splits into segments, so the `None` is a guard rather
/// than a case with a behaviour of its own: it leaves the path escaped, which
/// is what refusing the segment would have done.
fn with_path(uri: &Uri, path: &str) -> Option<Uri> {
    let path_and_query = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(path_and_query.parse().ok()?);
    Uri::from_parts(parts).ok()
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
/// Two things land here. `/ok:80` is a route in Go and is served as one, for
/// the `matchit` reason above; the path it is compared against has already been
/// unescaped by [`canonicalise`], which is what makes `GET /ok%3A80` answer
/// `ok` as it does on the live Go server. It has to be checked first, because
/// everything else is Go's root file server, which would otherwise be handed
/// the colon path and answer its 404.
async fn fallback(State(state): State<Arc<AppState>>, req: Request) -> Response {
    if req.uri().path() == OK_COLON_80 {
        return conncheck::handle(State(state), req).await;
    }
    webroot::web_root(&state, req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bare_prefixes_are_the_registered_subtrees_without_their_slash() {
        // The middleware and the route table have to name the same two
        // prefixes, and this is what says so if either side changes.
        assert_eq!(format!("{}/", BARE_SUBTREE_PREFIXES[0]), sdkapp::PREFIX);
        assert_eq!(format!("{}/", BARE_SUBTREE_PREFIXES[1]), api::PREFIX);
    }

    #[test]
    fn a_rewritten_path_keeps_the_query() {
        let uri: Uri = "/ok%3A80?runMDNS=true".parse().expect("parse the uri");
        let rewritten = with_path(&uri, "/ok:80").expect("rewrite the path");
        assert_eq!(rewritten.path(), "/ok:80");
        assert_eq!(rewritten.query(), Some("runMDNS=true"));

        let uri: Uri = "/%6Fk".parse().expect("parse the uri");
        let rewritten = with_path(&uri, "/ok").expect("rewrite the path");
        assert_eq!(rewritten.to_string(), "/ok");
        assert_eq!(rewritten.query(), None);
    }
}
