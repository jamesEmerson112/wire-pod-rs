//! The response shapes Go produces, as helpers.
//!
//! Four of Go's writers appear on this surface and they differ in ways the
//! client can see. `fmt.Fprint` writes a body and lets `net/http` sniff its
//! content type, which for every text body here is
//! `text/plain; charset=utf-8`; that is exactly what axum's `String` responder
//! produces, so [`text`] is a thin wrapper. A handler that writes zero bytes
//! gets **no** `Content-Type` at all, which needs an empty [`Body`] rather than
//! an empty `String`, so [`empty`] exists for the deferred routes that do that.
//! `http.Error` sets the content type and `X-Content-Type-Options: nosniff` and
//! appends a newline. And the root file server's 404 carries four headers, of
//! which `Cache-Control` is conspicuously absent.

use std::fmt::Write as _;

use axum::body::Body;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, Method, StatusCode, header};

use crate::literals;

/// A sniffed `text/plain; charset=utf-8` body at HTTP 200, which is Go's
/// `fmt.Fprint` on this surface.
pub fn text(body: impl Into<String>) -> Response {
    body.into().into_response()
}

/// A zero-byte body at HTTP 200 with no `Content-Type` header.
///
/// Go sniffs a content type only when the handler writes something, so
/// `move_wheels`, `move_lift`, `move_head` and `play_sound` answer with no
/// content type at all. None of those four is in the slice; this exists so the
/// distinction is not discovered later by a parity diff, and so the deferred
/// routes have the right helper waiting for them.
pub fn empty() -> Response {
    Response::new(Body::empty())
}

/// An `application/json` body at HTTP 200, the content type
/// `/api/get_bot_status` sets explicitly (`webserver.go:307`).
pub fn json(body: impl Into<String>) -> Response {
    let mut response = text(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(literals::CONTENT_TYPE_JSON),
    );
    response
}

/// Go's `http.Error`: the body verbatim, `text/plain; charset=utf-8` and
/// `X-Content-Type-Options: nosniff`.
///
/// `body` carries its own trailing newline, because `http.Error` appends one
/// and the constants in [`literals`] are the finished bodies.
pub fn go_error(status: StatusCode, body: &'static str) -> Response {
    let mut response = text(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static(literals::NOSNIFF),
    );
    response
}

/// The 404 both dispatch defaults answer with (`server.go:69`,
/// `webserver.go:77`).
pub fn not_found() -> Response {
    go_error(StatusCode::NOT_FOUND, literals::NOT_FOUND)
}

/// `/api-sdk/get_sdk_info` with no authenticated robots (`server.go:187`).
pub fn no_bots_authenticated() -> Response {
    go_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        literals::NO_BOTS_AUTHENTICATED,
    )
}

/// The root file server's 404 (`webserver.go:439`), which is the router
/// fallback.
///
/// The header set is observed rather than inferred. `DisableCachingAndSniffing`
/// sets four headers (`webserver.go:415-421`), but `net/http`'s `serveError`
/// deletes `Cache-Control` before calling `http.Error`, so the 404 carries
/// `Content-Type`, `X-Content-Type-Options`, `Pragma` and `Expires` and **not**
/// `Cache-Control`, while a static 200 from the same handler does carry it.
pub fn file_not_found() -> Response {
    let mut response = go_error(StatusCode::NOT_FOUND, literals::FILE_NOT_FOUND);
    let headers = response.headers_mut();
    headers.insert(header::PRAGMA, HeaderValue::from_static(literals::NO_CACHE));
    headers.insert(
        header::EXPIRES,
        HeaderValue::from_static(literals::EXPIRES_ZERO),
    );
    response
}

/// Go's mux redirecting a bare subtree prefix to its trailing-slash form.
///
/// `http.ServeMux` answers `/api-sdk` with a 301 to `/api-sdk/`, carrying the
/// original query (`net/http/server.go`, `matchOrRedirect`). `http.Redirect`
/// then writes the HTML body only for a GET and sets the HTML content type for
/// a GET or a HEAD, so a POST to the bare prefix gets the `Location` header and
/// nothing else. That method dependence is `http.Redirect`'s, not a handler's,
/// so it does not contradict this surface's rule that no handler inspects the
/// method.
pub fn moved_permanently(path: &str, method: &Method, query: Option<&str>) -> Response {
    let location = match query {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    let wants_body = *method == Method::GET;
    let wants_content_type = wants_body || *method == Method::HEAD;

    let body = if wants_body {
        // `body := "<a href=\"" + htmlEscape(url) + "\">" + StatusText(code) +
        // "</a>.\n"` and then `fmt.Fprintln(w, body)`, which is the second
        // newline.
        format!(
            "<a href=\"{}\">{}</a>.\n\n",
            html_escape(&location),
            literals::MOVED_PERMANENTLY
        )
    } else {
        String::new()
    };

    let mut response = if wants_body { text(body) } else { empty() };
    *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
    let headers = response.headers_mut();
    // `http.Redirect` writes `hexEscapeNonASCII(url)` into the header and
    // `htmlEscape(url)` into the body, so the two differ for a non-ASCII byte.
    // After that escape the only bytes `from_str` still rejects are control
    // characters, which hyper refuses in a request target before a handler ever
    // runs, so the fallback is unreachable rather than a policy.
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&hex_escape_non_ascii(&location))
            .unwrap_or_else(|_| HeaderValue::from_static("/")),
    );
    if wants_content_type {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(literals::CONTENT_TYPE_HTML),
        );
    }
    response
}

/// The two CORS headers `apiHandler` sets before it dispatches
/// (`webserver.go:28-29`), so they are on every `/api/*` response including
/// its 404.
pub fn allow_cors(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static(literals::CORS_ANY),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(literals::CORS_ANY),
    );
}

/// Go's `hexEscapeNonASCII`, which `http.Redirect` runs over the location
/// before it sets `Location` (`net/http/server.go`).
///
/// Each byte at or above `0x80` becomes `%` and two lowercase hex digits, which
/// is what `strconv.AppendInt(b, int64(s[i]), 16)` produces. The escape works on
/// bytes rather than characters, so one non-ASCII character becomes two or more
/// escapes, exactly as in Go.
fn hex_escape_non_ascii(raw: &str) -> String {
    if raw.is_ascii() {
        return raw.to_owned();
    }
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii() {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:x}");
        }
    }
    out
}

/// Go's `htmlEscape`, the replacer `http.Redirect` runs over the location
/// before it puts it in the anchor (`net/http/server.go`).
fn html_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escape_matches_gos_replacer() {
        assert_eq!(html_escape("/api-sdk/"), "/api-sdk/");
        assert_eq!(
            html_escape("/api-sdk/?a=1&b=2"),
            "/api-sdk/?a=1&amp;b=2",
            "Go escapes the ampersand a query brings into the anchor"
        );
        assert_eq!(html_escape("<\"'>"), "&lt;&#34;&#39;&gt;");
    }

    #[test]
    fn the_location_is_hex_escaped_the_way_go_escapes_it() {
        assert_eq!(
            hex_escape_non_ascii("/api-sdk/?a=1&b=2"),
            "/api-sdk/?a=1&b=2"
        );
        // One two-byte character becomes two escapes, because Go escapes bytes.
        assert_eq!(hex_escape_non_ascii("/caf\u{e9}"), "/caf%c3%a9");
        // The header carries the escape and the body carries the raw location,
        // which is the one place the two disagree.
        let response = moved_permanently("/caf\u{e9}/", &Method::GET, None);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/caf%c3%a9/")
        );
    }
}
