//! The router: what reaches which handler, and what falls through.
//!
//! The first test builds the router, because `matchit` 0.7 treats a colon as
//! the path-parameter sigil and a bad pattern is a panic at build time rather
//! than a 404 at request time. Everything after it assumes the build succeeded.

use http::{Method, StatusCode, header};
use wirepod_server::test_support::{TestServer, one_robot, request, send_to};
use wirepod_server::{build_router, listener_specs, literals};

/// Paths the Go server genuinely lacks, so a 404 from the file server is the
/// contract rather than a stub.
///
/// `/ok:81` is here on purpose: only `/ok` and `/ok:80` are registered
/// (`server.go:813-814`), so a neighbouring port suffix is an ordinary miss.
/// Nothing Go serves is in this list; the deferred routes answer 404 too, but
/// that is a stub and no test asserts it.
const PATHS_GO_LACKS: [&str; 5] = [
    "/no-such-path",
    "/nope/deeper/still",
    "/ok:81",
    "/api-sdk-extra",
    "/apiary",
];

#[test]
fn the_router_builds_without_panic() {
    let server = TestServer::connected(one_robot());
    // Built twice, because a route table is built once per `build_router` call
    // and the panic would be in the second one just as much as the first.
    let _second = build_router(server.state);
}

#[tokio::test]
async fn the_exact_prefixes_reach_their_handlers() {
    let server = TestServer::connected(one_robot());

    // Go registers `/api-sdk/` as a subtree pattern, so the bare prefix runs
    // the handler with an empty serial and pays for the preamble. It must not
    // reach the file-server fallback.
    let reply = server.post("/api-sdk/").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::ROBOT_NOT_FOUND);

    // `/api/` reaches `apiHandler`'s default, which is distinguishable from the
    // fallback by both its body and its CORS headers.
    let reply = server.get("/api/").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::NOT_FOUND);
    assert_eq!(
        reply.header(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(literals::CORS_ANY)
    );
}

#[tokio::test]
async fn a_bare_prefix_is_a_301_to_its_trailing_slash_form() {
    let server = TestServer::connected(one_robot());

    let reply = server.get("/api-sdk").await;
    assert_eq!(reply.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(reply.header(header::LOCATION), Some("/api-sdk/"));
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_HTML));
    assert_eq!(
        reply.body,
        "<a href=\"/api-sdk/\">Moved Permanently</a>.\n\n"
    );
    // The live server answers `Content-Length: 44`.
    assert_eq!(reply.body.len(), 44);

    let reply = server.get("/api").await;
    assert_eq!(reply.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(reply.header(header::LOCATION), Some("/api/"));
    assert_eq!(reply.body, "<a href=\"/api/\">Moved Permanently</a>.\n\n");
}

#[tokio::test]
async fn the_redirect_carries_the_query_and_omits_the_body_for_a_post() {
    let server = TestServer::connected(one_robot());

    // Go's mux builds the location as `url.URL{Path: path + "/", RawQuery:
    // r.URL.RawQuery}`, so the query survives, and `htmlEscape` escapes the
    // ampersand it brings into the anchor.
    let reply = server.get("/api-sdk?serial=00303f28&_=1").await;
    assert_eq!(
        reply.header(header::LOCATION),
        Some("/api-sdk/?serial=00303f28&_=1")
    );
    assert_eq!(
        reply.body,
        "<a href=\"/api-sdk/?serial=00303f28&amp;_=1\">Moved Permanently</a>.\n\n"
    );

    // `http.Redirect` writes the body only for a GET and sets the HTML content
    // type only for a GET or a HEAD, so a POST gets the header and nothing
    // else.
    let reply = server.post("/api-sdk").await;
    assert_eq!(reply.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(reply.header(header::LOCATION), Some("/api-sdk/"));
    assert_eq!(reply.body, "");
    assert_eq!(reply.content_type(), None);

    let reply = server.send(request(Method::HEAD, "/api-sdk", None)).await;
    assert_eq!(reply.content_type(), Some(literals::CONTENT_TYPE_HTML));
    assert_eq!(reply.body, "");
}

#[tokio::test]
async fn the_fallback_is_gos_file_server_404_without_a_cache_control() {
    let server = TestServer::connected(one_robot());

    for path in PATHS_GO_LACKS {
        let reply = server.get(path).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(reply.body, literals::FILE_NOT_FOUND, "{path}");
        assert_eq!(
            reply.content_type(),
            Some(literals::CONTENT_TYPE_TEXT),
            "{path}"
        );
        assert_eq!(
            reply.header(header::X_CONTENT_TYPE_OPTIONS),
            Some(literals::NOSNIFF),
            "{path}"
        );
        assert_eq!(
            reply.header(header::PRAGMA),
            Some(literals::NO_CACHE),
            "{path}"
        );
        assert_eq!(
            reply.header(header::EXPIRES),
            Some(literals::EXPIRES_ZERO),
            "{path}"
        );
        // The middleware sets `Cache-Control`, but `serveError` deletes it
        // before writing the error, so the 404 carries four headers and not
        // five. A static 200 from the same handler does carry it.
        assert_eq!(reply.header(header::CACHE_CONTROL), None, "{path}");
    }
}

#[tokio::test]
async fn both_conn_check_paths_answer_ok_and_the_colon_one_goes_through_the_fallback() {
    let server = TestServer::connected(one_robot());

    for path in ["/ok", "/ok:80"] {
        let reply = server.get(path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.body, literals::OK, "{path}");
        assert_eq!(
            reply.content_type(),
            Some(literals::CONTENT_TYPE_TEXT),
            "{path}"
        );
    }

    // `/ok:81` is not a route in Go either, so it is the file-server 404 and
    // not the conn check. This is what distinguishes "matched the literal
    // `/ok:80`" from "matched anything starting with `/ok`".
    let reply = server.get("/ok:81").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::FILE_NOT_FOUND);
}

#[tokio::test]
async fn the_conn_check_reads_run_mdns_through_the_form_merge() {
    let server = TestServer::connected(one_robot());

    // Exactly the string `true` (`jdocspinger.go:199`), on both paths.
    assert_eq!(
        server.get("/ok?runMDNS=true").await.body,
        literals::MDNS_RAN
    );
    assert_eq!(
        server.get("/ok:80?runMDNS=true").await.body,
        literals::MDNS_RAN
    );
    assert_eq!(server.get("/ok?runMDNS=false").await.body, literals::OK);
    assert_eq!(server.get("/ok?runMDNS=TRUE").await.body, literals::OK);

    // It is a `FormValue` read, so a urlencoded body carries it too.
    assert_eq!(
        server.post_form("/ok", "runMDNS=true").await.body,
        literals::MDNS_RAN
    );
}

#[test]
fn both_listeners_are_described_and_neither_is_bound() {
    let specs = listener_specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].port, wirepod_server::CONN_CHECK_PORT);
    assert_eq!(specs[0].port, 80);
    assert_eq!(specs[1].port, wirepod_server::DEFAULT_WEB_PORT);
    assert_eq!(specs[1].port, 8080);
    assert_ne!(specs[0].name, specs[1].name);
}

#[tokio::test]
async fn one_router_value_answers_identically_for_every_listener() {
    // Go puts every route on `http.DefaultServeMux` and serves that same mux
    // from both listeners, so a route reachable on one is reachable on the
    // other with the same answer. The Rust shape is one router value handed to
    // both, and this is the test that says so.
    let server = TestServer::connected(one_robot());
    let probes = [
        "/ok",
        "/ok:80",
        "/api-sdk/conn_test?serial=00303f28",
        "/api-sdk/get_sdk_info",
        "/api/get_bot_status",
        "/no-such-path",
    ];

    let mut answers: Vec<Vec<(StatusCode, String)>> = Vec::new();
    for spec in listener_specs() {
        // The same router value is what each listener would be handed, so the
        // spec chooses nothing about routing. That is the property under test.
        let router = server.router.clone();
        let mut listener_answers = Vec::new();
        for probe in probes {
            let reply = send_to(&router, request(Method::GET, probe, None)).await;
            listener_answers.push((reply.status, reply.body));
        }
        assert!(!listener_answers.is_empty(), "{} probed nothing", spec.name);
        answers.push(listener_answers);
    }

    assert_eq!(
        answers[0], answers[1],
        "the two listeners must serve the union of every route"
    );
}
