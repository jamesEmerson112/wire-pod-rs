//! An in-process fake robot, behind the non-default `test-util` feature.
//!
//! It serves the real generated `ExternalInterface` service over plaintext
//! HTTP/2 on `127.0.0.1:0`, so a test exercises the actual tonic client, the
//! actual codec and the actual metadata path rather than a hand-written stand
//! in. Binding an ephemeral loopback port and never `0.0.0.0` is what keeps
//! Windows Defender from prompting.
//!
//! It lives in `src/` rather than in `tests/` because an integration test
//! binary is not importable from another crate and `wirepod-server`'s tests
//! need this too. Consumers list `wirepod-vector` a second time as a
//! dev-dependency with `features = ["test-util"]`.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::Stream;
use tokio_stream::wrappers::{TcpListenerStream, UnboundedReceiverStream};
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use wirepod_core::ProtocolResult;
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

struct FakeState {
    calls: Mutex<Vec<RecordedCall>>,
    battery: Mutex<Result<pb::BatteryStateResponse, Status>>,
    protocol: Mutex<(ProtocolResult, i64)>,
    last_protocol: Mutex<Option<RecordedProtocolRequest>>,
    last_event: Mutex<Option<RecordedEventRequest>>,
    events: Mutex<Option<mpsc::UnboundedReceiver<Result<pb::EventResponse, Status>>>>,
    frames: Mutex<Option<mpsc::UnboundedReceiver<Result<pb::CameraFeedResponse, Status>>>>,
    enables: Mutex<Vec<bool>>,
}

fn lock<T>(cell: &Mutex<T>) -> MutexGuard<'_, T> {
    cell.lock().expect("fake robot mutex poisoned")
}

/// Every RPC the fake does not implement, expanded into a body that answers
/// `Unimplemented`.
///
/// `ExternalInterface` has 88 methods and the slice uses five of them. Writing
/// the other 83 out by hand would bury the five that matter.
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
                _request: Request<$u_req>,
            ) -> Pin<Box<dyn Future<Output = Result<Response<$u_resp>, Status>> + Send + 'fut>>
            where
                'life: 'fut,
                Self: 'fut,
            {
                Box::pin(async { Err(Status::unimplemented(stringify!($u_name))) })
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
            pull_jdocs(pb::PullJdocsRequest) -> pb::PullJdocsResponse;
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
            behavior_control(tonic::Streaming<pb::BehaviorControlRequest>)
                -> BehaviorControlStream = pb::BehaviorControlResponse;
            assume_behavior_control(pb::BehaviorControlRequest)
                -> AssumeBehaviorControlStream = pb::BehaviorControlResponse;
            audio_feed(pb::AudioFeedRequest) -> AudioFeedStream = pb::AudioFeedResponse;
            nav_map_feed(pb::NavMapFeedRequest) -> NavMapFeedStream = pb::NavMapFeedResponse;
        }
    }
}

/// Drives a running fake robot: scripts its answers, feeds its streams and
/// reads back what it saw.
pub struct FakeRobotHandle {
    state: Arc<FakeState>,
    events: Mutex<Option<mpsc::UnboundedSender<Result<pb::EventResponse, Status>>>>,
    frames: Mutex<Option<mpsc::UnboundedSender<Result<pb::CameraFeedResponse, Status>>>>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    served: Mutex<Option<tokio::task::JoinHandle<()>>>,
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

    /// Makes `BatteryState` fail with `status`.
    pub fn fail_battery(&self, status: Status) {
        *lock(&self.state.battery) = Err(status);
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

    /// Stops the server and waits for it to finish.
    pub async fn shutdown(&self) {
        if let Some(sender) = lock(&self.shutdown).take() {
            let _ = sender.send(());
        }
        let served = lock(&self.served).take();
        if let Some(served) = served {
            let _ = served.await;
        }
    }
}

/// Starts a fake robot on an ephemeral loopback port.
///
/// Returns the address it bound and the handle that drives it. The caller
/// should `shutdown` the handle, though dropping it is safe: the task ends when
/// the runtime does.
pub async fn spawn_fake_robot() -> (SocketAddr, FakeRobotHandle) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (frame_tx, frame_rx) = mpsc::unbounded_channel();
    let state = Arc::new(FakeState {
        calls: Mutex::new(Vec::new()),
        battery: Mutex::new(Ok(pb::BatteryStateResponse::default())),
        protocol: Mutex::new((ProtocolResult::Unsupported, 0)),
        last_protocol: Mutex::new(None),
        last_event: Mutex::new(None),
        events: Mutex::new(Some(event_rx)),
        frames: Mutex::new(Some(frame_rx)),
        enables: Mutex::new(Vec::new()),
    });

    // `127.0.0.1:0` and never `0.0.0.0`: an ephemeral loopback port raises no
    // Windows Defender prompt.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("loopback address");

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let service = ExternalInterfaceServer::new(FakeRobot {
        state: Arc::clone(&state),
    });
    let served = tokio::spawn(async move {
        let _ = Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _ = shutdown_rx.await;
            })
            .await;
    });

    let handle = FakeRobotHandle {
        state,
        events: Mutex::new(Some(event_tx)),
        frames: Mutex::new(Some(frame_tx)),
        shutdown: Mutex::new(Some(shutdown_tx)),
        served: Mutex::new(Some(served)),
    };
    (addr, handle)
}
