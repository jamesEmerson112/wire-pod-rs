//! `SSHSetup` and `RegisterSSHAPI` of `pkg/wirepod/setup/ssh.go`.
//!
//! The two handler bodies are in `wirepod-setup`, which knows nothing about
//! axum; what is left here is Go's `r.FormValue` and `r.FormFile`, and the
//! subtree registration `StartWebServer` makes (`webserver.go:426`).
//!
//! `/api-ssh/setup` is the one route on this surface whose body is a multipart
//! form: the dashboard sends the private key as a file part beside `ip`
//! (`webroot/js/ssh.js:14-20`). [`crate::form`] deliberately does not merge
//! multipart bodies, so this reads its own.

use std::sync::Arc;

// `Multipart` is an extractor, and taking a whole `Request` is what lets the
// dispatch below read the path before the body.
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::response::Response;
use wirepod_core::AppState;

use crate::reply;

/// The subtree prefix, registered with its trailing slash (`ssh.go:253`).
pub const PREFIX: &str = "/api-ssh/";

const SETUP: &str = "/api-ssh/setup";
const GET_SETUP_STATUS: &str = "/api-ssh/get_setup_status";

/// Go's `SSHSetup` (`ssh.go:222`). Its switch matches nothing else, and the
/// handler then writes no body.
pub async fn ssh_setup(State(state): State<Arc<AppState>>, req: Request) -> Response {
    match req.uri().path() {
        SETUP => {
            let query = req.uri().query().map(str::to_owned);
            let (ip, key) = read_setup_form(req).await;
            // Go's `FormValue` reads the multipart form first and the query
            // after it, so a part named `ip` wins and a query one still counts.
            let ip = if ip.is_empty() {
                crate::form::Form::merge(query.as_deref(), None)
                    .get("ip")
                    .to_owned()
            } else {
                ip
            };
            reply::text(wirepod_setup::ssh::ssh_setup(
                state.paths().clone(),
                state.config().server.clone(),
                &ip,
                &key,
            ))
        }
        GET_SETUP_STATUS => reply::text(wirepod_setup::ssh::get_setup_status()),
        _ => reply::empty(),
    }
}

/// `r.FormValue("ip")` and `r.FormFile("key")` out of one multipart body.
///
/// A body that is not multipart, or one that cannot be read, gives both fields
/// as empty, which is the same state Go reaches when `ParseMultipartForm`
/// fails: `ip` is empty and the handler answers "must provide ip".
async fn read_setup_form(req: Request) -> (String, Vec<u8>) {
    let (mut ip, mut key) = (String::new(), Vec::new());
    let mut multipart = match Multipart::from_request(req, &()).await {
        Ok(multipart) => multipart,
        Err(err) => {
            tracing::debug!(comp = "", "{err}");
            return (ip, key);
        }
    };
    while let Ok(Some(field)) = multipart.next_field().await {
        match field.name() {
            // Go's `FormValue` answers with the first value under a key.
            Some("ip") if ip.is_empty() => ip = field.text().await.unwrap_or_default(),
            Some("key") if key.is_empty() => {
                key = field.bytes().await.unwrap_or_default().to_vec();
            }
            _ => {}
        }
    }
    (ip, key)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use http::{Method, StatusCode, header};

    use super::*;
    use crate::api::{body_of, test_dir, test_state};

    const BOUNDARY: &str = "X-BOUNDARY";

    fn multipart_body(parts: &[(&str, Option<&str>, &str)]) -> String {
        let mut body = String::new();
        for (name, filename, value) in parts {
            body.push_str(&format!("--{BOUNDARY}\r\n"));
            body.push_str(&format!("Content-Disposition: form-data; name=\"{name}\""));
            if let Some(filename) = filename {
                body.push_str(&format!("; filename=\"{filename}\""));
            }
            body.push_str("\r\n\r\n");
            body.push_str(value);
            body.push_str("\r\n");
        }
        body.push_str(&format!("--{BOUNDARY}--\r\n"));
        body
    }

    fn post(path: &str, body: String) -> Request {
        Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .body(Body::from(body))
            .expect("build the request")
    }

    #[tokio::test]
    async fn a_setup_without_an_ip_or_a_key_answers_before_dialling_anything() {
        let dir = test_dir("ssh_api");
        let state = test_state(&dir);

        let no_ip = ssh_setup(
            State(Arc::clone(&state)),
            post(
                SETUP,
                multipart_body(&[("key", Some("id_rsa"), "not-a-key")]),
            ),
        )
        .await;
        assert_eq!(body_of(no_ip).await, "error: must provide ip");

        // An ip and a key too short to be one: Go's second guard, which is the
        // last point before it would dial.
        let short_key = ssh_setup(
            State(Arc::clone(&state)),
            post(
                SETUP,
                multipart_body(&[("ip", None, "192.0.2.1"), ("key", Some("id_rsa"), "abc")]),
            ),
        )
        .await;
        assert_eq!(body_of(short_key).await, "error: must provide ssh key ()");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_status_route_reports_not_running_and_an_unknown_path_is_empty() {
        let dir = test_dir("ssh_api_status");
        let state = test_state(&dir);

        let status = ssh_setup(
            State(Arc::clone(&state)),
            Request::builder()
                .uri(GET_SETUP_STATUS)
                .body(Body::empty())
                .expect("build the request"),
        )
        .await;
        assert_eq!(status.status(), StatusCode::OK);
        assert_eq!(body_of(status).await, "not running");

        let unknown = ssh_setup(
            State(state),
            Request::builder()
                .uri("/api-ssh/nothing")
                .body(Body::empty())
                .expect("build the request"),
        )
        .await;
        assert_eq!(body_of(unknown).await, "");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
