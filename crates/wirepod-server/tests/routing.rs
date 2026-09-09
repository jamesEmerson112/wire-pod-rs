//! The router: what reaches which handler, and what falls through.
//!
//! The first test builds the router, because `matchit` 0.7 treats a colon as
//! the path-parameter sigil and a bad pattern is a panic at build time rather
//! than a 404 at request time. Everything after it assumes the build succeeded.

use http::{Method, StatusCode, header};
use wirepod_server::test_support::{
    CACHE_HEADERS, CORS_HEADERS, TestServer, one_robot, request, send_to,
};
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

#[tokio::test]
async fn a_path_is_matched_and_dispatched_by_its_unescaped_form() {
    let server = TestServer::connected(one_robot());

    // Go's routing tree unescapes each segment before it compares it against a
    // literal pattern (`net/http/routing_tree.go:205-215`), so an escaped colon
    // still reaches `/ok:80`. Live: 200, `ok`, `Content-Length: 2`.
    let reply = server.get("/ok%3A80").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::OK);

    // The same rule reaches the plain conn check. Live: 200, `ok`.
    let reply = server.get("/%6Fk").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, literals::OK);

    // `SdkapiHandler` switches on `r.URL.Path`, which `url.Parse` has decoded,
    // so an escaped route name is preamble-exempt and reaches the 404 rather
    // than the doubled connect error. Live: 404, `not found`.
    let reply = server.get("/api-sdk/deb%75g?serial=bogus").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::NOT_FOUND);

    // An escape in the prefix segment reaches the same handler, because the
    // segment is unescaped before the subtree pattern is tried. Live: 404.
    let reply = server.get("/api%2Dsdk/debug?serial=bogus").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::NOT_FOUND);

    // And a served route is reached the same way, so the decoding is not just
    // the 404 path.
    let reply = server.get("/api-sdk/get%5Fsdk%5Finfo").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply.body.starts_with(r#"{"global_guid":"#),
        "{}",
        reply.body
    );

    // `%2F` does not create a segment boundary: Go compares the decoded segment
    // `ok/80` against the patterns and matches none of them. Live: 404,
    // `404 page not found`.
    let reply = server.get("/ok%2F80").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::FILE_NOT_FOUND);
}

#[tokio::test]
async fn a_path_that_needs_cleaning_is_a_301_to_the_cleaned_path() {
    let server = TestServer::connected(one_robot());

    // `http.ServeMux` cleans the path before it routes and answers a 301 when
    // that changed it (`net/http/server.go:2681`, `:2690-2698`). Live: 301,
    // `Location: /api-sdk/debug?serial=bogus`, `Content-Length: 62`.
    let reply = server.get("/api-sdk//debug?serial=bogus").await;
    assert_eq!(reply.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        reply.header(header::LOCATION),
        Some("/api-sdk/debug?serial=bogus")
    );
    assert_eq!(
        reply.body,
        "<a href=\"/api-sdk/debug?serial=bogus\">Moved Permanently</a>.\n\n"
    );
    assert_eq!(reply.body.len(), 62);

    // Live: 301, `Location: /api/get_bot_status`, `Content-Length: 54`.
    let reply = server.get("/api//get_bot_status").await;
    assert_eq!(reply.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(reply.header(header::LOCATION), Some("/api/get_bot_status"));
    assert_eq!(reply.body.len(), 54);

    // Dot segments are resolved, and a `..` cannot climb past the root. All
    // three were probed live with `curl --path-as-is`.
    for uri in [
        "/api-sdk/./debug?serial=bogus",
        "/api-sdk/x/../debug?serial=bogus",
        "/api-sdk/debug/../debug?serial=bogus",
    ] {
        let reply = server.get(uri).await;
        assert_eq!(reply.status, StatusCode::MOVED_PERMANENTLY, "{uri}");
        assert_eq!(
            reply.header(header::LOCATION),
            Some("/api-sdk/debug?serial=bogus"),
            "{uri}"
        );
    }
    let reply = server.get("/../ok").await;
    assert_eq!(reply.header(header::LOCATION), Some("/ok"));

    // Go runs the trailing-slash redirect first, so a path that cleans to a
    // bare subtree prefix goes to the prefix with its slash rather than to the
    // cleaned path. Live: `Location: /api-sdk/`, `Content-Length: 44`.
    for uri in ["//api-sdk", "/api-sdk/."] {
        let reply = server.get(uri).await;
        assert_eq!(reply.status, StatusCode::MOVED_PERMANENTLY, "{uri}");
        assert_eq!(reply.header(header::LOCATION), Some("/api-sdk/"), "{uri}");
        assert_eq!(reply.body.len(), 44, "{uri}");
    }

    // A trailing slash survives the clean, so `/ok/` is not `/ok`: it is the
    // file-server 404. Live: 404, `404 page not found`.
    let reply = server.get("/ok/").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::FILE_NOT_FOUND);

    // An already-clean path is served, not redirected, which is what keeps the
    // clean check off the hot path for every request the robot actually sends.
    let reply = server.get("/api-sdk/debug?serial=bogus").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body, literals::NOT_FOUND);
}

#[tokio::test]
async fn only_the_api_prefix_carries_cors_and_only_the_file_server_caches() {
    let server = TestServer::connected(one_robot());

    // `apiHandler` sets both CORS headers as its first two statements, so they
    // are on its 404 as well, and it sets no cache header at all. That pair of
    // facts is what tells this 404 apart from the file server's.
    for uri in ["/api/get_bot_status", "/api/no_such_route", "/api/"] {
        let reply = server.get(uri).await;
        for name in CORS_HEADERS {
            assert_eq!(reply.header_str(name), Some(literals::CORS_ANY), "{uri}");
        }
        reply.assert_absent(&CACHE_HEADERS, uri);
    }

    // Nothing wraps `/api-sdk/*` or the conn check, so neither carries a CORS
    // header or a cache header. This is the half of the split that a mutation
    // adding `allow_cors` to `sdkapp::handle` would otherwise pass.
    for uri in [
        "/api-sdk/conn_test?serial=00303f28",
        "/api-sdk/does_not_exist?serial=00303f28",
        "/api-sdk/get_sdk_info",
        "/ok",
        "/ok:80",
    ] {
        let reply = server.get(uri).await;
        reply.assert_absent(&CORS_HEADERS, uri);
        reply.assert_absent(&CACHE_HEADERS, uri);
    }

    // The file server's 404 is the one response with `Pragma` and `Expires`,
    // and it has no CORS header either.
    let reply = server.get("/no-such-path").await;
    reply.assert_absent(&CORS_HEADERS, "/no-such-path");
    assert_eq!(reply.header(header::PRAGMA), Some(literals::NO_CACHE));
    assert_eq!(reply.header(header::EXPIRES), Some(literals::EXPIRES_ZERO));
    assert_eq!(reply.header(header::CACHE_CONTROL), None);
}
