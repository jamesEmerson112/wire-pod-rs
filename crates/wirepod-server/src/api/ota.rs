//! The OTA proxy route of `pkg/wirepod/config-ws/webserver.go`.

use axum::body::Body;
use axum::extract::Request;
use axum::response::Response;
use http::StatusCode;

use crate::api::error_text;

const ARCHIVE_PREFIX: &str = "https://archive.org/download/vector-pod-firmware/";

pub async fn get_ota(req: Request) -> Response {
    let (parts, _) = req.into_parts();
    // Go indexes the fourth path segment without checking it exists, and the
    // switch arm only ever matches the exact path `/api/get_ota`, which has
    // three. Go panics here; this answers the parse failure instead.
    let Some(ota_name) = parts.uri.path().split('/').nth(3) else {
        tracing::debug!(comp = "", "no OTA name in {}", parts.uri.path());
        return error_text(StatusCode::INTERNAL_SERVER_ERROR, "failed to parse URL\n");
    };
    let Some(target_url) = target_url(ota_name) else {
        return error_text(StatusCode::INTERNAL_SERVER_ERROR, "failed to parse URL\n");
    };
    let mut request = reqwest::Client::new().request(parts.method, target_url);
    for (key, value) in parts.headers.iter() {
        request = request.header(key, value);
    }
    let Ok(upstream) = request.send().await else {
        return error_text(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to perform request\n",
        );
    };
    // Go copies the upstream headers but never its status, so the answer is a
    // 200 whatever the upstream said.
    let mut response = Response::new(Body::empty());
    for (key, value) in upstream.headers().iter() {
        response.headers_mut().append(key, value.clone());
    }
    match upstream.bytes().await {
        Ok(body) => {
            *response.body_mut() = Body::from(body);
            response
        }
        Err(_) => error_text(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to copy response body\n",
        ),
    }
}

fn target_url(ota_name: &str) -> Option<reqwest::Url> {
    format!("{ARCHIVE_PREFIX}{}", ota_name.trim()).parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::body_of;

    #[test]
    fn the_name_is_trimmed_onto_the_archive_prefix() {
        assert_eq!(
            target_url("  1.8.0.ota ").unwrap().as_str(),
            "https://archive.org/download/vector-pod-firmware/1.8.0.ota"
        );
    }

    #[tokio::test]
    async fn the_only_path_the_switch_matches_has_no_ota_name() {
        let req = http::Request::builder()
            .uri("/api/get_ota")
            .body(Body::empty())
            .unwrap();
        let reply = get_ota(req).await;
        assert_eq!(reply.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body_of(reply).await, "failed to parse URL\n");
    }
}
