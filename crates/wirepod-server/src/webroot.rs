//! The static mounts: the web root, the `/sdk-app` file server and
//! `/session-certs/`.
//!
//! Go wraps each file server in a `DisableCachingAndSniffing`, and its two
//! copies of that middleware set different headers: `config-ws`'s sets four and
//! `sdkapp`'s sets three, with a trailing semicolon on its `Cache-Control`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use http::{HeaderValue, Method, StatusCode, header};
use tower::ServiceExt;
use tower_http::services::ServeDir;
use wirepod_core::AppState;

use crate::{literals, reply};

const WEBROOT_CACHE_CONTROL: &str = "no-cache, no-store, must-revalidate, max-age=0";

/// The trailing semicolon is Go's.
const SDK_APP_CACHE_CONTROL: &str = "no-cache, no-store, must-revalidate;";

/// The exact pattern the `sdkapp` file server is registered under.
///
/// Go strips no prefix, so the file server is handed `/sdk-app` and looks for a
/// file of that name inside `./webroot/sdkapp`. There is none, which is why the
/// live server answers this path with a 404.
pub const SDK_APP_PATH: &str = "/sdk-app";

/// The prefix `cert_handler` is registered under, and the substring it then
/// looks for.
pub const SESSION_CERTS_PREFIX: &str = "/session-certs/";

const MUST_REQUEST_BY_ESN: &str = "must request a cert by esn (ex. /session-certs/00e20145)\n";

const CERT_DOES_NOT_EXIST: &str = "cert does not exist\n";

/// Go's root file server, `http.Handle("/", DisableCachingAndSniffing(webRoot))`.
///
/// The 404 loses `Cache-Control` on the way out, because `net/http`'s
/// `serveError` deletes it before calling `http.Error`.
pub async fn web_root(state: &AppState, req: Request) -> Response {
    let mut response = serve_dir(state.paths().assets().webroot_dir(), req).await;
    if response.status() == StatusCode::NOT_FOUND {
        return reply::file_not_found();
    }
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(WEBROOT_CACHE_CONTROL),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static(literals::NO_CACHE));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static(literals::NOSNIFF),
    );
    headers.insert(
        header::EXPIRES,
        HeaderValue::from_static(literals::EXPIRES_ZERO),
    );
    response
}

/// Go's `http.Handle("/sdk-app", DisableCachingAndSniffing(fileServer))`.
pub async fn sdk_app(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let mut response = serve_dir(state.paths().assets().sdk_app_dir(), req).await;
    if response.status() == StatusCode::NOT_FOUND {
        // Three headers rather than four, so this 404 keeps `Pragma`, loses
        // `Cache-Control` and carries no `Expires` at all.
        response = reply::go_error(StatusCode::NOT_FOUND, literals::FILE_NOT_FOUND);
        response
            .headers_mut()
            .insert(header::PRAGMA, HeaderValue::from_static(literals::NO_CACHE));
        return response;
    }
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(SDK_APP_CACHE_CONTROL),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static(literals::NO_CACHE));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static(literals::NOSNIFF),
    );
    response
}

/// Go's `certHandler`. Its `switch` has no `default`, so a path without the
/// prefix writes nothing at all.
pub async fn cert_handler(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let path = req.uri().path();
    if !path.contains(SESSION_CERTS_PREFIX) {
        return reply::empty();
    }
    let split: Vec<&str> = path.split('/').collect();
    if split.len() < 3 {
        return reply::go_error(StatusCode::BAD_REQUEST, MUST_REQUEST_BY_ESN);
    }
    let esn = split[2];
    match tokio::fs::read(state.paths().data().session_cert_path(esn)).await {
        Ok(bytes) => {
            // Go writes the bytes and lets `net/http` sniff them; a PEM block
            // has no signature and is all printable, so the sniff lands here.
            let mut response = Response::new(Body::from(bytes));
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(literals::CONTENT_TYPE_TEXT),
            );
            response
        }
        Err(_) => reply::go_error(StatusCode::NOT_FOUND, CERT_DOES_NOT_EXIST),
    }
}

/// One `http.FileServer(http.Dir(dir))` call.
///
/// Two of `ServeDir`'s answers differ from Go's and are put back. Go's
/// `localRedirect` sends a 301 where `ServeDir` sends a 307, and Go's file
/// server never inspects the method where `ServeDir` answers 405 to anything
/// but a GET or a HEAD, so another method is handed on as a GET.
/// The content types Go's `mime.TypeByExtension` carries a charset on
/// (`mime/type.go`, `builtinTypesLower`). `mime_guess` writes the bare type, so
/// the charset is put back on the ones Go spells with it.
const GO_CHARSET_TYPES: &[&str] = &["text/css", "text/html", "text/javascript", "text/xml"];

/// Go's index redirect (`net/http/fs.go`): a path ending in `/index.html` is
/// answered with a 301 to its directory rather than with the file.
const INDEX_PAGE: &str = "/index.html";

async fn serve_dir(dir: PathBuf, req: Request) -> Response {
    let (mut parts, body) = req.into_parts();
    if parts.method != Method::GET && parts.method != Method::HEAD {
        parts.method = Method::GET;
    }
    if parts.uri.path().ends_with(INDEX_PAGE) {
        return local_redirect(parts.uri.query());
    }
    let req = Request::from_parts(parts, body);
    let response = match ServeDir::new(dir).oneshot(req).await {
        Ok(response) => response,
        Err(infallible) => match infallible {},
    };
    let mut response = response.map(Body::new);
    if response.status() == StatusCode::TEMPORARY_REDIRECT {
        *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
    }
    add_charset(&mut response);
    response
}

/// Go writes `text/css; charset=utf-8` where `mime_guess` writes `text/css`.
fn add_charset(response: &mut Response) {
    let Some(current) = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    if !GO_CHARSET_TYPES.contains(&current) {
        return;
    }
    if let Ok(value) = HeaderValue::from_str(&format!("{current}; charset=utf-8")) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
}

/// Go's `localRedirect` (`net/http/fs.go`): a 301 to `./` carrying the query
/// and nothing else, with no body and no content type.
fn local_redirect(query: Option<&str>) -> Response {
    let target = match query {
        Some(query) if !query.is_empty() => format!("./?{query}"),
        _ => "./".to_owned(),
    };
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
    if let Ok(value) = HeaderValue::from_str(&target) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}
