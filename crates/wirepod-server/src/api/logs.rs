//! The log routes of `pkg/wirepod/config-ws/webserver.go`.

use axum::response::Response;
use wirepod_core::AppState;
use wirepod_core::logger::LogLevel;

use crate::api::{encode_json, plain};
use crate::form::Form;

pub fn get_logs(state: &AppState) -> Response {
    plain(state.logs().info_text())
}

pub fn get_debug_logs(state: &AppState) -> Response {
    plain(state.logs().tray_text())
}

/// The one route on this surface that reads `r.URL.Query()` rather than
/// `r.FormValue`, so a body parameter is ignored.
pub fn get_logs_json(state: &AppState, query: Option<&str>) -> Response {
    let params = Form::merge(query, None);
    let lvl = match params.get("level") {
        "info" => LogLevel::Info,
        "warn" => LogLevel::Warn,
        "error" => LogLevel::Error,
        // Every other value, `debug` included, is Go's `default` arm.
        _ => LogLevel::Debug,
    };
    // Go discards the parse error, so a bad or absent value is zero.
    let since = params.get("since").parse::<i64>().unwrap_or(0);
    encode_json(&state.logs().get_entries(lvl, since))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{body_of, test_dir, test_state};

    #[tokio::test]
    async fn an_empty_ring_is_an_empty_array_and_never_null() {
        let dir = test_dir("logs");
        let state = test_state(&dir);
        let reply = get_logs_json(&state, Some("level=error&since=12"));
        assert_eq!(
            reply.headers().get(http::header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(body_of(reply).await, "[]\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_two_text_routes_answer_bare_text_plain() {
        let dir = test_dir("logs-text");
        let state = test_state(&dir);
        for reply in [get_logs(&state), get_debug_logs(&state)] {
            assert_eq!(
                reply.headers().get(http::header::CONTENT_TYPE).unwrap(),
                "text/plain"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
