//! Core domain state for the wire-pod SDK app: robot identity, the injectable
//! timing constants, Go-compatible number formatting, the per-robot stream
//! ownership state machines and the never-pruned camera meters.
//!
//! Everything here is synchronous. The ownership state machines guard their
//! state with [`std::sync::Mutex`] and never hold a guard across an `.await`,
//! which is what lets them be driven from plain `#[test]` functions with no
//! runtime. The crate-level `deny` below makes that a compile error rather than
//! a review comment.
//!
//! The rest of the crate's eventual responsibility (config, path resolution,
//! the logger ring, the jdocs, bot-info and session-cert stores, and the
//! pinger) arrives in later commits.
#![deny(clippy::await_holding_lock)]

pub mod esn;
pub mod gofmt;
pub mod robot;
pub mod timings;

pub use crate::esn::{Esn, Generation};
pub use crate::gofmt::{GoJsonError, go_format_f32, go_json_f64};
pub use crate::robot::{CamMeter, CamMeters, CamOwner, EventOwner, StimSample};
pub use crate::timings::Timings;
