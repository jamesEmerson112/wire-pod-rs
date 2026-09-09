//! Go's `r.FormValue` merge: the URL query overlaid by the urlencoded body.
//!
//! Every parameter on this surface is read with `r.FormValue`, starting with
//! `serial` (`server.go:57`), so reading the query string alone would be wrong
//! against the real client. The vendored dashboard proves it: `custom_eye_color`
//! is a POST whose `serial` is in the query and whose `hue` and `sat` are in a
//! `application/x-www-form-urlencoded` body (`webroot/sdkapp/js/main.js:248-258`),
//! and most of the other calls are POSTs that declare that same content type and
//! then send an empty body with every parameter in the query
//! (`main.js:150-152`).
//!
//! Go's rule, in `net/http`'s `ParseForm`, is that the body is parsed first for
//! POST, PUT and PATCH, the query is appended after it, and `FormValue` returns
//! the first value under a key. So a body parameter shadows a same-named query
//! parameter, and a key present only in the query is still found. A body that
//! cannot be read leaves the query values in place, because `ParseForm` merges
//! the query whether or not the body parse failed, and `FormValue` discards the
//! error anyway.
//!
//! Multipart bodies are not merged here. `/api-sdk/play_sound` is the only
//! route that sends one and it is deferred, so a multipart body is read as no
//! body at all rather than half-supported.

use axum::extract::Request;
use http::request::Parts;
use http::{HeaderMap, Method, header};
use http_body_util::{BodyExt, Limited};

/// The largest body this merges, which is Go's own `ParseForm` cap of 10 MiB.
const MAX_FORM_BODY: usize = 10 << 20;

/// The media type whose body `ParseForm` merges.
const FORM_MEDIA_TYPE: &str = "application/x-www-form-urlencoded";

/// The merged parameters of one request, in Go's order.
///
/// Body pairs come first and query pairs after, which is what makes
/// [`Form::get`] reproduce `FormValue`: it answers with the first value under
/// a key, so the body wins when both carry it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    pairs: Vec<(String, String)>,
}

impl Form {
    /// Merges an already-read request, which is the shape the pure tests use.
    ///
    /// `body` is `None` when the request has no body to merge, either because
    /// the method is not one of the three Go parses or because the content type
    /// is not urlencoded.
    pub fn merge(query: Option<&str>, body: Option<&str>) -> Self {
        let mut pairs = Vec::new();
        if let Some(body) = body {
            parse_query(body, &mut pairs);
        }
        if let Some(query) = query {
            parse_query(query, &mut pairs);
        }
        Self { pairs }
    }

    /// Go's `r.FormValue(key)`: the first value under `key`, or the empty
    /// string when the key is absent.
    ///
    /// The empty string is what an absent parameter and an explicitly empty one
    /// both produce, exactly as in Go, which is why `?serial=` and no `serial`
    /// at all behave identically.
    pub fn get(&self, key: &str) -> &str {
        self.pairs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
            .unwrap_or_default()
    }

    /// Every parameter, in merge order. Body pairs first, then query pairs.
    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }
}

/// Splits a request into its head and its merged parameters.
///
/// This consumes the body, which is why it returns the [`Parts`] rather than
/// the request: a handler that needs the path or the headers afterwards reads
/// them from there.
pub async fn read(req: Request) -> (Parts, Form) {
    let (parts, body) = req.into_parts();
    let body = if merges_body(&parts.method, &parts.headers) {
        match Limited::new(body, MAX_FORM_BODY).collect().await {
            Ok(collected) => Some(String::from_utf8_lossy(&collected.to_bytes()).into_owned()),
            Err(err) => {
                // Go's `ParseForm` returns this error and merges the query
                // anyway, and `FormValue` never looks at the error
                // (`net/http/request.go`), so an unreadable body costs the
                // request its body parameters and nothing else.
                tracing::warn!(%err, "form body could not be read; using query parameters only");
                None
            }
        }
    } else {
        None
    };
    let form = Form::merge(parts.uri.query(), body.as_deref());
    (parts, form)
}

/// Whether Go's `parsePostForm` would parse this request's body.
///
/// The method test is `POST`, `PUT` or `PATCH` and the content-type test is the
/// urlencoded media type, with parameters such as `charset` ignored. A missing
/// `Content-Type` is treated as `application/octet-stream` by RFC 7231 and by
/// Go, so it merges nothing.
fn merges_body(method: &Method, headers: &HeaderMap) -> bool {
    if !matches!(*method, Method::POST | Method::PUT | Method::PATCH) {
        return false;
    }
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(media_type)
        .is_some_and(|media| media.eq_ignore_ascii_case(FORM_MEDIA_TYPE))
}

/// The media type of a `Content-Type` header, without its parameters.
fn media_type(content_type: &str) -> &str {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim_ascii()
}

/// `url.ParseQuery`, appending onto `out` the way Go's `copyValues` does.
///
/// Three of Go's rules are load-bearing. A segment containing a semicolon is
/// dropped, because Go rejected the semicolon separator in 1.17. A segment that
/// is empty is skipped, so a trailing `&` costs nothing. And a pair whose key or
/// value is not a valid escape sequence is skipped while the rest of the query
/// is still parsed, because `ParseQuery` records the error and continues.
fn parse_query(raw: &str, out: &mut Vec<(String, String)>) {
    for segment in raw.split('&') {
        if segment.is_empty() || segment.contains(';') {
            continue;
        }
        let (key, value) = match segment.split_once('=') {
            Some((key, value)) => (key, value),
            None => (segment, ""),
        };
        let (Some(key), Some(value)) = (query_unescape(key), query_unescape(value)) else {
            continue;
        };
        out.push((key, value));
    }
}

/// Go's `url.QueryUnescape`: `+` is a space and `%XX` is a byte.
///
/// `None` is Go's `EscapeError`, which is a `%` that is not followed by two hex
/// digits. Decoded bytes are turned into a `String` lossily, because Go's
/// strings are byte strings and Rust's are not; every parameter this surface
/// reads is ASCII in practice.
fn query_unescape(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let high = (hex[0] as char).to_digit(16)?;
                let low = (hex[1] as char).to_digit(16)?;
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_shadows_the_query_and_the_query_fills_the_rest() {
        // What the dashboard actually sends for `custom_eye_color`: the serial
        // in the query, the other two parameters in the body.
        let form = Form::merge(Some("serial=00303f28"), Some("hue=0.500&sat=1.000"));
        assert_eq!(form.get("serial"), "00303f28");
        assert_eq!(form.get("hue"), "0.500");
        assert_eq!(form.get("sat"), "1.000");

        // Go appends the query after the body and `FormValue` takes the first
        // value, so a body parameter wins.
        let form = Form::merge(Some("serial=fromquery"), Some("serial=frombody"));
        assert_eq!(form.get("serial"), "frombody");
    }

    #[test]
    fn an_absent_or_empty_parameter_is_the_empty_string() {
        let form = Form::merge(Some("other=1"), None);
        assert_eq!(form.get("serial"), "");

        let form = Form::merge(Some("serial="), None);
        assert_eq!(form.get("serial"), "");

        // An empty body parameter still shadows the query, because Go merges
        // the key whether or not its value is empty.
        let form = Form::merge(Some("serial=00303f28"), Some("serial="));
        assert_eq!(form.get("serial"), "");
    }

    #[test]
    fn the_dashboard_cache_buster_and_other_unknown_keys_are_ignored() {
        // The dashboard appends `&_=<millis>` to defeat caching.
        let form = Form::merge(Some("serial=00303f28&_=1700000000000"), None);
        assert_eq!(form.get("serial"), "00303f28");
        assert_eq!(form.pairs().len(), 2);

        // A bare `&_=` is a key with an empty value, not a parse failure.
        let form = Form::merge(Some("serial=00303f28&_="), None);
        assert_eq!(form.get("serial"), "00303f28");
        assert_eq!(form.get("_"), "");
    }

    #[test]
    fn escapes_follow_gos_query_unescape() {
        let form = Form::merge(Some("text=hello+world&name=a%2Fb"), None);
        assert_eq!(form.get("text"), "hello world");
        assert_eq!(form.get("name"), "a/b");

        // A bad escape drops that pair only; the rest of the query survives.
        let form = Form::merge(Some("bad=%zz&serial=00303f28"), None);
        assert_eq!(form.get("bad"), "");
        assert_eq!(form.get("serial"), "00303f28");

        // A truncated escape at the very end is the same case.
        let form = Form::merge(Some("bad=%2&serial=00303f28"), None);
        assert_eq!(form.get("serial"), "00303f28");
    }

    #[test]
    fn empty_segments_are_skipped_and_semicolons_drop_their_segment() {
        let form = Form::merge(Some("&serial=00303f28&"), None);
        assert_eq!(form.get("serial"), "00303f28");
        assert_eq!(form.pairs().len(), 1);

        // Go stopped accepting `;` as a separator in 1.17: the segment is
        // dropped rather than split.
        let form = Form::merge(Some("a=1;b=2&serial=00303f28"), None);
        assert_eq!(form.get("a"), "");
        assert_eq!(form.get("b"), "");
        assert_eq!(form.get("serial"), "00303f28");
    }

    #[test]
    fn a_key_with_no_equals_is_a_key_with_an_empty_value() {
        let form = Form::merge(Some("serial"), None);
        assert_eq!(form.pairs(), [("serial".to_owned(), String::new())]);
    }

    #[test]
    fn only_a_urlencoded_post_put_or_patch_body_is_merged() {
        let mut headers = HeaderMap::new();
        assert!(!merges_body(&Method::POST, &headers));

        headers.insert(header::CONTENT_TYPE, FORM_MEDIA_TYPE.parse().unwrap());
        assert!(merges_body(&Method::POST, &headers));
        assert!(merges_body(&Method::PUT, &headers));
        assert!(merges_body(&Method::PATCH, &headers));
        // Go parses no body for any other method, so a GET's body is ignored
        // even when it declares the right type.
        assert!(!merges_body(&Method::GET, &headers));
        assert!(!merges_body(&Method::DELETE, &headers));

        // `mime.ParseMediaType` drops the parameters and lowercases the type.
        headers.insert(
            header::CONTENT_TYPE,
            "Application/X-WWW-Form-Urlencoded; charset=UTF-8"
                .parse()
                .unwrap(),
        );
        assert!(merges_body(&Method::POST, &headers));

        headers.insert(header::CONTENT_TYPE, "multipart/form-data".parse().unwrap());
        assert!(!merges_body(&Method::POST, &headers));
    }
}
