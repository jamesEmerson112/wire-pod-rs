//! The round trip of a motion RPC, in one log line.
//!
//! Every motion call site in this workspace used to throw the robot's answer
//! away, which was faithful to Go but left no way to tell a robot that moved
//! from one that ignored us. The transport status cannot tell them apart
//! either: `docs/robot-api.md` records that `DriveWheels`, `MoveHead`,
//! `MoveLift` and `StopAllMotors` return success and do nothing when external
//! movement commands are not allowed. What distinguishes them is inside the
//! response body, so [`describe`] reads it.
//!
//! Nothing here changes what a call site does with a failure. The wrapper hands
//! the `Result` straight back, so a handler that discarded the error still
//! discards it; the line is written on the way past.

use std::future::Future;
use std::time::Instant;

use tonic::{Response, Status};
use wirepod_core::SdkSession;
use wirepod_core::robot::observe::MotionCall;
use wirepod_proto::anki::vector::external_interface as pb;

use crate::error::status_error;

/// What a motion response said once its body has been read.
///
/// Every motion response carries a `ResponseStatus`. The behaviour family adds
/// a `BehaviorResults`, and the action family adds an `ActionResult`, and in
/// both cases that second field is the one worth reading.
pub trait MotionOutcome {
    /// Whether the call is meant to move the robot. Only these open a motion
    /// window, so a call such as `SayText` never earns a "no movement" line.
    const MOVES: bool;

    /// The body, rendered for a log line.
    fn describe(&self) -> String;
}

/// `ResponseStatus.code`, by name.
///
/// The field is a message rather than a scalar, so prost makes it an `Option`
/// and an absent status is indistinguishable from `UNKNOWN`. Both read as
/// `UNKNOWN`, which is what the robot means by either.
fn response_status(status: Option<&pb::ResponseStatus>) -> &'static str {
    let code = status.map_or(0, |status| status.code);
    match pb::response_status::StatusCode::try_from(code) {
        Ok(pb::response_status::StatusCode::Unknown) => "UNKNOWN",
        Ok(pb::response_status::StatusCode::ResponseReceived) => "RESPONSE_RECEIVED",
        Ok(pb::response_status::StatusCode::RequestProcessing) => "REQUEST_PROCESSING",
        Ok(pb::response_status::StatusCode::Ok) => "OK",
        Ok(pb::response_status::StatusCode::Forbidden) => "FORBIDDEN",
        Ok(pb::response_status::StatusCode::NotFound) => "NOT_FOUND",
        Ok(pb::response_status::StatusCode::ErrorUpdateInProgress) => "ERROR_UPDATE_IN_PROGRESS",
        Err(_) => "UNRECOGNISED",
    }
}

/// `BehaviorResults`, by name.
///
/// A behaviour request is handled only while the SDK behaviour holds control,
/// so without control the call never answers at all. `WONT_ACTIVATE` is sent
/// only when control is held and the behaviour asked for refuses to start.
fn behavior_result(result: i32) -> &'static str {
    match pb::BehaviorResults::try_from(result) {
        Ok(pb::BehaviorResults::BehaviorInvalidState) => "INVALID_STATE",
        Ok(pb::BehaviorResults::BehaviorCompleteState) => "COMPLETE",
        Ok(pb::BehaviorResults::BehaviorWontActivateState) => "WONT_ACTIVATE",
        Err(_) => "UNRECOGNISED",
    }
}

/// `ActionResult.code`, by name where the name is worth having.
///
/// The enum has some fifty values. The ones spelled out here are the ones a
/// motion caller can act on; everything else renders as its number, which is
/// enough to look up and keeps this from becoming a transcription of the proto.
///
/// `PATH_PLANNING_FAILED_RETRY` is in the enum and its comment invites a retry,
/// but the robot's engine never produces it: every planning failure arrives as
/// `PATH_PLANNING_FAILED_ABORT`. It is listed so a reader who greps for it
/// finds this note rather than concluding the code is missing.
fn action_result(result: Option<&pb::action_result::ActionResultCode>) -> String {
    let Some(code) = result else {
        return "no-result".to_owned();
    };
    let name = match code {
        pb::action_result::ActionResultCode::ActionResultSuccess => "SUCCESS",
        pb::action_result::ActionResultCode::ActionResultRunning => "RUNNING",
        pb::action_result::ActionResultCode::ActionResultCancelledWhileRunning => "CANCELLED",
        pb::action_result::ActionResultCode::NotStarted => "NOT_STARTED",
        pb::action_result::ActionResultCode::Abort => "ABORT",
        pb::action_result::ActionResultCode::AnimAborted => "ANIM_ABORTED",
        pb::action_result::ActionResultCode::PathPlanningFailedAbort => "PATH_PLANNING_FAILED",
        // What an unreachable goal most likely reports: the planner found no
        // path, so the path component went back to ready without the robot
        // having arrived.
        pb::action_result::ActionResultCode::FailedTraversingPath => "FAILED_TRAVERSING_PATH",
        pb::action_result::ActionResultCode::FollowingPathButNotTraversing => {
            "FOLLOWING_PATH_BUT_NOT_TRAVERSING"
        }
        // Never produced by the robot; see the doc comment above.
        pb::action_result::ActionResultCode::PathPlanningFailedRetry => "PATH_PLANNING_RETRY",
        pb::action_result::ActionResultCode::StillOnCharger => "STILL_ON_CHARGER",
        // The action's tag is still in use on the robot, for instance after a
        // server restart began the tag counter again while an older action was
        // pending.
        pb::action_result::ActionResultCode::BadTag => "BAD_TAG",
        pb::action_result::ActionResultCode::UnexpectedPitchAngle => "UNEXPECTED_PITCH",
        other => return format!("code={}", *other as i32),
    };
    name.to_owned()
}

/// The `status` field alone, which is the whole body of the direct-motor
/// responses.
macro_rules! status_only {
    ($($response:ty => $moves:expr),* $(,)?) => {
        $(
            impl MotionOutcome for $response {
                const MOVES: bool = $moves;

                fn describe(&self) -> String {
                    response_status(self.status.as_ref()).to_owned()
                }
            }
        )*
    };
}

status_only!(
    pb::DriveWheelsResponse => true,
    pb::MoveHeadResponse => true,
    pb::MoveLiftResponse => true,
    pb::StopAllMotorsResponse => true,
    pb::EnableMirrorModeResponse => false,
    pb::SayTextResponse => false,
);

/// `status` plus an `ActionResult`, which is the action family.
macro_rules! status_and_action {
    ($($response:ty),* $(,)?) => {
        $(
            impl MotionOutcome for $response {
                const MOVES: bool = true;

                fn describe(&self) -> String {
                    format!(
                        "{} {}",
                        response_status(self.status.as_ref()),
                        action_result(
                            self.result
                                .as_ref()
                                .and_then(|result| result.code.try_into().ok())
                                .as_ref(),
                        ),
                    )
                }
            }
        )*
    };
}

status_and_action!(
    pb::GoToPoseResponse,
    pb::TurnInPlaceResponse,
    pb::SetHeadAngleResponse,
    pb::SetLiftHeightResponse,
    pb::DriveStraightResponse,
);

/// `status` plus a `BehaviorResults`, which is the behaviour family.
macro_rules! status_and_behavior {
    ($($response:ty),* $(,)?) => {
        $(
            impl MotionOutcome for $response {
                const MOVES: bool = true;

                fn describe(&self) -> String {
                    format!(
                        "{} {}",
                        response_status(self.status.as_ref()),
                        behavior_result(self.result),
                    )
                }
            }
        )*
    };
}

status_and_behavior!(
    pb::LookAroundInPlaceResponse,
    pb::DriveOffChargerResponse,
    pb::DriveOnChargerResponse,
);

/// `PlayAnimation` answers with the behaviour result and the animation it
/// actually chose, which for a trigger is the only way to learn which variant
/// ran.
impl MotionOutcome for pb::PlayAnimationResponse {
    const MOVES: bool = true;

    fn describe(&self) -> String {
        let played = self
            .animation
            .as_ref()
            .map_or("", |animation| animation.name.as_str());
        format!(
            "{} {} played={played}",
            response_status(self.status.as_ref()),
            behavior_result(self.result),
        )
    }
}

/// The priority a `ControlRequest` asked for, by name.
fn control_priority(priority: i32) -> &'static str {
    match pb::control_request::Priority::try_from(priority) {
        Ok(pb::control_request::Priority::Unknown) => "UNKNOWN",
        Ok(pb::control_request::Priority::OverrideBehaviors) => "OVERRIDE_BEHAVIORS",
        Ok(pb::control_request::Priority::Default) => "DEFAULT",
        Ok(pb::control_request::Priority::ReserveControl) => "RESERVE_CONTROL",
        Err(_) => "UNRECOGNISED",
    }
}

/// One line for a behaviour-control stream that has just been granted.
///
/// Worth its own line because behaviour control is the usual reason a motion
/// call does nothing: without it the robot accepts the RPC and ignores it.
///
/// `OVERRIDE_BEHAVIORS` is called out on the line because of what it costs. The
/// proto's own comment for it is "Suppresses most automatic physical
/// reactions", and one of the reactions it suppresses is the cliff response:
/// the robot stops braking at drops, and stops recording them in its map, until
/// control is released. A script that takes this priority and then drives is a
/// robot that will go off a table edge.
pub fn control_granted(comp: &'static str, esn: &str, priority: i32) {
    let name = control_priority(priority);
    if priority == pb::control_request::Priority::OverrideBehaviors as i32 {
        tracing::debug!(
            target: "sdkapp",
            comp = comp,
            bot = esn,
            "behavior control granted at {name}; cliff detection is off until release",
        );
    } else {
        tracing::debug!(
            target: "sdkapp",
            comp = comp,
            bot = esn,
            "behavior control granted at {name}",
        );
    }
}

/// One line for a behaviour-control stream being handed back.
pub fn control_released(comp: &'static str, esn: &str) {
    tracing::debug!(
        target: "sdkapp",
        comp = comp,
        bot = esn,
        "behavior control released",
    );
}

/// Times `call`, logs one debug line naming the decoded body, and hands the
/// result back untouched.
///
/// `args` is whatever the caller sent, already rendered, because a speed or an
/// angle is what makes a line worth reading twice. `comp` is the logger
/// component the line should carry, so a dashboard call shows as `sdkapp` and a
/// script's call shows as `lua`.
pub async fn logged<T, F>(
    comp: &'static str,
    session: &SdkSession,
    rpc: &'static str,
    args: &str,
    call: F,
) -> Result<Response<T>, Status>
where
    T: MotionOutcome,
    F: Future<Output = Result<Response<T>, Status>>,
{
    let esn = session.esn().as_str();
    let started = Instant::now();
    // Stamped before the call goes out, so the state stream's window covers
    // the robot's reaction from its first moment.
    if T::MOVES {
        session.state_stream.note_motion_call(MotionCall {
            rpc,
            args: args.to_owned(),
            at: started,
        });
    }
    let result = call.await;
    let millis = started.elapsed().as_millis();
    match &result {
        Ok(response) => tracing::debug!(
            target: "sdkapp",
            comp = comp,
            bot = esn,
            "motion {rpc}({args}) -> {} in {millis}ms",
            response.get_ref().describe(),
        ),
        Err(status) => tracing::debug!(
            target: "sdkapp",
            comp = comp,
            bot = esn,
            "motion {rpc}({args}) failed after {millis}ms: {}",
            status_error(status),
        ),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_direct_motor_response_names_its_status() {
        let ignored = pb::DriveWheelsResponse {
            status: Some(pb::ResponseStatus {
                code: pb::response_status::StatusCode::RequestProcessing as i32,
            }),
        };
        assert_eq!(ignored.describe(), "REQUEST_PROCESSING");

        // An absent status and an explicit UNKNOWN read the same, because the
        // robot means the same thing by both.
        assert_eq!(
            pb::DriveWheelsResponse { status: None }.describe(),
            "UNKNOWN"
        );
    }

    #[test]
    fn an_animation_that_could_not_start_says_so() {
        let refused = pb::PlayAnimationResponse {
            status: Some(pb::ResponseStatus {
                code: pb::response_status::StatusCode::Ok as i32,
            }),
            result: pb::BehaviorResults::BehaviorWontActivateState as i32,
            animation: None,
        };
        // The transport would have reported success here; only the body knows
        // that behaviour control was missing.
        assert_eq!(refused.describe(), "OK WONT_ACTIVATE played=");
    }

    #[test]
    fn an_action_result_names_the_codes_worth_acting_on() {
        let planning_failed = pb::GoToPoseResponse {
            status: Some(pb::ResponseStatus {
                code: pb::response_status::StatusCode::Ok as i32,
            }),
            result: Some(pb::ActionResult {
                code: pb::action_result::ActionResultCode::PathPlanningFailedAbort as i32,
            }),
        };
        assert_eq!(planning_failed.describe(), "OK PATH_PLANNING_FAILED");

        let on_charger = pb::GoToPoseResponse {
            status: None,
            result: Some(pb::ActionResult {
                code: pb::action_result::ActionResultCode::StillOnCharger as i32,
            }),
        };
        assert_eq!(on_charger.describe(), "UNKNOWN STILL_ON_CHARGER");
    }

    #[tokio::test]
    async fn the_wrapper_returns_the_result_untouched() {
        let session = SdkSession::new(wirepod_core::Esn::new("00303f28"));
        let ok = logged("sdkapp", &session, "DriveWheels", "lw=50 rw=50", async {
            Ok(Response::new(pb::DriveWheelsResponse {
                status: Some(pb::ResponseStatus {
                    code: pb::response_status::StatusCode::Ok as i32,
                }),
            }))
        })
        .await;
        assert_eq!(
            ok.expect("the wrapper passes the response through")
                .get_ref()
                .describe(),
            "OK"
        );

        let failed: Result<Response<pb::MoveHeadResponse>, Status> =
            logged("lua", &session, "MoveHead", "speed=2", async {
                Err(Status::unavailable("no connection"))
            })
            .await;
        assert_eq!(
            failed
                .expect_err("the wrapper passes the failure through")
                .code(),
            tonic::Code::Unavailable
        );
    }

    #[tokio::test]
    async fn only_a_call_that_moves_the_robot_opens_a_motion_window() {
        let session = SdkSession::new(wirepod_core::Esn::new("00303f28"));
        let _ = logged(
            "sdkapp",
            &session,
            "EnableMirrorMode",
            "enable=true",
            async { Ok(Response::new(pb::EnableMirrorModeResponse::default())) },
        )
        .await;
        assert_eq!(session.state_stream.motion_call(), None);

        let _ = logged("sdkapp", &session, "DriveWheels", "lw=50 rw=50", async {
            Ok(Response::new(pb::DriveWheelsResponse::default()))
        })
        .await;
        let call = session
            .state_stream
            .motion_call()
            .expect("a motion call is stamped");
        assert_eq!(
            (call.rpc, call.args.as_str()),
            ("DriveWheels", "lw=50 rw=50")
        );
    }
}
