//! The nav map page and the snapshot it polls. Neither has a counterpart in the
//! Go server; both are recorded in `docs/translation.md` as additions.
//!
//! `GET /navmap?serial=<esn>` serves `navmap.html`, compiled into the binary
//! with `include_str!` because `assets/` is vendored byte-identically from the
//! Go repository and cannot take a new file. The page is reachable on the plain
//! HTTP listeners only, like the rest of the web UI.
//!
//! `GET /api-navmap/snapshot?serial=<esn>` resolves the robot, touches his idle
//! clock, renews the lease on his map feed, starts the feed if it is not
//! running, and answers the latest state of both streams. The server
//! reconstructs every quad's position; the page only draws. The reply is this
//! contract, which the page depends on exactly:
//!
//! ```json
//! {
//!   "serial": "00303f28",
//!   "status": "streaming",
//!   "map": {
//!     "origin_id": 7,
//!     "root": { "cx": 64.0, "cy": 0.0, "size_mm": 512.0, "depth": 6 },
//!     "quads": [[64.0, 0.0, 512.0, 1, 4278190335]],
//!     "counts": {
//!       "unknown": 0, "clear_of_obstacle": 1, "clear_of_cliff": 0,
//!       "obstacle_cube": 0, "obstacle_proximity": 0,
//!       "obstacle_proximity_explored": 0, "obstacle_unrecognized": 0,
//!       "cliff": 0, "interesting_edge": 0, "non_interesting_edge": 0
//!     },
//!     "received_ms": 1790000000000
//!   },
//!   "robot": { "x": 12.5, "y": -3.0, "angle": 0.12, "origin_id": 7,
//!              "localized_to": 0, "flags": ["moving", "wheels_moving"] }
//! }
//! ```
//!
//! Each quad is `[cx, cy, side, content, rgba]`: centre and side in
//! millimetres, the `NavNodeContentType` number, and the robot's packed colour
//! with red in the high byte and alpha in the low byte. `counts` has one key
//! per content type, named as [`NavContent::name`] names them.
//!
//! `map` is null until a map has arrived, and `robot` is null until a state
//! sample has. `status` is `starting` when this request started the feed,
//! `waiting_for_map` while a feed runs with no map yet, `streaming` once a map
//! has arrived, and otherwise the feed's error text.
//!
//! [`NavContent::name`]: wirepod_core::robot::navmap::NavContent::name

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::Response;
use http::{HeaderValue, header};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use tokio_util::sync::CancellationToken;
use wirepod_core::robot::navmap::{ContentCounts, NavContent, ReconstructError, reconstruct};
use wirepod_core::robot::navmap_feed::MapFeed;
use wirepod_core::robot::observe::ReceivedMap;
use wirepod_core::{AppState, Esn, RobotEntry, RobotStateSample};

use crate::{form, literals, reply};

/// The page.
pub const PAGE_PATH: &str = "/navmap";

/// The snapshot the page polls.
pub const SNAPSHOT_PATH: &str = "/api-navmap/snapshot";

/// `status` when this request started the feed.
pub const STARTING: &str = "starting";

/// `status` while the feed runs and no map has arrived.
pub const WAITING_FOR_MAP: &str = "waiting_for_map";

/// `status` once a map has arrived.
pub const STREAMING: &str = "streaming";

const PAGE: &str = include_str!("navmap.html");

/// `GET /navmap`.
pub async fn page() -> Response {
    let mut response = reply::text(PAGE);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(literals::CONTENT_TYPE_HTML),
    );
    response
}

/// `GET /api-navmap/snapshot?serial=<esn>`.
///
/// A serial that does not resolve answers the contract with the error as its
/// `status`, at HTTP 200 like every other body.
pub async fn snapshot(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (_, form) = form::read(req).await;
    let serial = Esn::new(form.get("serial"));
    let body = match state.get_robot(&serial).await {
        Ok(entry) => watch(&state, &entry),
        Err(err) => Snapshot {
            serial: serial.as_str().to_owned(),
            status: err.to_string(),
            map: None,
            robot: None,
        },
    };
    reply::json(encode(&body))
}

/// Touches the robot, renews the lease, starts the feed if it is not running,
/// and reads what both streams last reported.
fn watch(state: &AppState, entry: &RobotEntry) -> Snapshot {
    let now = state.clock().now();
    entry.touch(now);
    let slot = &entry.session.map_feed;
    slot.watch(now);

    // Read before the claim, which clears it.
    let previous_error = slot.error();
    let cancel = CancellationToken::new();
    let started = match slot.claim(cancel.clone()) {
        Some(generation) => {
            let feed = MapFeed {
                slot: Arc::clone(slot),
                generation,
                cancel,
                serial: entry.esn.clone(),
                clock: Arc::clone(state.clock()),
                wall: Arc::clone(state.wall()),
                timings: *state.timings(),
                previous_error: previous_error.clone(),
            };
            let conn = Arc::clone(&entry.conn);
            tokio::spawn(async move {
                feed.open_and_run(conn.as_ref()).await;
            });
            true
        }
        None => false,
    };

    let (map, malformed) = match slot.latest().as_deref().map(map_json) {
        Some(Ok(map)) => (Some(map), None),
        Some(Err(err)) => (None, Some(err.to_string())),
        None => (None, None),
    };
    // A restart after a failure reports the failure rather than `starting`:
    // the page polls every second and would otherwise never see why.
    let status = if let Some(malformed) = malformed {
        malformed
    } else if started {
        previous_error.unwrap_or_else(|| STARTING.to_owned())
    } else if let Some(error) = slot.error() {
        error
    } else if map.is_some() {
        STREAMING.to_owned()
    } else {
        WAITING_FOR_MAP.to_owned()
    };

    Snapshot {
        serial: entry.esn.as_str().to_owned(),
        status,
        map,
        robot: entry.session.state_stream.latest().map(robot_json),
    }
}

/// The reply, in the contract's field order.
#[derive(Debug, Serialize)]
struct Snapshot {
    serial: String,
    status: String,
    map: Option<MapJson>,
    robot: Option<RobotJson>,
}

#[derive(Debug, Serialize)]
struct MapJson {
    origin_id: u32,
    root: RootJson,
    /// `[cx, cy, side, content, rgba]`.
    quads: Vec<(f32, f32, f32, i32, u32)>,
    counts: CountsJson,
    received_ms: i64,
}

#[derive(Debug, Serialize)]
struct RootJson {
    cx: f32,
    cy: f32,
    size_mm: f32,
    depth: i32,
}

/// Every content type as a key, in wire order, including the zeros.
#[derive(Debug)]
struct CountsJson(ContentCounts);

impl Serialize for CountsJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(NavContent::ALL.len()))?;
        for (content, count) in self.0.iter() {
            map.serialize_entry(content.name(), &count)?;
        }
        map.end()
    }
}

#[derive(Debug, Serialize)]
struct RobotJson {
    x: f32,
    y: f32,
    angle: f32,
    origin_id: u32,
    localized_to: i32,
    flags: Vec<&'static str>,
}

fn map_json(received: &ReceivedMap) -> Result<MapJson, ReconstructError> {
    let frame = &received.frame;
    let quads = reconstruct(frame)?
        .into_iter()
        .map(|quad| (quad.cx, quad.cy, quad.side, quad.content, quad.rgba))
        .collect();
    Ok(MapJson {
        origin_id: frame.origin_id,
        root: RootJson {
            cx: frame.info.root_center_x,
            cy: frame.info.root_center_y,
            size_mm: frame.info.root_size_mm,
            depth: frame.info.root_depth,
        },
        quads,
        counts: CountsJson(ContentCounts::of(&frame.quads)),
        received_ms: received.received_ms,
    })
}

fn robot_json(sample: RobotStateSample) -> RobotJson {
    RobotJson {
        x: sample.x_mm,
        y: sample.y_mm,
        angle: sample.angle_rad,
        origin_id: sample.origin_id,
        localized_to: sample.localized_to_object_id,
        flags: sample.status_names(),
    }
}

/// The reply as JSON. Nothing in it can fail to serialize, but the fallback is
/// still the contract's shape rather than a 500.
fn encode(snapshot: &Snapshot) -> String {
    serde_json::to_string(snapshot).unwrap_or_else(|err| {
        serde_json::json!({
            "serial": snapshot.serial,
            "status": err.to_string(),
            "map": null,
            "robot": null,
        })
        .to_string()
    })
}
