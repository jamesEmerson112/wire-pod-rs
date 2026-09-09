//! The shared application state and its builder.
//!
//! The builder is the only place the slice's defaults are written down, so a
//! test asserts them: Go's timings, a system clock, an empty bot-info file and
//! no connect-time liveness deadline.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn};
use wirepod_core::{
    AppState, BotInfo, BotInfoRobot, Clock, Esn, GetRobotError, ManualClock, RobotConn,
    RobotConnFactory, Timings,
};

const CEILING: Duration = Duration::from_secs(2);

const ESN_A: &str = "00303f28";

async fn within<F: Future>(operation: F) -> F::Output {
    tokio::time::timeout(CEILING, operation)
        .await
        .expect("the operation did not finish inside the ceiling")
}

fn fakes() -> (Arc<FakeRobotConn>, Arc<dyn RobotConnFactory>) {
    let robot = Arc::new(FakeRobotConn::new());
    let conn: Arc<dyn RobotConn> = robot.clone();
    (robot, Arc::new(FakeConnFactory::connecting_to(conn)))
}

fn bot_info() -> BotInfo {
    BotInfo {
        global_guid: "<guid>".to_string(),
        robots: vec![BotInfoRobot {
            esn: ESN_A.to_string(),
            ip_address: "192.168.8.203".to_string(),
            activated: true,
            ..BotInfoRobot::default()
        }],
        ..BotInfo::default()
    }
}

#[test]
fn the_builder_defaults_to_the_go_timings_and_an_empty_bot_info() {
    let (_robot, factory) = fakes();
    let state = AppState::builder(factory).build();

    assert_eq!(*state.timings(), Timings::default());
    assert_eq!(*state.registry().timings(), Timings::default());
    assert_eq!(
        state.registry().liveness_deadline(),
        None,
        "the connect-time liveness check picked up a deadline Go does not have"
    );
    assert!(state.registry().is_empty());
    state.with_bot_info(|info| {
        assert_eq!(
            info,
            &BotInfo::default(),
            "the builder invented a bot-info file"
        );
    });
    assert!(
        state.pinger().is_enabled(),
        "the pinger did not default to Go's PingerEnabled = true"
    );
    assert!(
        state
            .with_bot_info(|info| state.pinger().snapshot(info, state.clock().as_ref()))
            .is_empty()
    );
}

#[tokio::test]
async fn the_builder_applies_its_overrides() {
    let (robot, factory) = fakes();
    let clock = Arc::new(ManualClock::at(Duration::from_secs(9)));
    let state = AppState::builder(factory)
        .bot_info(bot_info())
        .timings(Timings::instant())
        .clock(Arc::clone(&clock) as Arc<dyn Clock>)
        .liveness_deadline(Some(Duration::from_millis(20)))
        .build();

    assert_eq!(*state.timings(), Timings::instant());
    assert_eq!(*state.registry().timings(), Timings::instant());
    assert_eq!(
        state.registry().liveness_deadline(),
        Some(Duration::from_millis(20))
    );
    assert_eq!(state.clock().now(), Duration::from_secs(9));

    let entry = within(state.get_robot(&Esn::new(ESN_A)))
        .await
        .expect("the connect failed");
    assert_eq!(entry.target.grpc_target(), "192.168.8.203:443");
    assert_eq!(
        entry.last_touch(),
        Duration::from_secs(9),
        "the connect did not stamp the entry from the builder's clock"
    );
    assert_eq!(robot.calls().len(), 1);
    assert_eq!(state.registry().len(), 1);
}

/// The preamble resolves the serial against whatever the bot-info store holds
/// now, so a robot that authenticates after startup is reachable without a
/// restart.
#[tokio::test]
async fn get_robot_reads_the_current_bot_info() {
    let (_robot, factory) = fakes();
    let state = AppState::builder(factory).build();
    let esn = Esn::new(ESN_A);

    let err = within(state.get_robot(&esn))
        .await
        .expect_err("an empty bot-info file produced a robot");
    assert_eq!(err, GetRobotError::NotFound);
    assert_eq!(err.to_string(), "error: robot not found in SDK info file");

    state.set_bot_info(bot_info());
    assert_eq!(state.bot_info_snapshot(), bot_info());

    let entry = within(state.get_robot(&esn))
        .await
        .expect("the connect failed after the bot-info file was replaced");
    assert_eq!(entry.esn, esn);
}
