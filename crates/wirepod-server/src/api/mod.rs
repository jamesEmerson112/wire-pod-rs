//! `/api/*`: the web UI's own API, with CORS on every response.
//!
//! Go registers this prefix with a trailing slash (`webserver.go:428`) and
//! dispatches on `strings.TrimPrefix(r.URL.Path, "/api/")` in an exact-string
//! switch (`webserver.go:31-78`). The two CORS headers are the first two
//! statements of the handler, so they are on every response including the
//! `default` case's 404.

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
use http::{HeaderValue, StatusCode, header};
use serde::Serialize;
use wirepod_core::AppState;
use wirepod_core::gojson::go_marshal;

use crate::{literals, reply};

/// The subtree prefix, registered with its trailing slash
/// (`webserver.go:428`).
pub const PREFIX: &str = "/api/";

/// The one handler behind `/api/` and everything under it.
pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let route = req
        .uri()
        .path()
        .strip_prefix(PREFIX)
        .unwrap_or_default()
        .to_owned();
    let mut response = match route.as_str() {
        "add_custom_intent" => intents::add_custom_intent(&state, &body(req).await),
        "edit_custom_intent" => intents::edit_custom_intent(&state, &body(req).await),
        "get_custom_intents_json" => intents::get_custom_intents_json(&state),
        "remove_custom_intent" => intents::remove_custom_intent(&state, &body(req).await),
        "set_weather_api" => weather::set_weather_api(&state, &body(req).await).await,
        "get_weather_api" => weather::get_weather_api(&state),
        "get_bot_status" => bot_status::handle(&state),
        // Go's `default` (`webserver.go:76-77`).
        _ => reply::not_found(),
    };
    reply::allow_cors(&mut response);
    response
}

/// Go's `json.NewDecoder(r.Body)`: the whole body, with a body that cannot be
/// read seen as no bytes, which is the decode error Go reports.
async fn body(req: Request) -> Vec<u8> {
    match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(bytes) => bytes.to_vec(),
        Err(err) => {
            tracing::debug!(comp = "", "{err}");
            Vec::new()
        }
    }
}

/// Go's `http.Error` with a body only known at run time, which
/// [`reply::go_error`] cannot take. The caller supplies the newline
/// `http.Error` appends.
pub fn error_text(status: StatusCode, body: impl Into<String>) -> Response {
    let mut response = reply::text(body.into());
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static(literals::NOSNIFF),
    );
    response
}

/// The bare `text/plain` these handlers set explicitly (`webserver.go:262`,
/// `:280`, `:285`, `:312`), which is not the sniffed
/// `text/plain; charset=utf-8`.
pub fn plain(body: impl Into<String>) -> Response {
    let mut response = reply::text(body.into());
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    response
}

/// `json.NewEncoder(w).Encode(v)`: Go's marshal plus the newline `Encode`
/// appends, under `application/json`.
///
/// Go sets the content type before it encodes, so a marshal failure leaves an
/// `application/json` response with an empty body.
pub fn encode_json<T: Serialize>(value: &T) -> Response {
    let body = match go_marshal(value) {
        Ok(mut bytes) => {
            bytes.push(b'\n');
            String::from_utf8_lossy(&bytes).into_owned()
        }
        Err(_) => String::new(),
    };
    reply::json(body)
}

/// A state whose files all live under `dir`.
#[cfg(test)]
pub(crate) fn test_state(dir: &std::path::Path) -> Arc<AppState> {
    use wirepod_core::paths::{AssetDir, DataDir};
    use wirepod_core::test_support::FakeConnFactory;
    use wirepod_core::{ConnError, Paths, RobotConnFactory, StatusCode as ConnCode};

    let factory: Arc<dyn RobotConnFactory> = Arc::new(FakeConnFactory::failing(ConnError::new(
        ConnCode::Unavailable,
        "no robot in this test",
    )));
    AppState::builder(factory)
        .paths(Paths::new(DataDir::rooted(dir), AssetDir::new(dir)))
        .build()
}

/// One response's body, as text.
#[cfg(test)]
pub(crate) async fn body_of(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("collect the response body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A fresh empty directory under the system temporary directory.
#[cfg(test)]
pub(crate) fn test_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wirepod-api-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test directory");
    dir
}
