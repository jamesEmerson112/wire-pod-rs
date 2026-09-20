//! Translation of `pkg/scripting/bcontrol.go`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mlua::{Lua, Value};
use tokio::runtime::Handle;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use wirepod_core::{ConnError, StatusCode};
use wirepod_proto::anki::vector::external_interface as pb;
use wirepod_vector::status_error;

use crate::scripting::{g_rf_ls, to_int};

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

/// Go logs the failure and returns without ever signalling `start`, which
/// leaves the script blocked forever; the signal still goes out here so the
/// call returns.
async fn fail(start: &mpsc::Sender<()>, err: ConnError) {
    tracing::debug!(comp = "", "{err}");
    let _ = start.send(()).await;
}

pub fn set_b_control_functions(lua: &Lua) -> mlua::Result<()> {
    // Go's channels are unbuffered, so a send blocks until the other side
    // takes it; one slot is the closest tokio shape and only makes the release
    // return a moment earlier.
    let (start_tx, start_rx) = mpsc::channel::<()>(1);
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let start_rx = Arc::new(AsyncMutex::new(start_rx));
    let stop_rx = Arc::new(AsyncMutex::new(stop_rx));
    let currently_assumed = Arc::new(AtomicBool::new(false));

    let assumed = Arc::clone(&currently_assumed);
    let assume = lua.create_function(move |lua, arg: Value| {
        let mut client = g_rf_ls(lua)?;
        // Go seeds `priority` with OVERRIDE_BEHAVIORS and only then tests it
        // against the four valid values, so the warning is unreachable and the
        // argument always wins.
        let mut priority = pb::control_request::Priority::OverrideBehaviors as i32;
        if priority != 0 && priority != 10 && priority != 20 && priority != 30 {
            tracing::debug!(
                comp = "",
                "LUA: Behavior control priority was not valid. Valid choices are 10, 20, and 30. Assuming 10."
            );
        } else {
            priority = to_int(lua, arg) as i32;
        }
        let control_request = pb::BehaviorControlRequest {
            request_type: Some(pb::behavior_control_request::RequestType::ControlRequest(
                pb::ControlRequest { priority },
            )),
        };
        let Ok(handle) = Handle::try_current() else {
            return Ok(());
        };
        let start = start_tx.clone();
        let stop = Arc::clone(&stop_rx);
        handle.spawn(async move {
            // * begin - modified from official vector-go-sdk
            let (sender, receiver) = mpsc::channel(4);
            // Queued before the call rather than after it, because tonic
            // returns on the response headers and a peer that waits for the
            // first request message would otherwise never send them.
            let _ = sender.send(control_request).await;
            let mut r = match client.behavior_control(ReceiverStream::new(receiver)).await {
                Ok(stream) => stream.into_inner(),
                Err(status) => return fail(&start, status_error(&status)).await,
            };

            loop {
                match r.message().await {
                    Ok(Some(ctrlresp)) if granted(&ctrlresp) => {
                        let _ = start.send(()).await;
                        break;
                    }
                    Ok(Some(_)) => continue,
                    // Go reads a closed stream as `io.EOF` and logs the bare text.
                    Ok(None) => {
                        return fail(&start, ConnError::new(StatusCode::Unavailable, "EOF")).await;
                    }
                    Err(status) => return fail(&start, status_error(&status)).await,
                }
            }

            // Go's wait is a `select` with a `default` arm, which spins a core
            // at full tilt until the stop arrives; awaiting removes the spin.
            stop.lock().await.recv().await;
            if let Err(err) = sender.send(control_release()).await {
                tracing::debug!(comp = "", "{err}");
            }
            // * end - modified from official vector-go-sdk
        });
        handle.block_on(async { start_rx.lock().await.recv().await });
        assumed.store(true, Ordering::SeqCst);
        Ok(())
    })?;
    lua.globals().set("assumeBehaviorControl", assume)?;

    let assumed = Arc::clone(&currently_assumed);
    let release = lua.create_function(move |_, ()| {
        if assumed.load(Ordering::SeqCst) {
            if let Ok(handle) = Handle::try_current() {
                let _ = handle.block_on(stop_tx.send(()));
            }
            assumed.store(false, Ordering::SeqCst);
            return Ok(());
        }
        // Go returns one value here without ever pushing one.
        Ok(())
    })?;
    lua.globals().set("releaseBehaviorControl", release)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_globals_exist_and_release_without_an_assume_does_nothing() {
        let lua = Lua::new();
        set_b_control_functions(&lua).expect("the globals are set");
        let kind: String = lua
            .load("return type(assumeBehaviorControl)")
            .eval()
            .expect("the global is readable");
        assert_eq!(kind, "function");
        lua.load("releaseBehaviorControl()")
            .exec()
            .expect("a release with no control taken is a no-op");
    }
}
