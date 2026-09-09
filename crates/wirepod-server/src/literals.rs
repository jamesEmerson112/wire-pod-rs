//! Every byte-exact body and header value this surface writes.
//!
//! The vendored web UI reads several of these as literal strings rather than as
//! structured data, so a stray newline or a changed word is a bug the parity
//! diff would find long after the fact. Each one is a constant here with a
//! verbatim assertion in `tests/contract_bodies.rs`, which is what turns an
//! accidental edit into a failing test.
//!
//! Where Go writes with `fmt.Fprint` the body has no trailing newline; where it
//! writes with `http.Error` or `json.Encoder.Encode` it has one. That
//! difference is reproduced rather than harmonised, so the constants below
//! carry their newline when Go's body has one.

/// `/api-sdk/conn_test` (`server.go:73`). The connect happened in the
/// preamble, so the arm has nothing left to do.
pub const SUCCESS: &str = "success";

/// The answer from `begin_cam_stream` (`server.go:519`) and from most of the
/// deferred settings routes.
pub const DONE: &str = "done";

/// The prefix the `/api-sdk/*` preamble puts in front of a connect failure
/// (`server.go:61`).
pub const ERROR_PREFIX: &str = "error: ";

/// What an unknown serial produces, in full.
///
/// The prefix appears twice because Go's own message already starts with
/// `error: ` (`robot.go:349`) and the preamble prepends another
/// (`server.go:61`). The doubling is visible in the dashboard, so it is
/// contract.
pub const ROBOT_NOT_FOUND: &str = "error: error: robot not found in SDK info file";

/// The body of `http.Error(w, "not found", http.StatusNotFound)`, which is
/// both dispatch defaults: `/api-sdk/*` (`server.go:69`) and `/api/*`
/// (`webserver.go:77`). `http.Error` supplies the newline.
pub const NOT_FOUND: &str = "not found\n";

/// `/api-sdk/get_sdk_info` when no robot has authenticated yet
/// (`server.go:187`), at HTTP 500 through `http.Error`.
pub const NO_BOTS_AUTHENTICATED: &str = "no bots are authenticated\n";

/// `/api-sdk/get_sdk_info` when the marshal fails (`server.go:192`), at HTTP
/// 200 with no newline. Go's marshal of this struct cannot fail and neither
/// can ours; the arm exists so the body is not invented later.
pub const ERROR_MARSHALING_JSON: &str = "error marshaling json";

/// `/ok` and `/ok:80`, the robot's liveness heartbeat (`jdocspinger.go:218`).
pub const OK: &str = "ok";

/// `/ok?runMDNS=true` (`jdocspinger.go:200`).
pub const MDNS_RAN: &str = "ran";

/// The root file server's 404 (`webserver.go:439`), which every path none of
/// the registered patterns matches reaches.
pub const FILE_NOT_FOUND: &str = "404 page not found\n";

/// `/api/get_bot_status` with no robots in the bot-info file.
///
/// Go builds the slice as `[]BotStatus{}` rather than as a nil slice precisely
/// so this is `[]` and never `null` (`jdocspinger.go:42-45`), and
/// `json.Encoder.Encode` supplies the newline.
pub const EMPTY_BOT_STATUS: &str = "[]\n";

/// `http.StatusText(http.StatusMovedPermanently)`, the link text in the 301
/// body Go's mux writes for a bare subtree prefix.
pub const MOVED_PERMANENTLY: &str = "Moved Permanently";

/// What Go's content sniffing yields for every text body on this surface, and
/// what `http.Error` sets explicitly.
pub const CONTENT_TYPE_TEXT: &str = "text/plain; charset=utf-8";

/// Set explicitly by `/api/get_bot_status` (`webserver.go:307`).
pub const CONTENT_TYPE_JSON: &str = "application/json";

/// Set by `http.Redirect` on a GET or HEAD (`net/http/server.go`).
pub const CONTENT_TYPE_HTML: &str = "text/html; charset=utf-8";

/// The `X-Content-Type-Options` value `http.Error` and
/// `DisableCachingAndSniffing` both set.
pub const NOSNIFF: &str = "nosniff";

/// The `Pragma` value `DisableCachingAndSniffing` sets (`webserver.go:418`).
pub const NO_CACHE: &str = "no-cache";

/// The `Expires` value `DisableCachingAndSniffing` sets (`webserver.go:420`).
pub const EXPIRES_ZERO: &str = "0";

/// The value of both CORS headers `apiHandler` sets, on every `/api/*`
/// response including its 404 (`webserver.go:28-29`).
pub const CORS_ANY: &str = "*";
