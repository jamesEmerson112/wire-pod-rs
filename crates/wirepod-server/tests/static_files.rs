//! The three static mounts: the web root, `/sdk-app` and `/session-certs/`.
//!
//! Every file these tests serve is written into this test binary's own
//! temporary directory, so nothing here reaches the live `%APPDATA%\wire-pod`
//! or the installed web root.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use http::{Method, StatusCode, header};
use wirepod_core::paths::{AssetDir, DataDir};
use wirepod_core::test_support::FakeConnFactory;
use wirepod_core::{AppState, Paths, RobotConnFactory};
use wirepod_server::test_support::{Reply, request, send_to, unreachable_error};
use wirepod_server::{build_router, literals};

/// The four headers `config-ws`'s `DisableCachingAndSniffing` sets
/// (`webserver.go:415-421`).
const WEBROOT_CACHE_CONTROL: &str = "no-cache, no-store, must-revalidate, max-age=0";

/// The one `sdkapp`'s copy sets, semicolon and all (`server.go:793`).
const SDK_APP_CACHE_CONTROL: &str = "no-cache, no-store, must-revalidate;";

/// Sends one GET through the router under test.
async fn get(router: &Router, uri: &str) -> Reply {
    send_to(router, request(Method::GET, uri, None)).await
}

/// A router whose asset and data directories are `label`'s own, with a web root
/// holding a page, a stylesheet and a subdirectory.
fn temp_router(label: &str, with_sdk_app_file: bool) -> Router {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(label);
    let _ = fs::remove_dir_all(&root);
    let webroot = root.join("webroot");
    fs::create_dir_all(webroot.join("css")).expect("create the css directory");
    fs::create_dir_all(webroot.join("sub")).expect("create the subdirectory");
    fs::create_dir_all(webroot.join("sdkapp")).expect("create the sdk-app directory");
    fs::create_dir_all(root.join("session-certs")).expect("create the session-certs directory");
    fs::write(webroot.join("index.html"), "<h1>wire-pod</h1>").expect("write the index");
    fs::write(webroot.join("css").join("style.css"), "body{color:red}").expect("write the css");
    fs::write(webroot.join("sub").join("index.html"), "<h1>sub</h1>").expect("write the sub index");
    if with_sdk_app_file {
        // Go strips no prefix, so the `/sdk-app` mount looks for a file of that
        // name inside `./webroot/sdkapp` (`server.go:810-811`).
        fs::write(webroot.join("sdkapp").join("sdk-app"), "app").expect("write the sdk-app file");
    }
    fs::write(
        root.join("session-certs").join("00303f28"),
        "-----BEGIN-----\n",
    )
    .expect("write the session certificate");

    let factory: Arc<dyn RobotConnFactory> =
        Arc::new(FakeConnFactory::failing(unreachable_error()));
    let state = AppState::builder(factory)
        .paths(Paths::new(DataDir::rooted(&root), AssetDir::new(&root)))
        .build();
    build_router(state)
}

#[tokio::test]
async fn the_web_root_serves_files_with_four_cache_headers_and_404s_with_three() {
    let router = temp_router("static-webroot", false);

    // A directory serves its `index.html`, and the content type comes from the
    // extension.
    let reply = get(&router, "/").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, "<h1>wire-pod</h1>");
    assert!(
        reply
            .content_type()
            .is_some_and(|value| value.starts_with("text/html")),
        "{:?}",
        reply.content_type()
    );
    assert_eq!(
        reply.header(header::CACHE_CONTROL),
        Some(WEBROOT_CACHE_CONTROL)
    );
    assert_eq!(reply.header(header::PRAGMA), Some(literals::NO_CACHE));
    assert_eq!(reply.header(header::EXPIRES), Some(literals::EXPIRES_ZERO));
    assert_eq!(
        reply.header(header::X_CONTENT_TYPE_OPTIONS),
        Some(literals::NOSNIFF)
    );

    let reply = get(&router, "/css/style.css").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, "body{color:red}");
    assert!(
        reply
            .content_type()
            .is_some_and(|value| value.starts_with("text/css")),
        "{:?}",
        reply.content_type()
    );
    assert_eq!(
        reply.header(header::CACHE_CONTROL),
        Some(WEBROOT_CACHE_CONTROL)
    );

    // A directory without its trailing slash is a 301 to the slashed form,
    // which is Go's `localRedirect` (`net/http/fs.go`).
    let reply = get(&router, "/sub?a=1").await;
    assert_eq!(reply.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(reply.header(header::LOCATION), Some("/sub/?a=1"));

    // The 404 keeps `Pragma` and `Expires` and loses `Cache-Control`, because
    // `serveError` deletes that one before calling `http.Error`.
    let reply = get(&router, "/no-such-file.txt").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::FILE_NOT_FOUND);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    assert_eq!(
        reply.header(header::X_CONTENT_TYPE_OPTIONS),
        Some(literals::NOSNIFF)
    );
    assert_eq!(reply.header(header::PRAGMA), Some(literals::NO_CACHE));
    assert_eq!(reply.header(header::EXPIRES), Some(literals::EXPIRES_ZERO));
    assert_eq!(reply.header(header::CACHE_CONTROL), None);
}

#[tokio::test]
async fn the_sdk_app_mount_carries_three_headers_and_the_certs_route_reads_by_esn() {
    let router = temp_router("static-sdk-app", true);

    let reply = get(&router, "/sdk-app").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, "app");
    assert_eq!(
        reply.header(header::CACHE_CONTROL),
        Some(SDK_APP_CACHE_CONTROL)
    );
    assert_eq!(reply.header(header::PRAGMA), Some(literals::NO_CACHE));
    assert_eq!(
        reply.header(header::X_CONTENT_TYPE_OPTIONS),
        Some(literals::NOSNIFF)
    );
    // The `sdkapp` middleware sets no `Expires` at all, which is the header
    // that tells the two mounts apart on the wire.
    assert_eq!(reply.header(header::EXPIRES), None);

    // With no file behind it the mount answers the same body with `Pragma` and
    // no `Expires`, which is what the live Go server answers for `/sdk-app`.
    let empty = temp_router("static-sdk-app-empty", false);
    let reply = get(&empty, "/sdk-app").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::FILE_NOT_FOUND);
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_TEXT));
    assert_eq!(reply.header(header::PRAGMA), Some(literals::NO_CACHE));
    assert_eq!(reply.header(header::EXPIRES), None);
    assert_eq!(reply.header(header::CACHE_CONTROL), None);

    // `certHandler` splits the path and reads the third segment as the ESN
    // (`webserver.go:502-518`).
    let reply = get(&router, "/session-certs/00303f28").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, "-----BEGIN-----\n");

    let reply = get(&router, "/session-certs/deadbeef").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, "cert does not exist\n");
}
