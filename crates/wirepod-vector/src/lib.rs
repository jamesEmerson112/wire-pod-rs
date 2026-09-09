//! Outbound robot client: the tonic implementation of the `wirepod-core` robot
//! seam.
//!
//! This crate is the only place that knows the robot speaks gRPC. It holds the
//! [`conn::TonicRobotConn`] client, which attaches the bearer authorisation
//! metadata the Go SDK sends on every RPC, the [`stream`] adapters that turn
//! `tonic::Streaming` into core's domain streams, the [`error`] conversion from
//! `tonic::Status` into core's `ConnError`, and the [`factory`] that dials a
//! robot. `wirepod-core` stays free of tonic and `wirepod-proto`, so it can sit
//! at the bottom of the dependency graph and own `AppState`.
//!
//! The remaining Go surface this crate eventually replaces, the
//! `/v1/update_settings` REST calls in `urlreqs.go` and the port 8889
//! consolevar, arrives in a later commit.

pub mod conn;
pub mod error;
pub mod factory;
pub mod stream;
#[cfg(feature = "test-util")]
pub mod test_support;

pub use crate::conn::TonicRobotConn;
pub use crate::error::{dial_error, endpoint_error, status_code, status_error};
pub use crate::factory::{EndpointBuilder, TonicConnFactory, default_builder, plaintext_builder};
pub use crate::stream::{TonicEventReceiver, TonicFrameStream};
