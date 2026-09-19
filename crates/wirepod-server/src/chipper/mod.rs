//! Go's `pkg/servers/chipper`: the chipper gRPC service.

pub mod connectioncheck;
pub mod intent;
pub mod intent_graph;
pub mod knowledgegraph;
pub mod options;
pub mod server;
pub mod textintent;

pub use crate::chipper::options::Options;
pub use crate::chipper::server::Server;
