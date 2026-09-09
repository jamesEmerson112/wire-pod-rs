//! Every byte-exact body and header value, asserted verbatim.
//!
//! One test per constant, so a failure names the constant that moved. The
//! assertions restate the literal rather than deriving it, which is the point:
//! these are quoted from the Go server and from the live responses, and the
//! vendored web UI compares several of them as strings.
//!
//! The newline discipline is the thing most easily lost. Go's `fmt.Fprint`
//! writes no newline, while `http.Error` and `json.Encoder.Encode` both append
//! one, so half of these bodies end in `\n` and half do not.

use wirepod_server::literals;

#[test]
fn success_is_the_conn_test_body() {
    // `fmt.Fprint(w, "success")`, `server.go:73`.
    assert_eq!(literals::SUCCESS, "success");
}

#[test]
fn done_is_the_begin_cam_stream_body() {
    // `fmt.Fprint(w, "done")`, `server.go:519`.
    assert_eq!(literals::DONE, "done");
}

#[test]
fn the_error_prefix_is_seven_characters_with_a_trailing_space() {
    // `fmt.Fprint(w, "error: "+err.Error())`, `server.go:61`.
    assert_eq!(literals::ERROR_PREFIX, "error: ");
}

#[test]
fn the_robot_not_found_body_carries_the_prefix_twice() {
    // `robot.go:349` supplies one `error: ` and `server.go:61` prepends
    // another. No newline: it is written with `fmt.Fprint`.
    assert_eq!(
        literals::ROBOT_NOT_FOUND,
        "error: error: robot not found in SDK info file"
    );
}

#[test]
fn the_dispatch_default_body_is_not_found_with_a_newline() {
    // `http.Error(w, "not found", http.StatusNotFound)` supplies the newline,
    // `server.go:69` and `webserver.go:77`.
    assert_eq!(literals::NOT_FOUND, "not found\n");
}

#[test]
fn the_no_bots_body_has_a_newline() {
    // `http.Error(w, "no bots are authenticated", 500)`, `server.go:187`.
    assert_eq!(
        literals::NO_BOTS_AUTHENTICATED,
        "no bots are authenticated\n"
    );
}

#[test]
fn the_marshal_failure_body_has_no_newline() {
    // `fmt.Fprintf(w, "error marshaling json")`, `server.go:192`.
    assert_eq!(literals::ERROR_MARSHALING_JSON, "error marshaling json");
}

#[test]
fn the_conn_check_body_is_two_characters() {
    // `fmt.Fprintf(w, "ok")`, `jdocspinger.go:218`. The robot polls this every
    // few seconds for the life of the connection.
    assert_eq!(literals::OK, "ok");
}

#[test]
fn the_mdns_body_is_three_characters() {
    // `fmt.Fprintf(w, "ran")`, `jdocspinger.go:200`.
    assert_eq!(literals::MDNS_RAN, "ran");
}

#[test]
fn the_file_server_404_body_is_gos_own() {
    // `net/http`'s `serveError` writes this through `http.Error`, so it too
    // carries a newline. Nineteen bytes, which is the `Content-Length: 19` the
    // live server answers with.
    assert_eq!(literals::FILE_NOT_FOUND, "404 page not found\n");
    assert_eq!(literals::FILE_NOT_FOUND.len(), 19);
}

#[test]
fn the_empty_bot_status_body_is_a_bracket_pair_and_a_newline() {
    // Never `null`: Go starts from `[]BotStatus{}` on purpose
    // (`jdocspinger.go:42-45`) and `Encode` appends the newline.
    assert_eq!(literals::EMPTY_BOT_STATUS, "[]\n");
}

#[test]
fn the_redirect_link_text_is_gos_status_text() {
    // `http.StatusText(http.StatusMovedPermanently)`.
    assert_eq!(literals::MOVED_PERMANENTLY, "Moved Permanently");
}

#[test]
fn the_sniffed_text_content_type_is_gos() {
    assert_eq!(literals::CONTENT_TYPE_TEXT, "text/plain; charset=utf-8");
}

#[test]
fn the_json_content_type_has_no_charset() {
    // `w.Header().Set("Content-Type", "application/json")`, `webserver.go:307`.
    assert_eq!(literals::CONTENT_TYPE_JSON, "application/json");
}

#[test]
fn the_redirect_content_type_is_html_with_a_charset() {
    assert_eq!(literals::CONTENT_TYPE_HTML, "text/html; charset=utf-8");
}

#[test]
fn the_nosniff_value_is_lowercase() {
    assert_eq!(literals::NOSNIFF, "nosniff");
}

#[test]
fn the_pragma_value_is_no_cache() {
    // `w.Header().Set("pragma", "no-cache")`, `webserver.go:418`.
    assert_eq!(literals::NO_CACHE, "no-cache");
}

#[test]
fn the_expires_value_is_a_bare_zero() {
    // `w.Header().Set("Expires", "0")`, `webserver.go:420`.
    assert_eq!(literals::EXPIRES_ZERO, "0");
}

#[test]
fn the_cors_value_is_a_bare_star() {
    // Both `Access-Control-Allow-Origin` and `Access-Control-Allow-Headers`,
    // `webserver.go:28-29`.
    assert_eq!(literals::CORS_ANY, "*");
}

#[test]
fn the_newline_split_is_exactly_where_go_puts_it() {
    // Written with `fmt.Fprint`, so no newline.
    for body in [
        literals::SUCCESS,
        literals::DONE,
        literals::ROBOT_NOT_FOUND,
        literals::ERROR_MARSHALING_JSON,
        literals::OK,
        literals::MDNS_RAN,
    ] {
        assert!(
            !body.ends_with('\n'),
            "{body:?} must not end with a newline"
        );
    }

    // Written with `http.Error` or `json.Encoder.Encode`, so exactly one.
    for body in [
        literals::NOT_FOUND,
        literals::NO_BOTS_AUTHENTICATED,
        literals::FILE_NOT_FOUND,
        literals::EMPTY_BOT_STATUS,
    ] {
        assert!(body.ends_with('\n'), "{body:?} must end with a newline");
        assert!(
            !body.ends_with("\n\n"),
            "{body:?} must end with exactly one newline"
        );
    }
}
