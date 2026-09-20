//! Go's `pkg/initwirepod/web.go`: the `/api-chipper/` setup handlers.
//!
//! cant be part of config-ws, otherwise import cycle

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::response::Response;
use wirepod_core::{AppState, write_config_to_disk};

use crate::{form, literals, reply, startserver};

const RESTART: &str = "/api-chipper/restart";
const USE_IP: &str = "/api-chipper/use_ip";
const USE_EP: &str = "/api-chipper/use_ep";

pub async fn chipper_http_api(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, form) = form::read(req).await;
    match parts.uri.path() {
        RESTART => {
            restart(&state).await;
            reply::text(literals::DONE)
        }
        USE_IP => {
            let port = form.get("port");
            if port.is_empty() {
                return reply::text("error: must have port");
            }
            if port.parse::<i64>().is_err() {
                return reply::text("error: port is invalid");
            }
            let port = port.to_owned();
            state.update_config(|config| {
                config.server.epconfig = false;
                config.server.port = port;
            });
            // TODO(M5): botsetup.CreateCertCombo()
            // TODO(M5): botsetup.CreateServerConfig()
            state.update_config(|config| config.past_initial_setup = true);
            write_config(&state).await;
            restart(&state).await;
            reply::text(literals::DONE)
        }
        USE_EP => {
            state.update_config(|config| {
                config.server.epconfig = true;
                config.server.port = "443".to_owned();
                config.past_initial_setup = true;
            });
            // TODO(M5): botsetup.CreateServerConfig()
            write_config(&state).await;
            restart(&state).await;
            reply::text(literals::DONE)
        }
        // Go's switch matches nothing and the handler writes no body.
        _ => reply::empty(),
    }
}

/// Go discards this error; it is logged here.
async fn write_config(state: &Arc<AppState>) {
    if let Err(err) = write_config_to_disk(&state.config(), state.config_gate()).await {
        tracing::info!("{err}");
    }
}

/// Go's `RestartServer` reports nothing; a failed rebind is logged here.
async fn restart(state: &Arc<AppState>) {
    if let Err(err) = startserver::restart_server(state).await {
        tracing::info!("{err}");
    }
}
