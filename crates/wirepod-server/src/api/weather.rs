//! The weather-provider routes of `pkg/wirepod/config-ws/webserver.go`.

use axum::response::Response;
use http::StatusCode;
use serde::Deserialize;
use wirepod_core::{AppState, write_config_to_disk};

use crate::api::{encode_json, error_text};
use crate::reply;

#[derive(Deserialize)]
struct WeatherRequest {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    key: String,
}

pub async fn set_weather_api(state: &AppState, body: &[u8]) -> Response {
    let Ok(request) = serde_json::from_slice::<WeatherRequest>(body) else {
        return error_text(StatusCode::BAD_REQUEST, "invalid request body\n");
    };
    state.update_config(|config| {
        if request.provider.is_empty() {
            config.weather.enable = false;
        } else {
            config.weather.enable = true;
            config.weather.key = request.key.trim().to_owned();
            config.weather.provider = request.provider.clone();
        }
    });
    // Go discards this error.
    if let Err(err) = write_config_to_disk(&state.config(), state.config_gate()).await {
        tracing::debug!(comp = "", "{err}");
    }
    reply::text("Changes successfully applied.")
}

/// The key goes out in plain text, because the web UI reads it back into the
/// form.
pub fn get_weather_api(state: &AppState) -> Response {
    encode_json(&state.config().weather)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{test_dir, test_state};

    #[tokio::test]
    async fn a_provider_is_stored_and_read_back_and_an_empty_one_disables() {
        let dir = test_dir("weather");
        let state = test_state(&dir);

        let reply =
            set_weather_api(&state, br#"{"provider":"openweathermap","key":"  k  "}"#).await;
        assert_eq!(reply.status(), StatusCode::OK);
        assert!(state.config().weather.enable);
        assert_eq!(state.config().weather.key, "k", "the key is trimmed");

        let read_back = get_weather_api(&state);
        assert_eq!(
            read_back.headers().get(http::header::CONTENT_TYPE).unwrap(),
            "application/json"
        );

        set_weather_api(&state, br#"{"provider":""}"#).await;
        assert!(!state.config().weather.enable);
        // Disabling leaves the key and the provider where they were.
        assert_eq!(state.config().weather.provider, "openweathermap");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
