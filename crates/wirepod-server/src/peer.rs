//! The peer address of the connection a request arrived on.
//!
//! Go reads it with `peer.FromContext`. The accept loop in `startserver` puts this
//! into every request's extensions, and the jdocs and token services read it back.

use std::net::SocketAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerAddr(pub SocketAddr);
