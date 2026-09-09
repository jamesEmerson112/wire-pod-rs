//! `/ok` and `/ok:80`: the robot's liveness heartbeat.
//!
//! Both are exact patterns on Go's mux (`server.go:813-814`) and both reach
//! `connCheck` (`jdocspinger.go:193-221`). The handler reads one parameter,
//! `runMDNS`, through `r.FormValue`, answers `ran` when it is exactly the
//! string `true`, and otherwise answers `ok`. Both bodies are written with
//! `fmt.Fprintf` and have no trailing newline.
//!
//! The slice implements the bodies only. Go's `ok` path also does the jdocs
//! pinger bookkeeping for the peer IP and spawns mDNS for a peer the bot-info
//! file does not mention; both belong with P1's mDNS and jdocs work and are
//! recorded as deferred in `deviations.md` entry 2. The pinger state machine
//! itself is implemented and tested in `wirepod-core`; only its driver is
//! missing, so wiring it up here is a few lines rather than a design.

use axum::extract::Request;
use axum::response::Response;

use crate::form;
use crate::{literals, reply};

/// The parameter that turns the heartbeat into an mDNS re-announce
/// (`jdocspinger.go:199`).
const RUN_MDNS: &str = "runMDNS";

/// Answers the heartbeat.
pub async fn handle(req: Request) -> Response {
    let (_parts, form) = form::read(req).await;
    if form.get(RUN_MDNS) == "true" {
        reply::text(literals::MDNS_RAN)
    } else {
        reply::text(literals::OK)
    }
}
