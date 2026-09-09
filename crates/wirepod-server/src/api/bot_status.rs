//! `/api/get_bot_status`: the jdocs pinger's view of every known robot.

use axum::response::Response;
use wirepod_core::AppState;

use crate::reply;

/// Answers one element per robot in the bot-info file, in file order.
///
/// ```text
/// func handleGetBotStatus(w http.ResponseWriter) {
///     w.Header().Set("Content-Type", "application/json")
///     json.NewEncoder(w).Encode(sdkapp.GetConnectionStatus())
/// }
/// ```
///
/// `webserver.go:306-309`. The handler takes only the response writer, so no
/// parameter of any kind is read, not even from the query string, and it makes
/// no RPC. `json.Encoder.Encode` appends a newline, so the body always ends
/// with one. An empty list is `[]` and never `null`, which is the whole of a
/// deliberate Go change (`jdocspinger.go:42-45`).
///
/// The `esn` is emitted in the case the bot-info file stores, never normalised.
/// The dashboard matches it against the raw `?serial=` value from its own URL
/// with `===` (`vectorbrain.js:262`), so a normalised spelling would silently
/// kill the status card and the camera retry loop while leaving the camera's
/// first attempt working.
pub fn handle(state: &AppState) -> Response {
    let statuses =
        state.with_bot_info(|info| state.pinger().snapshot(info, state.clock().as_ref()));
    let body = match serde_json::to_string(&statuses) {
        Ok(mut body) => {
            body.push('\n');
            body
        }
        // Unreachable: the projection is four owned scalars per robot. Go
        // discards this error too, and because it sets the content type
        // before it encodes (`webserver.go:307`), the failure is an
        // `application/json` response with an empty body.
        Err(_) => String::new(),
    };
    reply::json(body)
}
