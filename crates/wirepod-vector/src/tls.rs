//! The TLS dialler the robot answers on port 443.
//!
//! A Vector presents a self-signed certificate that Anki minted for it, so no
//! root store on this machine will ever trust it. Every SDK therefore turns
//! certificate verification off. The Go path is three hops: `vector.New` passes
//! `client.WithInsecureSkipVerify()`
//! (`vector-go-sdk@v0.0.0-20231108155304-62168f3595d6/pkg/vector/vector.go:44`),
//! hugh turns that into `&tls.Config{InsecureSkipVerify: true}` with no
//! `RootCAs` and no `ServerName`
//! (`hugh@v0.0.0-20210210154335-f4159b9fcd5f/grpc/client/client.go:124-128`),
//! and hands it to `credentials.NewTLS`
//! (`client.go:55-56`), which is what grpc-go dials with.
//!
//! This module reproduces that with rustls: an [`InsecureTlsConnector`] that
//! opens a TCP connection, completes a TLS handshake through a
//! [`ServerCertVerifier`] that accepts every chain, and hands tonic the
//! resulting stream. `Endpoint::connect_with_connector` is the seam that lets
//! it do so without tonic's own `tls` feature, which cannot be enabled here
//! because it would pull `rustls-native-certs` and its companions into
//! `Cargo.lock`.

use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper_util::rt::TokioIo;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tonic::transport::Uri;
use tower::Service;

/// The only ALPN protocol the connector offers.
///
/// grpc-go appends `h2` to `NextProtos` inside `credentials.NewTLS`
/// (`google.golang.org/grpc@v1.82.1/credentials/tls.go:239`), so a Go client
/// offers `h2` and whatever the caller already listed, which for hugh is
/// nothing. One entry is therefore the same offer.
const ALPN_H2: &[u8] = b"h2";

/// The port a robot's gateway listens on, used when the URI names none.
///
/// Go never omits it: the target is `robot.IPAddress + ":443"`
/// (`chipper/pkg/wirepod/sdkapp/robot.go:336`).
const ROBOT_TLS_PORT: u16 = 443;

/// A certificate verifier that accepts every chain it is shown.
///
/// This is `InsecureSkipVerify: true`, not a weakening of it. Go's
/// `crypto/tls` skips exactly two things under that flag, chain building
/// against a root store and the hostname match, and still verifies the
/// handshake signature against the public key in the presented certificate.
/// Delegating [`verify_tls12_signature`](Self::verify_tls12_signature) and
/// [`verify_tls13_signature`](Self::verify_tls13_signature) back to the crypto
/// provider is what keeps that half intact; a verifier that asserted those too
/// would accept a handshake Go rejects.
///
/// The robot is reached by IP on a local network and its certificate is
/// self-signed by Anki, so there is nothing to verify against and no name to
/// match. Accepting the chain is what makes the connection possible at all,
/// and it is what every Vector SDK does.
#[derive(Debug)]
struct AcceptAnyServerCert {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// The rustls client configuration the robot is dialled with.
///
/// The ring provider is named rather than taken from the process default,
/// because installing a process default is a global side effect a library has
/// no business performing and it fails if something else installed one first.
///
/// TLS 1.2 stays enabled. grpc-go raises `MinVersion` to TLS 1.2 and no higher
/// (`google.golang.org/grpc@v1.82.1/credentials/tls.go:243-245`), and Vector's
/// gateway is old, so refusing anything below 1.3 would be a way to fail
/// against real hardware for no gain. `ClientConfig` does not report which
/// versions it enabled, so that is pinned by handshake instead:
/// `tests/tls.rs::a_tls12_only_robot_still_answers` runs this configuration
/// against a server that offers nothing but TLS 1.2.
///
/// What rustls offers that Go does not, and the reverse, is deviation 24.
pub fn insecure_client_config() -> ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("the ring provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert { provider }))
        .with_no_client_auth();
    config.alpn_protocols = vec![ALPN_H2.to_vec()];
    config
}

/// The bare host an authority names, with the brackets an IPv6 literal carries
/// in a URI removed.
///
/// `http::Uri::host` keeps them, and neither `TcpStream::connect` nor
/// [`ServerName`] wants them.
fn bare_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
}

/// The [`ServerName`] to hand rustls for a host.
///
/// An address that parses as an IP literal becomes [`ServerName::IpAddress`],
/// which rustls sends no SNI extension for. That is what matches Go: hugh
/// leaves `ServerName` empty and `crypto/tls` fills it from the dial target
/// only when the target is not an IP literal, so a robot dialled at
/// `192.168.x.x:443` receives no SNI from either side. The IP is parsed here
/// rather than left to `ServerName::try_from`, which reaches the same answer
/// only by way of a DNS syntax check that happens to reject an all-numeric
/// final label; deciding it explicitly makes the SNI behaviour a property of
/// this function rather than a side effect of that rule.
fn server_name(host: &str) -> io::Result<ServerName<'static>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ServerName::IpAddress(ip.into()));
    }
    ServerName::try_from(host.to_owned()).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid server name {host}: {err}"),
        )
    })
}

/// Opens a TLS connection to the authority a [`Uri`] names.
///
/// This is a [`tower::Service`] because that is the shape
/// `tonic::transport::Endpoint::connect_with_connector` takes. It is cheap to
/// clone: every clone shares the one [`ClientConfig`], and therefore the one
/// session cache, exactly as a single `credentials.TransportCredentials` value
/// is shared across a Go client's dials.
#[derive(Clone)]
pub struct InsecureTlsConnector {
    config: Arc<ClientConfig>,
}

impl Default for InsecureTlsConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl InsecureTlsConnector {
    /// A connector over a fresh [`insecure_client_config`].
    pub fn new() -> Self {
        Self {
            config: Arc::new(insecure_client_config()),
        }
    }

    /// A connector over an existing configuration.
    pub fn with_config(config: Arc<ClientConfig>) -> Self {
        Self { config }
    }

    /// The configuration this connector hands rustls.
    pub fn config(&self) -> &Arc<ClientConfig> {
        &self.config
    }
}

impl std::fmt::Debug for InsecureTlsConnector {
    /// The `ClientConfig` inside is large and its `Debug` says nothing useful,
    /// so this prints the two properties that are actually load-bearing.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InsecureTlsConnector")
            .field("alpn_protocols", &self.config.alpn_protocols)
            .field("certificate_verification", &"accept-any")
            .finish()
    }
}

impl Service<Uri> for InsecureTlsConnector {
    type Response = TokioIo<TlsStream<TcpStream>>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Self::Response>> + Send>>;

    /// Always ready. There is no pool and no permit to wait on: each call opens
    /// its own socket, which is what grpc-go's dialer does too.
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let config = Arc::clone(&self.config);
        Box::pin(async move {
            let host = uri
                .host()
                .map(bare_host)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("no host to dial in {uri}"),
                    )
                })?
                .to_owned();
            let port = uri.port_u16().unwrap_or(ROBOT_TLS_PORT);
            let name = server_name(&host)?;
            let tcp = TcpStream::connect((host.as_str(), port)).await?;
            let stream = TlsConnector::from(config).connect(name, tcp).await?;
            Ok(TokioIo::new(stream))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_config_offers_h2_and_nothing_else() {
        let config = insecure_client_config();
        assert_eq!(config.alpn_protocols, vec![b"h2".to_vec()]);
    }

    #[test]
    fn the_verifier_accepts_a_chain_no_root_would_trust() {
        let verifier = AcceptAnyServerCert {
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        };
        let nonsense = CertificateDer::from(vec![0x30, 0x00]);
        verifier
            .verify_server_cert(
                &nonsense,
                &[],
                &server_name("192.168.8.203").expect("an IP is a server name"),
                &[],
                UnixTime::since_unix_epoch(Duration::from_secs(0)),
            )
            .expect("accepts anything");
    }

    #[test]
    fn the_verifier_still_reports_the_providers_signature_schemes() {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let expected = provider
            .signature_verification_algorithms
            .supported_schemes();
        let verifier = AcceptAnyServerCert { provider };
        assert_eq!(verifier.supported_verify_schemes(), expected);
        assert!(!expected.is_empty());
    }

    #[test]
    fn an_ip_literal_sends_no_sni() {
        assert!(matches!(
            server_name("192.168.8.203").expect("an IP is a server name"),
            ServerName::IpAddress(_)
        ));
    }

    #[test]
    fn a_host_name_stays_a_dns_name() {
        assert!(matches!(
            server_name("escapepod.local").expect("a name is a server name"),
            ServerName::DnsName(_)
        ));
    }

    #[test]
    fn a_bracketed_ipv6_host_loses_its_brackets() {
        assert_eq!(bare_host("[::1]"), "::1");
        assert_eq!(bare_host("192.168.8.203"), "192.168.8.203");
    }
}
