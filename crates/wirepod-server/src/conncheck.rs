//! `/ok` and `/ok:80`: the robot's liveness heartbeat.
//!
//! Both are exact patterns on Go's mux (`server.go:813-814`) and both reach
//! `connCheck` (`jdocspinger.go:193-221`).

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::Response;
use wirepod_core::{AppState, host_of, marshal_bot_info};

use crate::form;
use crate::jdocspinger;
use crate::peer::PeerAddr;
use crate::{literals, reply};

/// The parameter that turns the heartbeat into an mDNS re-announce
/// (`jdocspinger.go:199`).
const RUN_MDNS: &str = "runMDNS";

/// Go's `RunMDNS("t")` argument on the `runMDNS=true` path
/// (`jdocspinger.go:200`).
const MDNS_MARKER: &str = "t";

/// Answers the heartbeat.
pub async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    // Go reads `r.RemoteAddr`, which is always set; a request that never came
    // off a listener has none, and the empty string is what the rest of the
    // handler then works from.
    let remote_addr = req
        .extensions()
        .get::<PeerAddr>()
        .map(|peer| peer.0.to_string())
        .unwrap_or_default();
    let (_parts, form) = form::read(req).await;

    if form.get(RUN_MDNS) == "true" {
        jdocspinger::run_mdns(Arc::clone(&state), MDNS_MARKER.to_owned()).await;
        return reply::text(literals::MDNS_RAN);
    }

    if state.pinger().is_enabled() {
        let robot_target = host_of(&remote_addr).to_owned();
        let bot_info = marshal_bot_info(&state.bot_info_snapshot());
        let bot_info = String::from_utf8_lossy(&bot_info);
        if bot_info.contains(robot_target.trim()) {
            if jdocspinger::should_ping_jdocs(&state, &robot_target) {
                jdocspinger::ping_jdocs(&state, &robot_target).await;
            }
        } else {
            tokio::spawn(jdocspinger::run_mdns(Arc::clone(&state), robot_target));
        }
    }

    reply::text(literals::OK)
}
