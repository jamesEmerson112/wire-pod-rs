//! The tonic client behind the robot seam.
//!
//! One [`TonicRobotConn`] owns one gRPC channel to one robot and implements
//! `wirepod-core`'s [`RobotConn`] and [`CameraControl`]. Every request carries
//! the bearer authorisation metadata, which is what the Go SDK does through
//! `grpc.WithPerRPCCredentials`: per RPC, not per connection.

use async_trait::async_trait;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use tonic::{Request, Status};
use wirepod_core::{
    BatteryLevel, BatteryReading, CameraControl, ConnError, EventReceiver, FrameStream,
    ProtocolResult, ProtocolVerdict, RobotConn, StatusCode,
};
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_proto::anki::vector::external_interface::external_interface_client::ExternalInterfaceClient;

use crate::error::status_error;
use crate::stream::{TonicEventReceiver, TonicFrameStream};

/// The metadata key the robot authenticates on.
///
/// The Go SDK's `tokenAuth.GetRequestMetadata` returns exactly this one header
/// with exactly this prefix, and `RequireTransportSecurity` is true so it only
/// ever travels over TLS
/// (`vector-go-sdk@v0.0.0-20231108155304-62168f3595d6/pkg/vector/token.go:9-17`).
const AUTHORIZATION: &str = "authorization";

/// The prefix the token is sent with (`token.go:11`).
const BEARER_PREFIX: &str = "Bearer ";

/// Attaches `authorization: Bearer <guid>` to every outgoing request.
///
/// An interceptor rather than a per-method insertion, so a method added later
/// cannot forget it. The value is built once at construction because a
/// malformed GUID is a configuration fault worth reporting at connect time
/// rather than on every call.
#[derive(Clone, Debug)]
pub struct BearerAuth {
    value: MetadataValue<Ascii>,
}

impl BearerAuth {
    /// Builds the credential for one robot's GUID.
    ///
    /// Fails when the GUID contains bytes a metadata value cannot hold, which
    /// Go never checks because Go's map value is a plain string and grpc-go
    /// rejects it later.
    pub fn new(guid: &str) -> Result<Self, ConnError> {
        let value = MetadataValue::try_from(format!("{BEARER_PREFIX}{guid}")).map_err(|_| {
            ConnError::new(
                StatusCode::Unauthenticated,
                "robot GUID is not a valid authorization header value",
            )
        })?;
        Ok(Self { value })
    }
}

impl Interceptor for BearerAuth {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request
            .metadata_mut()
            .insert(AUTHORIZATION, self.value.clone());
        Ok(request)
    }
}

/// A live gRPC connection to one robot.
///
/// The generated client takes `&mut self` per call while the seam takes
/// `&self`, so each method clones the client. That is the cheap clone tonic
/// documents: the `Channel` underneath is an `Arc` over one connection pool, so
/// no new socket is opened.
pub struct TonicRobotConn {
    client: ExternalInterfaceClient<InterceptedService<Channel, BearerAuth>>,
}

impl TonicRobotConn {
    /// Wraps a connected channel with the credential for `guid`.
    pub fn new(channel: Channel, guid: &str) -> Result<Self, ConnError> {
        Ok(Self {
            client: ExternalInterfaceClient::with_interceptor(channel, BearerAuth::new(guid)?),
        })
    }

    fn client(&self) -> ExternalInterfaceClient<InterceptedService<Channel, BearerAuth>> {
        self.client.clone()
    }
}

#[async_trait]
impl CameraControl for TonicRobotConn {
    async fn enable_image_streaming(&self, on: bool) -> Result<(), ConnError> {
        // Go sends only `Enable` and leaves the high-resolution flag at its
        // zero value (`server.go:671-678`).
        self.client()
            .enable_image_streaming(pb::EnableImageStreamingRequest {
                enable: on,
                enable_high_resolution: false,
            })
            .await
            .map_err(|status| status_error(&status))?;
        Ok(())
    }
}

#[async_trait]
impl RobotConn for TonicRobotConn {
    async fn battery_state(&self) -> Result<BatteryReading, ConnError> {
        let response = self
            .client()
            .battery_state(pb::BatteryStateRequest {})
            .await
            .map_err(|status| status_error(&status))?
            .into_inner();
        Ok(BatteryReading {
            level: BatteryLevel::from_wire(response.battery_level),
            volts: response.battery_volts,
        })
    }

    async fn protocol_version(
        &self,
        client_version: i64,
        min_host_version: i64,
    ) -> Result<ProtocolVerdict, ConnError> {
        let response = self
            .client()
            .protocol_version(pb::ProtocolVersionRequest {
                client_version,
                min_host_version,
            })
            .await
            .map_err(|status| status_error(&status))?
            .into_inner();
        // Anything that is not SUCCESS is UNSUPPORTED, including a value the
        // enum does not name. `net_probe` reads neither field; it times the
        // round trip and discards the answer (`server.go:88-107`).
        let result = match pb::protocol_version_response::Result::try_from(response.result) {
            Ok(pb::protocol_version_response::Result::Success) => ProtocolResult::Success,
            _ => ProtocolResult::Unsupported,
        };
        // The one place the verdict is visible. `deviations.md` lists whether
        // this robot answers `SUCCESS` or `UNSUPPORTED` to
        // `client_version = 5, min_host_version = 0` as an open question, and
        // nothing can answer it from a response body, because every caller
        // discards the verdict on purpose. So it is logged. This is the line
        // `RUNBOOK-SDK-TRIAL.md` tells the operator to read, and the reason the
        // trial binary turns this crate up to debug.
        //
        // The target is this module rather than the `sdkapp` the two ported Go
        // log lines use, because this is a diagnostic of the port's own and not
        // a line the Go server writes. That is also what puts it behind
        // `wirepod_vector=debug` rather than behind an `sdkapp` directive.
        tracing::debug!(
            ?result,
            host_version = response.host_version,
            "protocol version verdict, which every caller discards"
        );
        Ok(ProtocolVerdict {
            result,
            host_version: response.host_version,
        })
    }

    async fn open_event_stream(
        &self,
        whitelist: &[&str],
        connection_id: &str,
    ) -> Result<Box<dyn EventReceiver>, ConnError> {
        // The shape `begin_event_stream` sends: a whitelist filter and the
        // connection id (`server.go:485-492`).
        let request = pb::EventRequest {
            connection_id: connection_id.to_owned(),
            list_type: Some(pb::event_request::ListType::WhiteList(pb::FilterList {
                list: whitelist.iter().map(|name| (*name).to_owned()).collect(),
            })),
        };
        let stream = self
            .client()
            .event_stream(request)
            .await
            .map_err(|status| status_error(&status))?
            .into_inner();
        Ok(Box::new(TonicEventReceiver::new(stream)))
    }

    async fn open_camera_feed(&self) -> Result<Box<dyn FrameStream>, ConnError> {
        let stream = self
            .client()
            .camera_feed(pb::CameraFeedRequest {})
            .await
            .map_err(|status| status_error(&status))?
            .into_inner();
        Ok(Box::new(TonicFrameStream::new(stream)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_credential_is_the_literal_go_sdk_header() {
        let auth = BearerAuth::new("abc123").expect("valid guid");
        assert_eq!(auth.value.to_str().expect("ascii"), "Bearer abc123");
    }

    #[test]
    fn a_guid_that_cannot_be_a_header_value_is_rejected() {
        let err = BearerAuth::new("bad\nvalue").expect_err("newline is not header-safe");
        assert_eq!(err.code, StatusCode::Unauthenticated);
    }
}
