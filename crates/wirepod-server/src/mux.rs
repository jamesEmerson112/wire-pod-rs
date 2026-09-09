//! Go's `ServeMux` path canonicalisation, which runs before any pattern is
//! considered.
//!
//! `findHandler` does two things to the request path before it matches
//! anything (`net/http/server.go:2660-2699`). It cleans the escaped path with
//! `cleanPath` and answers a 301 when the cleaned form differs, and it then
//! matches with each segment individually unescaped, because the routing
//! tree's `firstSegment` calls `pathUnescape` (`net/http/routing_tree.go:205-215`).
//! The handlers behind the two subtree prefixes switch on `r.URL.Path`, which
//! `url.Parse` has already decoded, so the decoding is visible twice: once in
//! which pattern matches and once in which `case` arm runs.
//!
//! All of it is observable on the running Go server. `GET /ok%3A80` answers
//! `ok`, because the segment `ok%3A80` unescapes to the registered literal
//! `ok:80`. `GET /api-sdk/deb%75g?serial=bogus` answers `not found` at 404,
//! because the decoded path is `/api-sdk/debug`, which the preamble exempts
//! and which has no `case` arm. `GET /api%2Dsdk/debug?serial=bogus` reaches the
//! same handler, because the prefix segment is unescaped too. And
//! `GET /api-sdk//debug?serial=bogus` answers a 301 to `/api-sdk/debug`.
//!
//! Neither the robot firmware nor the vendored dashboard escapes a route name
//! or sends a doubled slash, so none of this is reachable from a shipped
//! client. It is here because it is the layer every later route inherits, and
//! because `/ok:80` is the one path this router has to match by hand.

use std::borrow::Cow;

/// Go's `cleanPath` (`net/http/server.go:2599-2618`).
///
/// The result is borrowed when the path is already clean, which is what the
/// caller uses to decide between serving the request and answering a 301.
pub fn clean(path: &str) -> Cow<'_, str> {
    if path.is_empty() {
        return Cow::Borrowed("/");
    }
    let rooted = if path.starts_with('/') {
        Cow::Borrowed(path)
    } else {
        Cow::Owned(format!("/{path}"))
    };
    let mut cleaned = lexical_clean(&rooted);
    // `path.Clean` drops a trailing slash and `cleanPath` puts it back, which
    // is what makes `/ok/` a different path from `/ok` rather than the same
    // one. Go's own fast path for this produces the same string as the append.
    if rooted.ends_with('/') && cleaned != "/" {
        cleaned.push('/');
    }
    if cleaned == path {
        Cow::Borrowed(path)
    } else {
        Cow::Owned(cleaned)
    }
}

/// The path with each segment unescaped the way the routing tree unescapes it,
/// or `None` when nothing would change.
///
/// A segment is left exactly as it arrived in two cases, both of which are
/// Go's behaviour or indistinguishable from it. `url.PathUnescape` fails on a
/// `%` that is not followed by two hex digits and `pathUnescape` then keeps the
/// original (`net/http/routing_tree.go:196-203`). And a segment whose decoded
/// form carries a byte that cannot be written back into a URI path -- a `/`,
/// `?`, `#`, `%`, a space, a control byte or anything non-ASCII -- is kept
/// escaped, because rewriting it would either change the path structure or
/// produce a URI that will not parse. Go compares the decoded segment against a
/// pattern, and no pattern this server registers holds such a byte, so both
/// answer the same 404 for the paths that reach a pattern; `deviations.md`
/// entry 18 records the narrower case that does not.
pub fn unescape_segments(path: &str) -> Option<String> {
    if !path.starts_with('/') || !path.contains('%') {
        return None;
    }
    let mut out = String::with_capacity(path.len());
    let mut changed = false;
    for (index, segment) in path.split('/').enumerate() {
        if index > 0 {
            out.push('/');
        }
        match unescape_segment(segment) {
            Some(decoded) => {
                changed = true;
                out.push_str(&decoded);
            }
            None => out.push_str(segment),
        }
    }
    changed.then_some(out)
}

/// One segment's `url.PathUnescape`, or `None` when the segment stays as it is.
///
/// Unlike `url.QueryUnescape`, which [`crate::form`] reproduces, a `+` is a
/// literal plus here rather than a space.
fn unescape_segment(segment: &str) -> Option<String> {
    if !segment.contains('%') {
        return None;
    }
    let mut out = String::with_capacity(segment.len());
    let mut rest = segment;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let hex = rest.as_bytes().get(at + 1..at + 3)?;
        let high = (hex[0] as char).to_digit(16)?;
        let low = (hex[1] as char).to_digit(16)?;
        let byte = (high * 16 + low) as u8;
        if !is_path_byte(byte) {
            return None;
        }
        out.push(byte as char);
        rest = &rest[at + 3..];
    }
    out.push_str(rest);
    Some(out)
}

/// Whether a decoded byte can be written back into a URI path segment as
/// itself.
///
/// This is RFC 3986's `pchar` without its percent-encoded form, which is the
/// set `http::Uri` accepts in a path and which cannot change how the path
/// splits into segments.
fn is_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
        )
}

/// Go's `path.Clean` for a path that is already rooted.
///
/// Transcribed from `path/path.go`, minus the branches that only a relative
/// path reaches, because `cleanPath` roots its argument first.
fn lexical_clean(path: &str) -> String {
    debug_assert!(path.starts_with('/'), "cleanPath roots the path first");
    // The index a `..` may not back past, which for a rooted path is the byte
    // after the leading slash.
    const DOTDOT: usize = 1;

    let bytes = path.as_bytes();
    let n = bytes.len();
    let mut out: Vec<u8> = Vec::with_capacity(n);
    out.push(b'/');
    let mut r = DOTDOT;

    while r < n {
        if bytes[r] == b'/' {
            // An empty element.
            r += 1;
        } else if bytes[r] == b'.' && (r + 1 == n || bytes[r + 1] == b'/') {
            // A `.` element.
            r += 1;
        } else if bytes[r] == b'.'
            && r + 1 < n
            && bytes[r + 1] == b'.'
            && (r + 2 == n || bytes[r + 2] == b'/')
        {
            // A `..` element: back up to the previous slash.
            r += 2;
            if out.len() > DOTDOT {
                let mut w = out.len() - 1;
                while w > DOTDOT && out[w] != b'/' {
                    w -= 1;
                }
                out.truncate(w);
            }
        } else {
            // A real element, with the separator it needs in front of it.
            if out.len() != 1 {
                out.push(b'/');
            }
            while r < n && bytes[r] != b'/' {
                out.push(bytes[r]);
                r += 1;
            }
        }
    }

    match String::from_utf8(out) {
        Ok(cleaned) => cleaned,
        // Unreachable: every byte is copied whole out of `path` and every cut
        // is at an ASCII slash. Handing back the input leaves the path
        // uncleaned rather than panicking on a request.
        Err(_) => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_path_is_borrowed_and_a_dirty_one_is_cleaned() {
        for already_clean in [
            "/",
            "/ok",
            "/ok/",
            "/api-sdk/",
            "/api-sdk/debug",
            "/a.b/c..d",
        ] {
            assert!(
                matches!(clean(already_clean), Cow::Borrowed(_)),
                "{already_clean}"
            );
        }

        // The four the live Go server was probed with.
        assert_eq!(clean("/api-sdk//debug"), "/api-sdk/debug");
        assert_eq!(clean("/api//get_bot_status"), "/api/get_bot_status");
        assert_eq!(clean("/api-sdk/./debug"), "/api-sdk/debug");
        assert_eq!(clean("/api-sdk/x/../debug"), "/api-sdk/debug");

        // A `..` cannot climb past the root, and a trailing `.` loses the slash
        // it was standing in for.
        assert_eq!(clean("/../ok"), "/ok");
        assert_eq!(clean("/../../ok"), "/ok");
        assert_eq!(clean("/api-sdk/."), "/api-sdk");
        assert_eq!(clean("//"), "/");
        assert_eq!(clean("///a//"), "/a/");

        // `cleanPath` roots a path that is not rooted, and answers `/` for the
        // empty string.
        assert_eq!(clean(""), "/");
        assert_eq!(clean("ok"), "/ok");
    }

    #[test]
    fn a_trailing_slash_survives_the_clean() {
        // This is the difference between `/ok`, which is a route, and `/ok/`,
        // which is the file server's 404 on the live Go server.
        assert_eq!(clean("/ok/"), "/ok/");
        assert_eq!(clean("/api-sdk/x/../"), "/api-sdk/");
    }

    #[test]
    fn segments_are_unescaped_the_way_the_routing_tree_unescapes_them() {
        assert_eq!(unescape_segments("/ok%3A80").as_deref(), Some("/ok:80"));
        assert_eq!(unescape_segments("/%6Fk").as_deref(), Some("/ok"));
        assert_eq!(
            unescape_segments("/api%2Dsdk/deb%75g").as_deref(),
            Some("/api-sdk/debug")
        );
        assert_eq!(
            unescape_segments("/api-sdk/get%5Fsdk%5Finfo").as_deref(),
            Some("/api-sdk/get_sdk_info")
        );

        // Nothing to do, so nothing is allocated.
        assert_eq!(unescape_segments("/api-sdk/debug"), None);
        assert_eq!(unescape_segments("/ok:80"), None);
    }

    #[test]
    fn a_segment_that_cannot_be_written_back_stays_escaped() {
        // `%2F` does not create a segment boundary. Go compares the decoded
        // segment `ok/80` against the patterns and matches none of them, and
        // leaving it escaped reaches the same 404.
        assert_eq!(unescape_segments("/ok%2F80"), None);
        // A space, a `?`, a `#` and a `%` are the same case.
        assert_eq!(unescape_segments("/ok%2080"), None);
        assert_eq!(unescape_segments("/ok%3F80"), None);
        assert_eq!(unescape_segments("/ok%2380"), None);
        assert_eq!(unescape_segments("/ok%2580"), None);
        // So is any non-ASCII byte.
        assert_eq!(unescape_segments("/ok%C3%A9"), None);

        // A `%` that is not followed by two hex digits is what Go's
        // `pathUnescape` keeps verbatim, and one bad segment does not stop the
        // others.
        assert_eq!(unescape_segments("/ok%zz"), None);
        assert_eq!(unescape_segments("/ok%2"), None);
        assert_eq!(
            unescape_segments("/api%2Dsdk/deb%zzg").as_deref(),
            Some("/api-sdk/deb%zzg")
        );
    }
}
