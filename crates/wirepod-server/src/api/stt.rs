//! The speech-to-text routes of `pkg/wirepod/config-ws/webserver.go`.

use std::sync::{Arc, LazyLock};

use axum::response::Response;
use http::StatusCode;
use serde::Deserialize;
use wirepod_core::intents::get_downloaded_vosk_models;
use wirepod_core::{AppState, write_config_to_disk};
use wirepod_intent::download::{DownloadStatus, download_vosk_model};
use wirepod_intent::localization::VALID_VOSK_MODELS;

use crate::api::{encode_json, error_text, plain};
use crate::reply;

// TODO(M3): var SttInitFunc func() error

/// Go's `localization.DownloadStatus` package global.
static DOWNLOAD_STATUS: LazyLock<DownloadStatus> = LazyLock::new(DownloadStatus::default);

#[derive(Deserialize)]
struct LanguageRequest {
    #[serde(default)]
    language: String,
}

pub async fn set_stt_info(state: &Arc<AppState>, body: &[u8]) -> Response {
    let Ok(request) = serde_json::from_slice::<LanguageRequest>(body) else {
        return error_text(StatusCode::BAD_REQUEST, "invalid request body\n");
    };
    let service = state.config().stt.provider.clone();
    if service == "vosk" {
        if !is_valid_language(&request.language, &VALID_VOSK_MODELS) {
            return error_text(StatusCode::BAD_REQUEST, "language not valid\n");
        }
        if !is_downloaded_language(
            &request.language,
            &get_downloaded_vosk_models(state.paths().data()),
        ) {
            let state = Arc::clone(state);
            let status = DOWNLOAD_STATUS.clone();
            let language = request.language.clone();
            tokio::spawn(async move { download_vosk_model(&state, &status, &language).await });
            return reply::text("downloading language model...");
        }
    } else if service == "whisper.cpp" {
        if !is_valid_language(&request.language, &VALID_VOSK_MODELS) {
            return error_text(StatusCode::BAD_REQUEST, "language not valid\n");
        }
    } else {
        return error_text(StatusCode::BAD_REQUEST, "service must be vosk or whisper\n");
    }
    state.update_config(|config| {
        config.stt.language = request.language.clone();
        config.past_initial_setup = true;
    });
    // Go discards this error.
    if let Err(err) = write_config_to_disk(&state.config(), state.config_gate()).await {
        tracing::debug!(comp = "", "{err}");
    }
    // TODO(M3): processreqs.ReloadVosk()
    tracing::debug!(comp = "", "Reloaded voice processor successfully");
    reply::text("Language switched successfully.")
}

/// The one route on this surface that mutates on read.
pub fn get_download_status() -> Response {
    let current = DOWNLOAD_STATUS.get();
    let response = plain(current.clone());
    if current == "success" || current.contains("error") {
        DOWNLOAD_STATUS.set("not downloading");
    }
    response
}

pub fn get_stt_info(state: &AppState) -> Response {
    encode_json(&state.config().stt)
}

fn is_valid_language(language: &str, valid_languages: &[&str]) -> bool {
    valid_languages.contains(&language)
}

fn is_downloaded_language(language: &str, downloaded_languages: &[String]) -> bool {
    downloaded_languages.iter().any(|lang| lang == language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{test_dir, test_state};

    #[tokio::test]
    async fn the_engine_decides_whether_a_language_is_accepted() {
        let dir = test_dir("stt");
        let state = test_state(&dir);

        // The zero configuration names no service at all.
        assert_eq!(
            set_stt_info(&state, br#"{"language":"en-US"}"#)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );

        state.update_config(|config| config.stt.provider = "whisper.cpp".to_owned());
        assert_eq!(
            set_stt_info(&state, br#"{"language":"xx-XX"}"#)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            set_stt_info(&state, br#"{"language":"it-IT"}"#)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(state.config().stt.language, "it-IT");
        assert!(state.config().past_initial_setup);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_download_is_reported_once_and_then_reset() {
        DOWNLOAD_STATUS.set("success");
        let reply = get_download_status();
        assert_eq!(
            reply.headers().get(http::header::CONTENT_TYPE).unwrap(),
            "text/plain"
        );
        assert_eq!(DOWNLOAD_STATUS.get(), "not downloading");
    }
}
