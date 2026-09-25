//! Translation of `pkg/wirepod/ttr/bcontrol.go`.

use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use wirepod_core::logger::COMP_SDK;
use wirepod_core::{ConnError, RobotEntry, StatusCode};
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::motionlog::{control_granted, control_released};
use wirepod_vector::{sdk_client, status_error};

/// The priority both helpers ask for, named once so the log line and the
/// request cannot drift apart.
const OVERRIDE_BEHAVIORS: i32 = pb::control_request::Priority::OverrideBehaviors as i32;

/// What a call answers when the connection is not a tonic one, which only a
/// test fake is.
fn no_sdk_client() -> ConnError {
    ConnError::new(StatusCode::Unavailable, "no SDK client")
}

/// Go's `r.Recv()` reads a closed stream as `io.EOF` and logs the bare `EOF`.
fn stream_ended() -> ConnError {
    ConnError::new(StatusCode::Unavailable, "EOF")
}

fn control_request() -> pb::BehaviorControlRequest {
    pb::BehaviorControlRequest {
        request_type: Some(pb::behavior_control_request::RequestType::ControlRequest(
            pb::ControlRequest {
                priority: pb::control_request::Priority::OverrideBehaviors as i32,
            },
        )),
    }
}

fn control_release() -> pb::BehaviorControlRequest {
    pb::BehaviorControlRequest {
        request_type: Some(pb::behavior_control_request::RequestType::ControlRelease(
            pb::ControlRelease {},
        )),
    }
}

fn granted(response: &pb::BehaviorControlResponse) -> bool {
    matches!(
        response.response_type,
        Some(pb::behavior_control_response::ResponseType::ControlGrantedResponse(_))
    )
}

/// Takes behaviour control, says `text`, releases.
///
/// Go nests two goroutines over two unbuffered channels and never closes the
/// outer `for range start`, so every call leaks a goroutine and both channels.
/// This is one awaited function that drops everything it made. Go's error paths
/// return without ever signalling `start`, which leaves that goroutine blocked
/// forever; here they reach the caller. The RPC is uncancelled, as Go's
/// `context.Background()` makes it.
pub async fn say_text(entry: &RobotEntry, text: &str) -> Result<(), ConnError> {
    let Some(mut client) = sdk_client(entry.conn.as_ref()) else {
        return Err(no_sdk_client());
    };
    let (sender, receiver) = mpsc::channel(4);
    // Queued before the call rather than after it, because tonic returns on the
    // response headers and a peer that waits for the first request message
    // would otherwise never send them.
    let _ = sender.send(control_request()).await;
    let mut stream = client
        .behavior_control(ReceiverStream::new(receiver))
        .await
        .map_err(|status| status_error(&status))?
        .into_inner();
    loop {
        match stream.message().await {
            Ok(Some(response)) if granted(&response) => break,
            Ok(Some(_)) => continue,
            Ok(None) => return Err(stream_ended()),
            Err(status) => return Err(status_error(&status)),
        }
    }
    control_granted(COMP_SDK, entry.esn.as_str(), OVERRIDE_BEHAVIORS);
    // Go discards this call's error.
    let _ = client
        .say_text(pb::SayTextRequest {
            text: text.to_owned(),
            use_vector_voice: true,
            duration_scalar: 1.0,
            pitch_scalar: 0.0,
        })
        .await;
    control_released(COMP_SDK, entry.esn.as_str());
    let _ = sender.send(control_release()).await;
    Ok(())
}

/// Logs what Go logs and hands the caller the error Go's `start` channel never
/// carries.
fn fail(start: oneshot::Sender<Result<(), ConnError>>, err: ConnError) {
    tracing::info!(comp = "", "{err}");
    let _ = start.send(Err(err));
}

/// Takes behaviour control in the background and releases it on `stop`.
///
/// The returned receiver is Go's `start` channel: it resolves once control is
/// granted, or carries the error Go only logs. Go's wait for `stop` is a
/// `select` with a `default` arm, which spins a core at full tilt until the
/// stop arrives; awaiting a token removes the spin and changes nothing else.
/// `ctx` cancels the whole task, which is what Go's context does to the stream.
pub fn b_control(
    entry: &RobotEntry,
    ctx: CancellationToken,
    stop: CancellationToken,
) -> oneshot::Receiver<Result<(), ConnError>> {
    let client = sdk_client(entry.conn.as_ref());
    let esn = entry.esn.as_str().to_owned();
    let (start, started) = oneshot::channel();
    tokio::spawn(async move {
        let run = async move {
            let Some(mut client) = client else {
                return fail(start, no_sdk_client());
            };
            let (sender, receiver) = mpsc::channel(4);
            let _ = sender.send(control_request()).await;
            let mut stream = match client.behavior_control(ReceiverStream::new(receiver)).await {
                Ok(stream) => stream.into_inner(),
                Err(status) => return fail(start, status_error(&status)),
            };
            loop {
                match stream.message().await {
                    Ok(Some(response)) if granted(&response) => break,
                    Ok(Some(_)) => continue,
                    Ok(None) => return fail(start, stream_ended()),
                    Err(status) => return fail(start, status_error(&status)),
                }
            }
            control_granted(COMP_SDK, &esn, OVERRIDE_BEHAVIORS);
            let _ = start.send(Ok(()));
            stop.cancelled().await;
            control_released(COMP_SDK, &esn);
            tracing::info!(comp = "", "KGSim: releasing behavior control (interrupt)");
            let _ = sender.send(control_release()).await;
        };
        tokio::select! {
            () = ctx.cancelled() => {}
            () = run => {}
        }
    });
    started
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use wirepod_core::{AppState, BotInfo, Esn, RobotConnFactory};
    use wirepod_vector::test_support::{FakeRobotHandle, spawn_fake_robot};
    use wirepod_vector::{TonicConnFactory, plaintext_builder};

    use super::*;

    /// This machine's robot. The GUID beside it is a placeholder: a real one
    /// never goes into a fixture.
    const SERIAL: &str = "00303f28";

    /// The real-clock ceiling every test runs under.
    const CEILING: Duration = Duration::from_secs(10);

    /// A robot entry whose connection is the loopback fake.
    async fn live_entry() -> (Arc<RobotEntry>, FakeRobotHandle) {
        let (addr, handle) = spawn_fake_robot().await;
        let bot_info: BotInfo = serde_json::from_str(&format!(
            concat!(
                r#"{{"global_guid":"global-guid-placeholder","robots":[{{"esn":"{esn}","#,
                r#""ip_address":"{ip}","guid":"robot-guid-placeholder","activated":true}}]}}"#
            ),
            esn = SERIAL,
            ip = addr
        ))
        .expect("parse the loopback fixture");
        let factory: Arc<dyn RobotConnFactory> =
            Arc::new(TonicConnFactory::with_endpoint_builder(plaintext_builder()));
        let state = AppState::builder(factory).bot_info(bot_info).build();
        let entry = state
            .get_robot(&Esn::new(SERIAL))
            .await
            .expect("dial the fake robot");
        (entry, handle)
    }

    #[tokio::test]
    async fn say_text_takes_control_before_it_speaks_and_returns_once_released() {
        tokio::time::timeout(CEILING, async {
            let (entry, handle) = live_entry().await;
            say_text(&entry, "hello").await.expect("say the text");
            let methods = handle.methods();
            assert_eq!(
                &methods[methods.len() - 2..],
                &["BehaviorControl", "say_text"][..]
            );
            handle.shutdown().await;
        })
        .await
        .expect("the say-text round trip finished");
    }

    #[tokio::test]
    async fn b_control_signals_the_caller_once_control_is_granted() {
        tokio::time::timeout(CEILING, async {
            let (entry, handle) = live_entry().await;
            let stop = CancellationToken::new();
            let started = b_control(&entry, CancellationToken::new(), stop.clone());
            assert_eq!(started.await.expect("the task signalled"), Ok(()));
            assert_eq!(handle.methods().last(), Some(&"BehaviorControl"));
            stop.cancel();
            handle.shutdown().await;
        })
        .await
        .expect("the grant arrived");
    }
}
