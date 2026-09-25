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
