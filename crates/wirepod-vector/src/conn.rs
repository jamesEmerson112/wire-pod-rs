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
    BatteryLevel, BatteryReading, CameraControl, ConnError, EventReceiver, FrameStream, Jdoc,
    JdocKind, NamedJdoc, ProtocolResult, ProtocolVerdict, RobotConn, StatusCode,
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
/// The generated SDK client, carrying the bearer credential. This is what Go
/// reaches as `robot.Conn`.
pub type SdkClient = ExternalInterfaceClient<InterceptedService<Channel, BearerAuth>>;

/// The SDK client behind a connection, or `None` for a connection that is not a
/// tonic one, which only a test fake is.
pub fn sdk_client(conn: &dyn RobotConn) -> Option<SdkClient> {
    conn.as_any()
        .downcast_ref::<TonicRobotConn>()
        .map(TonicRobotConn::client)
}

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

    fn client(&self) -> SdkClient {
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
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

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
            is_charging: response.is_charging,
            is_on_charger_platform: response.is_on_charger_platform,
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

    async fn pull_jdocs(&self, kinds: &[JdocKind]) -> Result<Vec<NamedJdoc>, ConnError> {
        // The one field the request has, in the order the caller asked for. Go
        // builds the same slice with one element at each of its three call
        // sites: `ROBOT_SETTINGS` for the pinger and
        // `/api-sdk/get_sdk_settings` (`sdkapp/jdocspinger.go:112-114`,
        // `sdkapp/server.go:200-202`), and `ROBOT_LIFETIME_STATS` for
        // `/api-sdk/get_robot_stats` (`sdkapp/server.go:592-595`).
        let response = self
            .client()
            .pull_jdocs(pb::PullJdocsRequest {
                jdoc_types: kinds.iter().map(|kind| kind.as_wire()).collect(),
            })
            .await
            .map_err(|status| status_error(&status))?
            .into_inner();
        if response.named_jdocs.is_empty() {
            return Err(empty_answer());
        }
        response.named_jdocs.into_iter().map(named_jdoc).collect()
    }
}

/// The failure an answer carrying no documents produces.
///
/// Go indexes `NamedJdocs[0]` with no length check at all three call sites
/// (`sdkapp/jdocspinger.go:122`, `sdkapp/server.go:207`,
/// `sdkapp/server.go:600`), so this answer takes the Go process down with an
/// index-out-of-range panic. It becomes an error the caller logs instead, and
/// it is the empty `NamedJdocs` panic reserved deviation 31 already names.
///
/// The code is `Internal`, which is grpc-go's code for a peer that broke the
/// contract, and the whole rendering therefore reads
/// `rpc error: code = Internal desc = robot answered PullJdocs with no
/// documents` like any other robot failure.
fn empty_answer() -> ConnError {
    ConnError::new(
        StatusCode::Internal,
        "robot answered PullJdocs with no documents",
    )
}

/// One wire entry as a domain [`NamedJdoc`].
///
/// prost renders `NamedJdoc.doc` as an `Option`, because proto3 cannot tell an
/// absent message from a default one. Go's field access on the nil pointer
/// (`sdkapp/jdocspinger.go:122`) is a nil dereference, and refusing it here
/// follows the same policy as reserved deviation 31, which does not itself
/// list this panic. The alternative, treating an absent document as the
/// default one, is worse than either: `AddJdoc` would replace a good
/// `vic.RobotSettings` with an empty one and the file would lose the robot's
/// settings without anything being logged at all.
///
/// The check runs on every entry, and collecting into a `Result` means one bad
/// entry refuses the whole answer. That covers entries Go never reads: all
/// three Go sites stop at index zero (`sdkapp/jdocspinger.go:122-125`,
/// `sdkapp/server.go:207-222`, `sdkapp/server.go:600`), so an absent document
/// in a second or later entry leaves the good first one in use there and is a
/// [`ConnError`] here. No Go request asks for more than one kind, so nothing
/// in the port can reach the difference. Recorded as a candidate deviation.
fn named_jdoc(named: pb::NamedJdoc) -> Result<NamedJdoc, ConnError> {
    let kind = JdocKind::from_wire(named.jdoc_type);
    let doc = named.doc.ok_or_else(|| {
        ConnError::new(
            StatusCode::Internal,
            format!("robot answered PullJdocs with an absent {kind:?} document"),
        )
    })?;
    // The four fields `pingJdocs` copies, in its order
    // (`sdkapp/jdocspinger.go:122-125`). Both of the document's integers are
    // `uint64` on the wire (`settings.proto:107-112`) and `u64` in the store,
    // so no conversion is involved. `extra` stays empty: it exists to carry
    // keys read off the on-disk file that the struct does not name, and the
    // wire has no such keys.
    Ok(NamedJdoc {
        kind,
        doc: Jdoc {
            doc_version: doc.doc_version,
            fmt_version: doc.fmt_version,
            client_metadata: doc.client_metadata,
            json_doc: doc.json_doc,
            ..Jdoc::default()
        },
    })
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

    /// The numbers `JdocKind` carries are the generated enum's, pinned here
    /// because this is the only crate that can see both. `wirepod-core` writes
    /// them out by hand so that it can stay free of `wirepod-proto`.
    #[test]
    fn every_kind_is_the_generated_jdoc_type() {
        let pairs = [
            (JdocKind::RobotSettings, pb::JdocType::RobotSettings),
            (
                JdocKind::RobotLifetimeStats,
                pb::JdocType::RobotLifetimeStats,
            ),
            (JdocKind::AccountSettings, pb::JdocType::AccountSettings),
            (JdocKind::UserEntitlements, pb::JdocType::UserEntitlements),
        ];
        // The enum has four values and no more, so a fifth added upstream
        // fails here rather than passing silently through `from_wire`.
        assert_eq!(pairs.len(), 4);
        for (kind, generated) in pairs {
            assert_eq!(kind.as_wire(), generated as i32);
            assert_eq!(JdocKind::from_wire(generated as i32), kind);
            assert_eq!(
                pb::JdocType::try_from(kind.as_wire()),
                Ok(generated),
                "{kind:?} is not a value the generated enum names"
            );
        }
    }

    /// A number the enum does not name reads as the proto3 zero value, which
    /// is what an absent field decodes to and what no caller can observe.
    #[test]
    fn a_number_outside_the_enum_reads_as_robot_settings() {
        assert!(pb::JdocType::try_from(4).is_err());
        assert_eq!(JdocKind::from_wire(4), JdocKind::RobotSettings);
        assert_eq!(JdocKind::from_wire(-1), JdocKind::RobotSettings);
    }

    #[test]
    fn an_answer_with_no_documents_is_internal() {
        let err = empty_answer();
        assert_eq!(err.code, StatusCode::Internal);
        assert_eq!(
            err.to_string(),
            "rpc error: code = Internal desc = robot answered PullJdocs with no documents"
        );
    }

    #[test]
    fn an_entry_with_no_document_names_the_kind_it_was_tagged_with() {
        let err = named_jdoc(pb::NamedJdoc {
            jdoc_type: pb::JdocType::AccountSettings as i32,
            doc: None,
        })
        .expect_err("an absent document is not usable");
        assert_eq!(err.code, StatusCode::Internal);
        assert_eq!(
            err.to_string(),
            "rpc error: code = Internal desc = robot answered PullJdocs with an absent \
             AccountSettings document"
        );
    }

    #[test]
    fn an_entry_carries_its_four_fields_across() {
        let named = named_jdoc(pb::NamedJdoc {
            jdoc_type: pb::JdocType::RobotSettings as i32,
            doc: Some(pb::Jdoc {
                doc_version: 41,
                fmt_version: 1,
                client_metadata: "wirepod-new-token".to_owned(),
                json_doc: "{\"clock_24_hour\":true}".to_owned(),
            }),
        })
        .expect("a complete document");
        assert_eq!(named.kind, JdocKind::RobotSettings);
        // Compared whole rather than field by field, so a field that stops
        // being copied fails here. `extra` is empty because the store's
        // round-trip map carries keys read off the file and the wire has none.
        assert_eq!(
            named.doc,
            Jdoc {
                doc_version: 41,
                fmt_version: 1,
                client_metadata: "wirepod-new-token".to_owned(),
                json_doc: "{\"clock_24_hour\":true}".to_owned(),
                ..Jdoc::default()
            }
        );
    }
}
