//! `/api/*`: the web UI's own API, with CORS on every response.
//!
//! Go registers this prefix with a trailing slash too (`webserver.go:428`) and
//! dispatches on `strings.TrimPrefix(r.URL.Path, "/api/")` in an exact-string
//! switch (`webserver.go:31-78`). The two CORS headers are the first two
//! statements of the handler (`webserver.go:28-29`), so they are on every
//! response including the `default` case's 404, which is what distinguishes an
//! unknown `/api/*` path from a path that missed the prefix entirely and
//! reached the file server.
//!
//! The prefix is stripped from Go's `r.URL.Path`, which is decoded, so this
//! reads the path [`crate::router`]'s middleware has already unescaped rather
//! than the escaped request target.
//!
//! Twenty-two routes live here. The slice serves `get_bot_status` and stubs the
//! other 21 at 404; the stubs are listed by name in `deviations.md` and no test
//! asserts them.

pub mod bot_status;
pub mod certs;
pub mod config;
pub mod intents;
pub mod kg;
pub mod logs;
pub mod ota;
pub mod stt;
pub mod version;
pub mod weather;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::Response;
use wirepod_core::AppState;

use crate::reply;

/// The subtree prefix, registered with its trailing slash
/// (`webserver.go:428`).
pub const PREFIX: &str = "/api/";

/// The one handler behind `/api/` and everything under it.
pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let mut response = match req.uri().path().strip_prefix(PREFIX).unwrap_or_default() {
        "get_bot_status" => bot_status::handle(&state),
        // Go's `default` (`webserver.go:76-77`), which is also where the 21
        // deferred routes land for now.
        _ => reply::not_found(),
    };
    reply::allow_cors(&mut response);
    response
}
