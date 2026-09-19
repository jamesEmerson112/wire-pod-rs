//! Go's `pkg/servers/jdocs`: the jdocs gRPC service.

pub mod server;

pub use crate::jdocs::server::{JdocServer, new_jdocs_server};
