//! An in-process fake robot, behind the non-default `test-util` feature.
//!
//! It serves the real generated `ExternalInterface` service on `127.0.0.1:0`,
//! so a test exercises the actual tonic client, the actual codec and the actual
//! metadata path rather than a hand-written stand in. Binding an ephemeral
//! loopback port and never `0.0.0.0` is what keeps Windows Defender from
//! prompting.
//!
//! Two transports. [`spawn_fake_robot`] serves plaintext HTTP/2, which is what
//! most tests want because it isolates the client from the handshake.
//! [`spawn_fake_robot_tls`] serves TLS with the vendored escape-pod
//! certificate, which no root store trusts, and is what proves the production
//! dialler in [`crate::tls`] actually completes a handshake and negotiates
//! `h2`.
//!
//! It lives in `src/` rather than in `tests/` because an integration test
//! binary is not importable from another crate and `wirepod-server`'s tests
//! need this too. Consumers list `wirepod-vector` a second time as a
//! dev-dependency with `features = ["test-util"]`.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::Stream;
use tokio_stream::wrappers::{TcpListenerStream, UnboundedReceiverStream};
use tonic::transport::Server;
use tonic::transport::server::Connected;
use tonic::{Request, Response, Status};
use wirepod_core::ProtocolResult;
use wirepod_core::robot::navmap::NavMapFrame;
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_proto::anki::vector::external_interface::external_interface_server::{
    ExternalInterface, ExternalInterfaceServer,
};

/// The response stream type every unimplemented streaming RPC declares.
type StubStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

/// One RPC the fake answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedCall {
    /// The gRPC method name, as it appears on the wire.
    pub method: &'static str,
    /// The `authorization` metadata the request carried, if any.
    pub authorization: Option<String>,
}

/// The `EventRequest` fields the slice cares about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordedEventRequest {
    /// The whitelist filter, or `None` when the request sent something else.
    pub whitelist: Option<Vec<String>>,
    /// The connection id the stream was tagged with.
    pub connection_id: String,
}

/// The `ProtocolVersionRequest` fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecordedProtocolRequest {
    /// The version the client claimed to speak.
    pub client_version: i64,
    /// The lowest version the client said it accepts.
    pub min_host_version: i64,
}

/// The `PullJdocsRequest` fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordedJdocsRequest {
    /// The `JdocType` wire numbers the request asked for, in order. Numbers
    /// rather than a domain enum, so a test can see a value the enum does not
    /// name as easily as one it does.
    pub jdoc_types: Vec<i32>,
}

/// One entry of a scripted `PullJdocs` answer.
///
/// The fields are the wire message's (`settings.proto:114-117`) rather than the
/// seam's, because the two shapes a robot can send that the seam refuses, an
/// answer with no entries and an entry with no document, only exist on the
/// wire.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptedJdoc {
    /// The `JdocType` wire number this entry is tagged with.
    pub jdoc_type: i32,
    /// The document, or `None` for an entry whose `doc` field is absent, which
    /// is the nil pointer Go dereferences (`sdkapp/jdocspinger.go:122`).
    pub doc: Option<ScriptedDoc>,
}

/// The four fields of a scripted document, which are the four `pingJdocs`
/// copies into `vars.AJdoc` (`sdkapp/jdocspinger.go:122-125`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptedDoc {
    /// `doc_version`.
    pub doc_version: u64,
    /// `fmt_version`.
    pub fmt_version: u64,
    /// `client_metadata`.
    pub client_metadata: String,
    /// `json_doc`.
    pub json_doc: String,
}

struct FakeState {
    calls: Mutex<Vec<RecordedCall>>,
    battery: Mutex<Result<pb::BatteryStateResponse, Status>>,
    protocol: Mutex<(ProtocolResult, i64)>,
    last_protocol: Mutex<Option<RecordedProtocolRequest>>,
    last_event: Mutex<Option<RecordedEventRequest>>,
    jdocs: Mutex<Result<Vec<pb::NamedJdoc>, Status>>,
    last_jdocs: Mutex<Option<RecordedJdocsRequest>>,
    events: Mutex<Option<mpsc::UnboundedReceiver<Result<pb::EventResponse, Status>>>>,
    frames: Mutex<Option<mpsc::UnboundedReceiver<Result<pb::CameraFeedResponse, Status>>>>,
    nav_maps: Mutex<Option<mpsc::UnboundedReceiver<Result<pb::NavMapFeedResponse, Status>>>>,
    last_nav_map: Mutex<Option<f32>>,
    enables: Mutex<Vec<bool>>,
}

fn lock<T>(cell: &Mutex<T>) -> MutexGuard<'_, T> {
    cell.lock().expect("fake robot mutex poisoned")
}

/// Every RPC the fake does not implement, expanded into a body that answers
/// `Unimplemented`.
///
/// `ExternalInterface` has 88 methods and the seam uses seven of them. Writing
/// the other 81 out by hand would bury the seven that matter.
///
/// The bodies are hand-desugared rather than written as `async fn`, which is
/// decision D8's documented fallback. `#[async_trait]` on the impl block runs
/// before this macro expands, so it never sees these methods and cannot rewrite
/// them; emitting the boxed future directly is what makes them match the
/// generated trait.
macro_rules! unimplemented_rpcs {
    (
        unary { $($u_name:ident($u_req:ty) -> $u_resp:ty;)* }
        streaming { $($s_name:ident($s_req:ty) -> $s_assoc:ident = $s_item:ty;)* }
    ) => {
        $(
            fn $u_name<'life, 'fut>(
                &'life self,
                request: Request<$u_req>,
            ) -> Pin<Box<dyn Future<Output = Result<Response<$u_resp>, Status>> + Send + 'fut>>
            where
                'life: 'fut,
                Self: 'fut,
            {
                self.record(stringify!($u_name), &request);
                Box::pin(async { Ok(Response::new(<$u_resp>::default())) })
            }
        )*
        $(
            type $s_assoc = StubStream<$s_item>;

            fn $s_name<'life, 'fut>(
                &'life self,
                _request: Request<$s_req>,
            ) -> Pin<
                Box<dyn Future<Output = Result<Response<Self::$s_assoc>, Status>> + Send + 'fut>,
            >
            where
                'life: 'fut,
                Self: 'fut,
            {
                Box::pin(async { Err(Status::unimplemented(stringify!($s_name))) })
            }
        )*
    };
}

struct FakeRobot {
    state: Arc<FakeState>,
}

impl FakeRobot {
    fn record<T>(&self, method: &'static str, request: &Request<T>) {
        let authorization = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        lock(&self.state.calls).push(RecordedCall {
            method,
            authorization,
        });
    }
}

#[tonic::async_trait]
impl ExternalInterface for FakeRobot {
    type EventStreamStream = UnboundedReceiverStream<Result<pb::EventResponse, Status>>;
    type CameraFeedStream = UnboundedReceiverStream<Result<pb::CameraFeedResponse, Status>>;
    type BehaviorControlStream =
        UnboundedReceiverStream<Result<pb::BehaviorControlResponse, Status>>;
    type NavMapFeedStream = UnboundedReceiverStream<Result<pb::NavMapFeedResponse, Status>>;

    async fn battery_state(
        &self,
        request: Request<pb::BatteryStateRequest>,
    ) -> Result<Response<pb::BatteryStateResponse>, Status> {
        self.record("BatteryState", &request);
        lock(&self.state.battery).clone().map(Response::new)
    }

    async fn protocol_version(
        &self,
        request: Request<pb::ProtocolVersionRequest>,
    ) -> Result<Response<pb::ProtocolVersionResponse>, Status> {
        self.record("ProtocolVersion", &request);
        let message = request.into_inner();
        *lock(&self.state.last_protocol) = Some(RecordedProtocolRequest {
            client_version: message.client_version,
            min_host_version: message.min_host_version,
        });
        let (result, host_version) = *lock(&self.state.protocol);
        let result = match result {
            ProtocolResult::Success => pb::protocol_version_response::Result::Success,
            ProtocolResult::Unsupported => pb::protocol_version_response::Result::Unsupported,
        };
        Ok(Response::new(pb::ProtocolVersionResponse {
            result: result as i32,
            host_version,
        }))
    }

    async fn event_stream(
        &self,
        request: Request<pb::EventRequest>,
    ) -> Result<Response<Self::EventStreamStream>, Status> {
        self.record("EventStream", &request);
        let message = request.into_inner();
        let whitelist = match message.list_type {
            Some(pb::event_request::ListType::WhiteList(filter)) => Some(filter.list),
            _ => None,
        };
        *lock(&self.state.last_event) = Some(RecordedEventRequest {
            whitelist,
            connection_id: message.connection_id,
        });
        let receiver = lock(&self.state.events)
            .take()
            .ok_or_else(|| Status::resource_exhausted("event stream already taken"))?;
        Ok(Response::new(UnboundedReceiverStream::new(receiver)))
    }

    async fn camera_feed(
        &self,
        request: Request<pb::CameraFeedRequest>,
    ) -> Result<Response<Self::CameraFeedStream>, Status> {
        self.record("CameraFeed", &request);
        let receiver = lock(&self.state.frames)
            .take()
            .ok_or_else(|| Status::resource_exhausted("camera feed already taken"))?;
        Ok(Response::new(UnboundedReceiverStream::new(receiver)))
    }

    async fn nav_map_feed(
        &self,
        request: Request<pb::NavMapFeedRequest>,
    ) -> Result<Response<Self::NavMapFeedStream>, Status> {
        self.record("NavMapFeed", &request);
        *lock(&self.state.last_nav_map) = Some(request.into_inner().frequency);
        let receiver = lock(&self.state.nav_maps)
            .take()
            .ok_or_else(|| Status::resource_exhausted("nav map feed already taken"))?;
        Ok(Response::new(UnboundedReceiverStream::new(receiver)))
    }

    async fn pull_jdocs(
        &self,
        request: Request<pb::PullJdocsRequest>,
    ) -> Result<Response<pb::PullJdocsResponse>, Status> {
        self.record("PullJdocs", &request);
        *lock(&self.state.last_jdocs) = Some(RecordedJdocsRequest {
            jdoc_types: request.into_inner().jdoc_types,
        });
        let named_jdocs = lock(&self.state.jdocs).clone()?;
        // `status` is the `ResponseStatus` every SDK response carries and no
        // caller in the slice reads.
        Ok(Response::new(pb::PullJdocsResponse {
            status: None,
            named_jdocs,
        }))
    }

    /// Grants control to every `ControlRequest` the caller sends, so the
    /// go-home path and `bcassume` can be driven end to end.
    async fn behavior_control(
        &self,
        request: Request<tonic::Streaming<pb::BehaviorControlRequest>>,
    ) -> Result<Response<Self::BehaviorControlStream>, Status> {
        self.record("BehaviorControl", &request);
        let mut incoming = request.into_inner();
        let (sender, receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(Some(message)) = incoming.message().await {
                let requested = matches!(
                    message.request_type,
                    Some(pb::behavior_control_request::RequestType::ControlRequest(_))
                );
                if requested {
                    let granted = pb::BehaviorControlResponse {
                        response_type: Some(
                            pb::behavior_control_response::ResponseType::ControlGrantedResponse(
                                pb::ControlGrantedResponse {},
                            ),
                        ),
                    };
                    if sender.send(Ok(granted)).is_err() {
                        return;
                    }
                }
            }
        });
        Ok(Response::new(UnboundedReceiverStream::new(receiver)))
    }

    async fn enable_image_streaming(
        &self,
        request: Request<pb::EnableImageStreamingRequest>,
    ) -> Result<Response<pb::EnableImageStreamingResponse>, Status> {
        self.record("EnableImageStreaming", &request);
        lock(&self.state.enables).push(request.into_inner().enable);
        Ok(Response::new(pb::EnableImageStreamingResponse {
            status: None,
        }))
    }

    unimplemented_rpcs! {
        unary {
            sdk_initialization(pb::SdkInitializationRequest) -> pb::SdkInitializationResponse;
            drive_wheels(pb::DriveWheelsRequest) -> pb::DriveWheelsResponse;
            play_animation_trigger(pb::PlayAnimationTriggerRequest) -> pb::PlayAnimationResponse;
            play_animation(pb::PlayAnimationRequest) -> pb::PlayAnimationResponse;
            list_animations(pb::ListAnimationsRequest) -> pb::ListAnimationsResponse;
            list_animation_triggers(pb::ListAnimationTriggersRequest) -> pb::ListAnimationTriggersResponse;
            move_head(pb::MoveHeadRequest) -> pb::MoveHeadResponse;
            move_lift(pb::MoveLiftRequest) -> pb::MoveLiftResponse;
            stop_all_motors(pb::StopAllMotorsRequest) -> pb::StopAllMotorsResponse;
            display_face_image_rgb(pb::DisplayFaceImageRgbRequest) -> pb::DisplayFaceImageRgbResponse;
            cancel_face_enrollment(pb::CancelFaceEnrollmentRequest) -> pb::CancelFaceEnrollmentResponse;
            request_enrolled_names(pb::RequestEnrolledNamesRequest) -> pb::RequestEnrolledNamesResponse;
            update_enrolled_face_by_id(pb::UpdateEnrolledFaceByIdRequest) -> pb::UpdateEnrolledFaceByIdResponse;
            erase_enrolled_face_by_id(pb::EraseEnrolledFaceByIdRequest) -> pb::EraseEnrolledFaceByIdResponse;
            erase_all_enrolled_faces(pb::EraseAllEnrolledFacesRequest) -> pb::EraseAllEnrolledFacesResponse;
            set_face_to_enroll(pb::SetFaceToEnrollRequest) -> pb::SetFaceToEnrollResponse;
            enroll_face(pb::EnrollFaceRequest) -> pb::EnrollFaceResponse;
            enable_marker_detection(pb::EnableMarkerDetectionRequest) -> pb::EnableMarkerDetectionResponse;
            enable_face_detection(pb::EnableFaceDetectionRequest) -> pb::EnableFaceDetectionResponse;
            enable_motion_detection(pb::EnableMotionDetectionRequest) -> pb::EnableMotionDetectionResponse;
            enable_mirror_mode(pb::EnableMirrorModeRequest) -> pb::EnableMirrorModeResponse;
            is_image_streaming_enabled(pb::IsImageStreamingEnabledRequest) -> pb::IsImageStreamingEnabledResponse;
            cancel_action_by_id_tag(pb::CancelActionByIdTagRequest) -> pb::CancelActionByIdTagResponse;
            cancel_behavior(pb::CancelBehaviorRequest) -> pb::CancelBehaviorResponse;
            go_to_pose(pb::GoToPoseRequest) -> pb::GoToPoseResponse;
            dock_with_cube(pb::DockWithCubeRequest) -> pb::DockWithCubeResponse;
            drive_off_charger(pb::DriveOffChargerRequest) -> pb::DriveOffChargerResponse;
            drive_on_charger(pb::DriveOnChargerRequest) -> pb::DriveOnChargerResponse;
            find_faces(pb::FindFacesRequest) -> pb::FindFacesResponse;
            look_around_in_place(pb::LookAroundInPlaceRequest) -> pb::LookAroundInPlaceResponse;
            roll_block(pb::RollBlockRequest) -> pb::RollBlockResponse;
            photos_info(pb::PhotosInfoRequest) -> pb::PhotosInfoResponse;
            photo(pb::PhotoRequest) -> pb::PhotoResponse;
            thumbnail(pb::ThumbnailRequest) -> pb::ThumbnailResponse;
            delete_photo(pb::DeletePhotoRequest) -> pb::DeletePhotoResponse;
            drive_straight(pb::DriveStraightRequest) -> pb::DriveStraightResponse;
            turn_in_place(pb::TurnInPlaceRequest) -> pb::TurnInPlaceResponse;
            set_head_angle(pb::SetHeadAngleRequest) -> pb::SetHeadAngleResponse;
            set_lift_height(pb::SetLiftHeightRequest) -> pb::SetLiftHeightResponse;
            turn_towards_face(pb::TurnTowardsFaceRequest) -> pb::TurnTowardsFaceResponse;
            go_to_object(pb::GoToObjectRequest) -> pb::GoToObjectResponse;
            roll_object(pb::RollObjectRequest) -> pb::RollObjectResponse;
            pop_a_wheelie(pb::PopAWheelieRequest) -> pb::PopAWheelieResponse;
            pickup_object(pb::PickupObjectRequest) -> pb::PickupObjectResponse;
            place_object_on_ground_here(pb::PlaceObjectOnGroundHereRequest) -> pb::PlaceObjectOnGroundHereResponse;
            set_master_volume(pb::MasterVolumeRequest) -> pb::MasterVolumeResponse;
            user_authentication(pb::UserAuthenticationRequest) -> pb::UserAuthenticationResponse;
            version_state(pb::VersionStateRequest) -> pb::VersionStateResponse;
            say_text(pb::SayTextRequest) -> pb::SayTextResponse;
            connect_cube(pb::ConnectCubeRequest) -> pb::ConnectCubeResponse;
            disconnect_cube(pb::DisconnectCubeRequest) -> pb::DisconnectCubeResponse;
            cubes_available(pb::CubesAvailableRequest) -> pb::CubesAvailableResponse;
            flash_cube_lights(pb::FlashCubeLightsRequest) -> pb::FlashCubeLightsResponse;
            forget_preferred_cube(pb::ForgetPreferredCubeRequest) -> pb::ForgetPreferredCubeResponse;
            set_preferred_cube(pb::SetPreferredCubeRequest) -> pb::SetPreferredCubeResponse;
            delete_custom_objects(pb::DeleteCustomObjectsRequest) -> pb::DeleteCustomObjectsResponse;
            create_fixed_custom_object(pb::CreateFixedCustomObjectRequest) -> pb::CreateFixedCustomObjectResponse;
            define_custom_object(pb::DefineCustomObjectRequest) -> pb::DefineCustomObjectResponse;
            set_cube_lights(pb::SetCubeLightsRequest) -> pb::SetCubeLightsResponse;
            capture_single_image(pb::CaptureSingleImageRequest) -> pb::CaptureSingleImageResponse;
            set_eye_color(pb::SetEyeColorRequest) -> pb::SetEyeColorResponse;
            app_intent(pb::AppIntentRequest) -> pb::AppIntentResponse;
            get_onboarding_state(pb::OnboardingStateRequest) -> pb::OnboardingStateResponse;
            send_onboarding_input(pb::OnboardingInputRequest) -> pb::OnboardingInputResponse;
            get_camera_config(pb::CameraConfigRequest) -> pb::CameraConfigResponse;
            set_camera_settings(pb::SetCameraSettingsRequest) -> pb::SetCameraSettingsResponse;
            get_latest_attention_transfer(pb::LatestAttentionTransferRequest) -> pb::LatestAttentionTransferResponse;
            update_settings(pb::UpdateSettingsRequest) -> pb::UpdateSettingsResponse;
            update_account_settings(pb::UpdateAccountSettingsRequest) -> pb::UpdateAccountSettingsResponse;
            start_update_engine(pb::CheckUpdateStatusRequest) -> pb::CheckUpdateStatusResponse;
            check_update_status(pb::CheckUpdateStatusRequest) -> pb::CheckUpdateStatusResponse;
            update_and_restart(pb::UpdateAndRestartRequest) -> pb::UpdateAndRestartResponse;
            check_cloud_connection(pb::CheckCloudRequest) -> pb::CheckCloudResponse;
            get_feature_flag(pb::FeatureFlagRequest) -> pb::FeatureFlagResponse;
            get_feature_flag_list(pb::FeatureFlagListRequest) -> pb::FeatureFlagListResponse;
            get_alexa_auth_state(pb::AlexaAuthStateRequest) -> pb::AlexaAuthStateResponse;
            alexa_opt_in(pb::AlexaOptInRequest) -> pb::AlexaOptInResponse;
        }
        streaming {
            external_audio_stream_playback(tonic::Streaming<pb::ExternalAudioStreamRequest>)
                -> ExternalAudioStreamPlaybackStream = pb::ExternalAudioStreamResponse;
            assume_behavior_control(pb::BehaviorControlRequest)
                -> AssumeBehaviorControlStream = pb::BehaviorControlResponse;
            audio_feed(pb::AudioFeedRequest) -> AudioFeedStream = pb::AudioFeedResponse;
        }
    }
}

/// Drives a running fake robot: scripts its answers, feeds its streams and
/// reads back what it saw.
pub struct FakeRobotHandle {
    state: Arc<FakeState>,
    events: Mutex<Option<mpsc::UnboundedSender<Result<pb::EventResponse, Status>>>>,
    frames: Mutex<Option<mpsc::UnboundedSender<Result<pb::CameraFeedResponse, Status>>>>,
    nav_maps: Mutex<Option<mpsc::UnboundedSender<Result<pb::NavMapFeedResponse, Status>>>>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    served: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The TLS accept loop, which the plaintext fake does not have. It owns the
    /// listener, so it has to be aborted rather than signalled: the server's
    /// own shutdown only stops it reading the stream it was given.
    accepting: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl FakeRobotHandle {
    /// Every RPC the fake has answered, in order, with the `authorization`
    /// metadata each one carried.
    pub fn calls(&self) -> Vec<RecordedCall> {
        lock(&self.state.calls).clone()
    }

    /// The method names of every RPC answered so far.
    pub fn methods(&self) -> Vec<&'static str> {
        lock(&self.state.calls)
            .iter()
            .map(|call| call.method)
            .collect()
    }

    /// The last `EventStream` request the fake received.
    pub fn last_event_request(&self) -> Option<RecordedEventRequest> {
        lock(&self.state.last_event).clone()
    }

    /// The last `ProtocolVersion` request the fake received.
    pub fn last_protocol_request(&self) -> Option<RecordedProtocolRequest> {
        *lock(&self.state.last_protocol)
    }

    /// The last `PullJdocs` request the fake received.
    pub fn last_jdocs_request(&self) -> Option<RecordedJdocsRequest> {
        lock(&self.state.last_jdocs).clone()
    }

    /// Every `EnableImageStreaming` flag the fake received, in order.
    pub fn image_streaming_calls(&self) -> Vec<bool> {
        lock(&self.state.enables).clone()
    }

    /// Scripts what `ProtocolVersion` answers. The default is `Unsupported`, so
    /// a caller that reads the verdict where Go does not is visible.
    pub fn set_protocol_verdict(&self, result: ProtocolResult, host_version: i64) {
        *lock(&self.state.protocol) = (result, host_version);
    }

    /// Scripts what `BatteryState` answers.
    pub fn set_battery(&self, level: i32, volts: f32) {
        *lock(&self.state.battery) = Ok(pb::BatteryStateResponse {
            battery_level: level,
            battery_volts: volts,
            ..pb::BatteryStateResponse::default()
        });
    }

    /// Scripts the two charger flags of what `BatteryState` answers.
    pub fn set_charger(&self, is_charging: bool, is_on_charger_platform: bool) {
        if let Ok(response) = lock(&self.state.battery).as_mut() {
            response.is_charging = is_charging;
            response.is_on_charger_platform = is_on_charger_platform;
        }
    }

    /// Makes `BatteryState` fail with `status`.
    pub fn fail_battery(&self, status: Status) {
        *lock(&self.state.battery) = Err(status);
    }

    /// Scripts what `PullJdocs` answers.
    ///
    /// The default is an answer carrying no entries, which is the shape Go's
    /// unchecked index panics on, so a test that wants a usable answer has to
    /// say so.
    pub fn set_jdocs(&self, jdocs: Vec<ScriptedJdoc>) {
        *lock(&self.state.jdocs) = Ok(jdocs.into_iter().map(named_jdoc).collect());
    }

    /// Makes `PullJdocs` fail with `status`.
    pub fn fail_jdocs(&self, status: Status) {
        *lock(&self.state.jdocs) = Err(status);
    }

    /// Pushes a stimulation event into the open event stream.
    pub fn push_stim(&self, value: f32, velocity: f32) {
        self.push_event(pb::EventResponse {
            status: None,
            event: Some(pb::Event {
                event_type: Some(pb::event::EventType::StimulationInfo(pb::StimulationInfo {
                    value,
                    velocity,
                    ..pb::StimulationInfo::default()
                })),
            }),
        });
    }

    /// Pushes an event of an unrelated type into the open event stream.
    pub fn push_other_event(&self) {
        self.push_event(pb::EventResponse {
            status: None,
            event: Some(pb::Event {
                event_type: Some(pb::event::EventType::KeepAlive(pb::KeepAlivePing {})),
            }),
        });
    }

    fn push_event(&self, response: pb::EventResponse) {
        if let Some(sender) = lock(&self.events).as_ref() {
            let _ = sender.send(Ok(response));
        }
    }

    /// Ends the event stream cleanly.
    pub fn end_event_stream(&self) {
        lock(&self.events).take();
    }

    /// Pushes one camera frame into the open feed.
    pub fn push_frame(&self, data: Vec<u8>) {
        if let Some(sender) = lock(&self.frames).as_ref() {
            let _ = sender.send(Ok(pb::CameraFeedResponse {
                data,
                ..pb::CameraFeedResponse::default()
            }));
        }
    }

    /// Ends the camera feed cleanly.
    pub fn end_camera_feed(&self) {
        lock(&self.frames).take();
    }

    /// The `frequency` the last `NavMapFeed` request carried, which the robot
    /// reads as a period in seconds.
    pub fn last_nav_map_period(&self) -> Option<f32> {
        *lock(&self.state.last_nav_map)
    }

    /// Pushes one map into the open nav map feed, with the zero
    /// `root_center_z` the robot's gateway always sends.
    pub fn push_nav_map(&self, frame: &NavMapFrame) {
        if let Some(sender) = lock(&self.nav_maps).as_ref() {
            let _ = sender.send(Ok(pb::NavMapFeedResponse {
                origin_id: frame.origin_id,
                map_info: Some(pb::NavMapInfo {
                    root_depth: frame.info.root_depth,
                    root_size_mm: frame.info.root_size_mm,
                    root_center_x: frame.info.root_center_x,
                    root_center_y: frame.info.root_center_y,
                    root_center_z: 0.0,
                }),
                quad_infos: frame
                    .quads
                    .iter()
                    .map(|quad| pb::NavMapQuadInfo {
                        content: quad.content,
                        depth: quad.depth,
                        color_rgba: quad.rgba,
                    })
                    .collect(),
            }));
        }
    }

    /// Ends the nav map feed cleanly.
    pub fn end_nav_map_feed(&self) {
        lock(&self.nav_maps).take();
    }

    /// Stops the server and waits for it to finish.
    pub async fn shutdown(&self) {
        if let Some(accepting) = lock(&self.accepting).take() {
            accepting.abort();
        }
        if let Some(sender) = lock(&self.shutdown).take() {
            let _ = sender.send(());
        }
        let served = lock(&self.served).take();
        if let Some(served) = served {
            let _ = served.await;
        }
    }
}

/// One scripted entry as the wire message the fake sends.
fn named_jdoc(entry: ScriptedJdoc) -> pb::NamedJdoc {
    pb::NamedJdoc {
        jdoc_type: entry.jdoc_type,
        doc: entry.doc.map(|doc| pb::Jdoc {
            doc_version: doc.doc_version,
            fmt_version: doc.fmt_version,
            client_metadata: doc.client_metadata,
            json_doc: doc.json_doc,
        }),
    }
}

/// The escape-pod certificate the TLS fake serves.
///
/// It is the vendored `assets/epod/ep.crt`, byte-identical to the Go server's,
/// which is self-signed and therefore trusted by no root store on any machine.
/// That is exactly the property the TLS tests need: a handshake that completes
/// against it proves the accept-all verifier is doing the accepting.
const EPOD_CERT: &[u8] = include_bytes!("../../../assets/epod/ep.crt");

/// The private key for [`EPOD_CERT`], vendored as `assets/epod/ep.key`.
const EPOD_KEY: &[u8] = include_bytes!("../../../assets/epod/ep.key");

/// The TLS versions a fake offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsVersions {
    /// TLS 1.2 and TLS 1.3, which is what a default rustls server offers and
    /// what a modern peer settles on as 1.3.
    Both,
    /// TLS 1.2 only. grpc-go raises `MinVersion` to TLS 1.2 and stops there
    /// (`google.golang.org/grpc@v1.82.1/credentials/tls.go:243-245`), and
    /// Vector's gateway is old, so a client that could not speak 1.2 would fail
    /// against real hardware while passing every TLS 1.3 test.
    Tls12Only,
}

/// The certificate chain the TLS fake presents, in DER.
///
/// Exported so a test can assert that the chain the client accepted is this
/// one, rather than merely that some handshake completed. Parsing lives here
/// because `rustls-pemfile` is an optional dependency this feature turns on.
pub fn epod_certificate_chain() -> Vec<rustls::pki_types::CertificateDer<'static>> {
    rustls_pemfile::certs(&mut &EPOD_CERT[..])
        .collect::<Result<Vec<_>, _>>()
        .expect("parse the escape-pod certificate")
}

/// The rustls server configuration the TLS fake serves.
fn epod_server_config(versions: TlsVersions) -> Arc<rustls::ServerConfig> {
    let certs = epod_certificate_chain();
    let key = rustls_pemfile::private_key(&mut &EPOD_KEY[..])
        .expect("read the escape-pod key")
        .expect("the escape-pod key file holds a private key");
    // The provider is named rather than installed as the process default, for
    // the same reason `crate::tls` names it: a test binary runs many tests in
    // one process and installing a global from one of them is a race.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider);
    let builder = match versions {
        TlsVersions::Both => builder.with_safe_default_protocol_versions(),
        TlsVersions::Tls12Only => builder.with_protocol_versions(&[&rustls::version::TLS12]),
    }
    .expect("the ring provider supports the requested protocol versions");
    let mut config = builder
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("the escape-pod certificate and key agree");
    config.alpn_protocols = vec![b"h2".to_vec()];
    Arc::new(config)
}

/// A handshaken TLS stream, wrapped so tonic will serve it.
///
/// tonic implements `Connected` for `tokio_rustls::server::TlsStream` only
/// under its own `tls` feature, which this workspace cannot enable, so the
/// newtype supplies the one impl that is missing. The connect info is `()`
/// because nothing in the slice reads it.
struct TlsIo(tokio_rustls::server::TlsStream<TcpStream>);

impl Connected for TlsIo {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl AsyncRead for TlsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for TlsIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// The scripted state and the two stream senders a fresh fake starts with.
struct FakeParts {
    state: Arc<FakeState>,
    events: mpsc::UnboundedSender<Result<pb::EventResponse, Status>>,
    frames: mpsc::UnboundedSender<Result<pb::CameraFeedResponse, Status>>,
    nav_maps: mpsc::UnboundedSender<Result<pb::NavMapFeedResponse, Status>>,
}

fn new_fake() -> FakeParts {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (frame_tx, frame_rx) = mpsc::unbounded_channel();
    let (nav_map_tx, nav_map_rx) = mpsc::unbounded_channel();
    FakeParts {
        state: Arc::new(FakeState {
            calls: Mutex::new(Vec::new()),
            battery: Mutex::new(Ok(pb::BatteryStateResponse::default())),
            protocol: Mutex::new((ProtocolResult::Unsupported, 0)),
            last_protocol: Mutex::new(None),
            last_event: Mutex::new(None),
            // The empty answer is the default, because it is the one a robot
            // can send that Go's unchecked index panics on
            // (`sdkapp/jdocspinger.go:122`).
            jdocs: Mutex::new(Ok(Vec::new())),
            last_jdocs: Mutex::new(None),
            events: Mutex::new(Some(event_rx)),
            frames: Mutex::new(Some(frame_rx)),
            nav_maps: Mutex::new(Some(nav_map_rx)),
            last_nav_map: Mutex::new(None),
            enables: Mutex::new(Vec::new()),
        }),
        events: event_tx,
        frames: frame_tx,
        nav_maps: nav_map_tx,
    }
}

/// Binds the one address every fake uses.
///
/// `127.0.0.1:0` and never `0.0.0.0`: an ephemeral loopback port raises no
/// Windows Defender prompt.
async fn bind_loopback() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("loopback address");
    (listener, addr)
}

/// The `ExternalInterface` service a fake serves.
fn fake_service(state: &Arc<FakeState>) -> ExternalInterfaceServer<FakeRobot> {
    ExternalInterfaceServer::new(FakeRobot {
        state: Arc::clone(state),
    })
}

/// Starts a fake robot on an ephemeral loopback port, speaking plaintext
/// HTTP/2.
///
/// Returns the address it bound and the handle that drives it. The caller
/// should `shutdown` the handle, though dropping it is safe: the task ends when
/// the runtime does.
pub async fn spawn_fake_robot() -> (SocketAddr, FakeRobotHandle) {
    let parts = new_fake();
    let (listener, addr) = bind_loopback().await;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let service = fake_service(&parts.state);
    let served = tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let handle = FakeRobotHandle {
        state: parts.state,
        events: Mutex::new(Some(parts.events)),
        frames: Mutex::new(Some(parts.frames)),
        nav_maps: Mutex::new(Some(parts.nav_maps)),
        shutdown: Mutex::new(Some(shutdown_tx)),
        served: Mutex::new(Some(served)),
        accepting: Mutex::new(None),
    };
    (addr, handle)
}

/// Starts a fake robot on an ephemeral loopback port, speaking TLS with the
/// escape-pod certificate and offering `h2`.
///
/// The versions are [`TlsVersions::Both`], so a modern client settles on TLS
/// 1.3. Use [`spawn_fake_robot_tls_with`] to pin 1.2.
pub async fn spawn_fake_robot_tls() -> (SocketAddr, FakeRobotHandle) {
    spawn_fake_robot_tls_with(TlsVersions::Both).await
}

/// [`spawn_fake_robot_tls`] with the offered TLS versions chosen.
pub async fn spawn_fake_robot_tls_with(versions: TlsVersions) -> (SocketAddr, FakeRobotHandle) {
    let parts = new_fake();
    let (listener, addr) = bind_loopback().await;
    let acceptor = tokio_rustls::TlsAcceptor::from(epod_server_config(versions));

    // The handshake happens in the accept loop rather than inside tonic, so the
    // stream handed to `serve_with_incoming_shutdown` carries only connections
    // that already completed one. Each handshake gets its own task, so a client
    // that opens a socket and then stalls cannot hold up the next one, and a
    // handshake that fails is dropped rather than surfaced: a test that cared
    // would see it as a dial error on the client side instead.
    let (conn_tx, conn_rx) = mpsc::unbounded_channel::<io::Result<TlsIo>>();
    let accepting = tokio::spawn(async move {
        while let Ok((tcp, _peer)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let conn_tx = conn_tx.clone();
            tokio::spawn(async move {
                if let Ok(stream) = acceptor.accept(tcp).await {
                    let _ = conn_tx.send(Ok(TlsIo(stream)));
                }
            });
        }
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let service = fake_service(&parts.state);
    let served = tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(UnboundedReceiverStream::new(conn_rx), async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let handle = FakeRobotHandle {
        state: parts.state,
        events: Mutex::new(Some(parts.events)),
        frames: Mutex::new(Some(parts.frames)),
        nav_maps: Mutex::new(Some(parts.nav_maps)),
        shutdown: Mutex::new(Some(shutdown_tx)),
        served: Mutex::new(Some(served)),
        accepting: Mutex::new(Some(accepting)),
    };
    (addr, handle)
}
