//! The knowledge-graph provider routes of `pkg/wirepod/config-ws/webserver.go`.

use axum::response::Response;
use http::StatusCode;
use serde_json::value::RawValue;
use wirepod_core::gojson::{Faults, store_object};
use wirepod_core::{AppState, write_config_to_disk};

use crate::api::{encode_json, error_text};
use crate::reply;

/// Go decodes straight into `vars.APIConfig.Knowledge`, so a key the body
/// leaves out keeps the value the configuration already had, and a value of the
/// wrong type is a 400 that still leaves every key before it applied.
pub async fn set_kg_api(state: &AppState, body: &[u8]) -> Response {
    let Ok(raw) = serde_json::from_slice::<Box<RawValue>>(body) else {
        return error_text(StatusCode::BAD_REQUEST, "invalid request body\n");
    };
    let mut faults = Faults::default();
    state.update_config(|config| {
        let _ = store_object(&mut config.knowledge, &raw, "", "knowledge", &mut faults);
    });
    if let Some(fault) = faults.first {
        tracing::debug!(comp = "", "{fault}");
        return error_text(StatusCode::BAD_REQUEST, "invalid request body\n");
    }
    // Go discards this error.
    if let Err(err) = write_config_to_disk(&state.config(), state.config_gate()).await {
        tracing::debug!(comp = "", "{err}");
    }
    reply::text("Changes successfully applied.")
}

/// The key goes out in plain text, because the web UI reads it back into the
/// form.
pub fn get_kg_api(state: &AppState) -> Response {
    encode_json(&state.config().knowledge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{test_dir, test_state};

    #[tokio::test]
    async fn the_body_is_merged_onto_the_knowledge_the_config_already_holds() {
        let dir = test_dir("kg");
        let state = test_state(&dir);
        state.update_config(|config| config.knowledge.model = "kept".to_owned());

        let reply = set_kg_api(&state, br#"{"enable":true,"provider":"openai","key":"k"}"#).await;
        assert_eq!(reply.status(), StatusCode::OK);
        assert_eq!(state.config().knowledge.provider, "openai");
        assert_eq!(
            state.config().knowledge.model,
            "kept",
            "a key the body leaves out keeps its value"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_malformed_body_is_refused() {
        let dir = test_dir("kg-bad");
        let state = test_state(&dir);
        assert_eq!(
            set_kg_api(&state, b"not json").await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            set_kg_api(&state, br#"{"provider":5}"#).await.status(),
            StatusCode::BAD_REQUEST,
            "a value of the wrong type is Go's decode error"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
