//! Dialling a robot.
//!
//! Production dials TLS with the accept-all verifier the Go SDK uses, which is
//! [`crate::tls`]. Tests dial plaintext HTTP/2 through an injected endpoint
//! builder, so the loopback fake can bind an ephemeral port without the factory
//! knowing anything about tests.

use std::sync::Arc;

use async_trait::async_trait;
use tonic::transport::Endpoint;
use wirepod_core::{ConnError, ConnTarget, RobotConn, RobotConnFactory};

use crate::conn::TonicRobotConn;
use crate::error::{dial_error, endpoint_error};
use crate::tls::InsecureTlsConnector;

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

/// The endpoint the TLS path dials.
///
/// The scheme is `https` because that is what the connection is and what the
/// `:scheme` pseudo-header should therefore say; grpc-go sets the same one
/// whenever transport credentials are configured. tonic does not act on the
/// scheme here, because the check that rejects an `https` URI without TLS
/// support lives inside the connector tonic wraps around its own HTTP
/// connector and is compiled out when tonic's `tls` feature is off
/// (`tonic-0.12.3/src/transport/channel/service/connector.rs:56-72`). The
/// handshake is [`InsecureTlsConnector`]'s job instead.
fn tls_endpoint(target: &ConnTarget) -> Result<Endpoint, ConnError> {
    let uri = format!("https://{}", authority(target));
    Endpoint::from_shared(uri.clone())
        .map_err(|err| endpoint_error(format!("invalid endpoint {uri}: {err}")))
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

/// How a factory reaches a robot.
enum Dialer {
    /// `Endpoint::connect`, over an injected endpoint. Plaintext HTTP/2, which
    /// only the loopback fake speaks.
    Plaintext(EndpointBuilder),
    /// `Endpoint::connect_with_connector`, over the TLS connector the Go SDK
    /// describes. This is production.
    Tls(InsecureTlsConnector),
}

/// Dials robots over gRPC.
pub struct TonicConnFactory {
    dialer: Dialer,
}

impl Default for TonicConnFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl TonicConnFactory {
    /// The production factory, which is [`Self::insecure_tls`].
    pub fn new() -> Self {
        Self::insecure_tls()
    }

    /// A factory that dials TLS on port 443 and accepts the robot's
    /// self-signed certificate.
    ///
    /// The name says `insecure` for the same reason hugh's option is called
    /// `WithInsecureSkipVerify`: certificate verification is off, deliberately
    /// and unavoidably, because Anki signed the robot's certificate with a key
    /// nothing on this machine has. [`crate::tls`] carries the full reasoning.
    pub fn insecure_tls() -> Self {
        Self {
            dialer: Dialer::Tls(InsecureTlsConnector::new()),
        }
    }

    /// A factory that dials plaintext HTTP/2 through the given endpoint
    /// builder.
    ///
    /// Tests only. Pair it with [`plaintext_builder`].
    pub fn with_endpoint_builder(build: EndpointBuilder) -> Self {
        Self {
            dialer: Dialer::Plaintext(build),
        }
    }

    /// The TLS connector this factory dials with, when it has one.
    ///
    /// A caller that wants to reuse the session cache for something other than
    /// a gRPC channel, such as the `/v1/update_settings` REST call, can clone
    /// it rather than building a second configuration.
    pub fn tls_connector(&self) -> Option<&InsecureTlsConnector> {
        match &self.dialer {
            Dialer::Tls(connector) => Some(connector),
            Dialer::Plaintext(_) => None,
        }
    }
}

#[async_trait]
impl RobotConnFactory for TonicConnFactory {
    /// Opens the channel and wraps it. No RPC is issued here by design: the
    /// connect-time `BatteryState` liveness check Go performs
    /// (`robot.go:365-369`) belongs to the registry, which is what decides
    /// whether a dialled robot counts as reachable.
    ///
    /// Both arms dial eagerly where `grpc.Dial` is lazy
    /// (`hugh@v0.0.0-20210210154335-f4159b9fcd5f/grpc/client/client.go:87`), so
    /// an unreachable robot fails here rather than at the first RPC. Deviation
    /// 23 records what that changes about the error text and what it does not.
    async fn connect(&self, target: &ConnTarget) -> Result<Arc<dyn RobotConn>, ConnError> {
        let channel = match &self.dialer {
            Dialer::Plaintext(build) => {
                let endpoint = build(target)?;
                endpoint.connect().await.map_err(|err| dial_error(&err))?
            }
            Dialer::Tls(connector) => {
                let endpoint = tls_endpoint(target)?;
                endpoint
                    .connect_with_connector(connector.clone())
                    .await
                    .map_err(|err| dial_error(&err))?
            }
        };
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
    fn the_tls_endpoint_is_https_on_the_go_port() {
        let endpoint = tls_endpoint(&target("192.168.8.203")).expect("valid endpoint");
        assert_eq!(endpoint.uri().scheme_str(), Some("https"));
        assert_eq!(
            endpoint.uri().authority().map(|a| a.as_str()),
            Some("192.168.8.203:443")
        );
    }

    #[test]
    fn the_plaintext_builder_accepts_a_loopback_address() {
        plaintext_builder()(&target("127.0.0.1:51234")).expect("valid endpoint");
    }

    #[test]
    fn the_default_factory_is_the_tls_one() {
        assert!(TonicConnFactory::default().tls_connector().is_some());
        assert!(
            TonicConnFactory::with_endpoint_builder(plaintext_builder())
                .tls_connector()
                .is_none()
        );
    }
}
