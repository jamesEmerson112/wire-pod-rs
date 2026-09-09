//! `/api-sdk/get_sdk_info`: the bot-info file, marshalled.

use axum::response::Response;
use wirepod_core::{AppState, BotInfoWire};

use crate::{literals, reply};

/// Answers the bot-info file as JSON, or 500 when no robot has authenticated.
///
/// ```text
/// if len(vars.BotInfo.Robots) == 0 {
///     http.Error(w, "no bots are authenticated", http.StatusInternalServerError)
///     return
/// }
/// jsonBytes, err := json.Marshal(vars.BotInfo)
/// ```
///
/// `server.go:185-196`. Three things are contract. The emptiness test is on
/// the robot list, not on the whole document, so a file holding only a global
/// GUID still answers 500. The key order is Go's declaration order,
/// `global_guid` then `robots`, and inside a robot `esn`, `ip_address`, `guid`,
/// `activated` (`vars.go:89-98`), with no `omitempty` anywhere. And the body has
/// no trailing newline, because `fmt.Fprint` writes the marshalled bytes as they
/// are.
///
/// [`BotInfoWire`] rather than the on-disk struct is what is serialised, because
/// the on-disk struct preserves unknown keys for rollback safety and those
/// extras would change this body. This route is the reason that projection
/// exists.
pub fn handle(state: &AppState) -> Response {
    state.with_bot_info(|info| {
        if info.robots.is_empty() {
            return reply::no_bots_authenticated();
        }
        match serde_json::to_string(&BotInfoWire::from(info)) {
            Ok(body) => reply::text(body),
            // Unreachable for this shape, as it is in Go. The arm exists so the
            // body is quoted from Go rather than invented if it ever is
            // reachable.
            Err(_) => reply::text(literals::ERROR_MARSHALING_JSON),
        }
    })
}
