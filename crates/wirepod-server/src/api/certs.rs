//! The certificate route of `pkg/wirepod/config-ws/webserver.go`.

use axum::response::Response;
use http::StatusCode;
use wirepod_core::AppState;

use crate::api::error_text;
use crate::{literals, reply};

pub async fn generate_certs(state: &AppState) -> Response {
    if let Err(err) = wirepod_setup::certs::create_cert_combo(state.paths().data()).await {
        return error_text(StatusCode::INTERNAL_SERVER_ERROR, format!("error: {err}\n"));
    }
    reply::text(literals::DONE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{body_of, test_dir, test_state};

    #[tokio::test]
    async fn the_web_ui_reads_the_body_as_the_word_done() {
        let dir = test_dir("generate_certs");
        let state = test_state(&dir);

        let reply = generate_certs(&state).await;
        assert_eq!(reply.status(), StatusCode::OK);
        assert_eq!(body_of(reply).await, "done");
        // The pair the robot is handed, written where the listener reads it.
        let data = state.paths().data();
        for path in [data.cert_path(), data.key_path()] {
            assert!(path.is_file(), "{} was not written", path.display());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
