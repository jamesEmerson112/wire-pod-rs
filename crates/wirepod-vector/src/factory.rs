//! Dialling a robot.
//!
//! The endpoint is built through an injectable closure so that the loopback
//! test can point the same client at a fake robot on an ephemeral port without
//! the factory knowing anything about tests.

use std::sync::Arc;

use async_trait::async_trait;
use tonic::transport::Endpoint;
use wirepod_core::{ConnError, ConnTarget, RobotConn, RobotConnFactory, StatusCode};

use crate::conn::TonicRobotConn;
use crate::error::{dial_error, endpoint_error};

/// Turns a [`ConnTarget`] into the endpoint to dial.
pub type EndpointBuilder = Box<dyn Fn(&ConnTarget) -> Result<Endpoint, ConnError> + Send + Sync>;

/// The authority a target names, which is `ip:443` unless the address already
/// carries a port.
///
/// Go builds it by plain concatenation, `robot.IPAddress + ":443"`
/// (`robot.go:336`), so a bracketed IPv6 literal would need the same handling
/// there and does not get it. The port-carrying case exists for the loopback
/// fake, which binds an ephemeral port.
fn authority(target: &ConnTarget) -> String {
    if target.ip.contains(':') {
        target.ip.clone()
    } else {
        target.grpc_target()
    }
}

/// The builder a [`TonicConnFactory`] uses when none is injected.
///
/// It refuses to dial. The robot presents a self-signed certificate, which the
/// Go SDK accepts with `client.WithInsecureSkipVerify()`
/// (`vector-go-sdk@v0.0.0-20231108155304-62168f3595d6/pkg/vector/vector.go:42-48`),
/// and reproducing that needs tonic's `tls` feature, which pulls `tokio-rustls`
/// and its companions into tonic's entry in `Cargo.lock`. The slice's lock gate
/// forbids that, so real-robot TLS is a documented follow-up and the default
/// path fails loudly rather than silently dialling plaintext to a robot that
/// would never answer.
pub fn default_builder() -> EndpointBuilder {
    Box::new(|target| {
        Err(ConnError::new(
            StatusCode::Unavailable,
            format!(
                "TLS dialling is not configured yet; no endpoint for {}",
                authority(target)
            ),
        ))
    })
}

/// A builder that dials plaintext HTTP/2.
///
/// This is what the loopback fake speaks. A real robot answers TLS only, so
/// this is never the production path.
pub fn plaintext_builder() -> EndpointBuilder {
    Box::new(|target| {
        let uri = format!("http://{}", authority(target));
        Endpoint::from_shared(uri.clone())
            .map_err(|err| endpoint_error(format!("invalid endpoint {uri}: {err}")))
    })
}

/// Dials robots over gRPC.
pub struct TonicConnFactory {
    build: EndpointBuilder,
}

impl Default for TonicConnFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl TonicConnFactory {
    /// A factory using [`default_builder`].
    pub fn new() -> Self {
        Self {
            build: default_builder(),
        }
    }

    /// A factory using the given endpoint builder.
    pub fn with_endpoint_builder(build: EndpointBuilder) -> Self {
        Self { build }
    }
}

#[async_trait]
impl RobotConnFactory for TonicConnFactory {
    /// Opens the channel and wraps it. No RPC is issued here by design: the
    /// connect-time `BatteryState` liveness check Go performs
    /// (`robot.go:365-369`) belongs to the registry, which is what decides
    /// whether a dialled robot counts as reachable.
    async fn connect(&self, target: &ConnTarget) -> Result<Arc<dyn RobotConn>, ConnError> {
        let endpoint = (self.build)(target)?;
        let channel = endpoint.connect().await.map_err(|err| dial_error(&err))?;
        Ok(Arc::new(TonicRobotConn::new(channel, &target.guid)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wirepod_core::Esn;

    fn target(ip: &str) -> ConnTarget {
        ConnTarget {
            esn: Esn::new("00303f28"),
            ip: ip.to_owned(),
            guid: "<guid>".to_owned(),
        }
    }

    #[test]
    fn a_bare_address_gets_the_go_port() {
        assert_eq!(authority(&target("192.168.8.203")), "192.168.8.203:443");
    }

    #[test]
    fn an_address_with_a_port_keeps_it() {
        assert_eq!(authority(&target("127.0.0.1:51234")), "127.0.0.1:51234");
    }

    #[test]
    fn the_default_builder_refuses_and_names_the_target() {
        let err = default_builder()(&target("192.168.8.203")).expect_err("no TLS yet");
        assert_eq!(err.code, StatusCode::Unavailable);
        assert!(err.desc.contains("192.168.8.203:443"), "{}", err.desc);
    }

    #[test]
    fn the_plaintext_builder_accepts_a_loopback_address() {
        plaintext_builder()(&target("127.0.0.1:51234")).expect("valid endpoint");
    }
}
