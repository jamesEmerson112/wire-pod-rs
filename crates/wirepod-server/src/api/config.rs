//! The configuration and liveness routes of
//! `pkg/wirepod/config-ws/webserver.go`.

use axum::response::Response;
use wirepod_core::AppState;

use crate::api::{encode_json, plain};
use crate::{literals, reply};

pub fn get_config(state: &AppState) -> Response {
    let config = state.config();
    encode_json(&*config)
}

/// The health probe the runbooks use.
pub fn is_running() -> Response {
    plain("true")
}

pub fn delete_chats() -> Response {
    // TODO(M4): vars.RememberedChats = []vars.RememberedChat{}
    reply::text(literals::DONE)
}

/// Go answers this one inline from the switch arm (`webserver.go:75`).
pub fn is_api_v3() -> Response {
    reply::text("it is!")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{body_of, test_dir, test_state};
    use http::header;

    #[tokio::test]
    async fn the_whole_config_goes_out_as_json_with_gos_trailing_newline() {
        let dir = test_dir("config");
        let state = test_state(&dir);
        state.update_config(|config| config.server.port = "443".to_owned());

        let reply = get_config(&state);
        assert_eq!(
            reply.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let body = body_of(reply).await;
        assert!(body.starts_with(r#"{"weather":{"#), "{body}");
        assert!(body.ends_with("}\n"), "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_liveness_probe_answers_true_under_bare_text_plain() {
        let reply = is_running();
        assert_eq!(
            reply.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain"
        );
        assert_eq!(body_of(reply).await, "true");
    }
}
