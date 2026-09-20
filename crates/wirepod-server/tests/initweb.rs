//! The `/api-chipper/` setup handlers, driven directly with a state rooted in
//! a temporary directory so the configuration write lands nowhere real.

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::Response;
use http_body_util::BodyExt;
use wirepod_core::paths::{AssetDir, DataDir};
use wirepod_core::test_support::{FakeConnFactory, FakeRobotConn};
use wirepod_core::{AppState, Paths, RobotConn, RobotConnFactory};
use wirepod_server::initweb::chipper_http_api;

/// A state whose `apiConfig.json` lives under the system temporary directory.
fn state_in(name: &str) -> (Arc<AppState>, PathBuf) {
    let root = std::env::temp_dir().join(format!("wirepod-initweb-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create the temporary directory");
    let data = DataDir::rooted(root.clone());
    let gate = wirepod_core::config::config_gate(&data);
    let conn: Arc<dyn RobotConn> = Arc::new(FakeRobotConn::new());
    let factory: Arc<dyn RobotConnFactory> = Arc::new(FakeConnFactory::connecting_to(conn));
    let state = AppState::builder(factory)
        .paths(Paths::new(data, AssetDir::new(&root)))
        .config_gate(gate)
        .build();
    (state, root)
}

async fn body_of(response: Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read the body")
        .to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn call(state: &Arc<AppState>, uri: &str) -> String {
    let req = Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("build the request");
    body_of(chipper_http_api(State(Arc::clone(state)), req).await).await
}

#[tokio::test]
async fn use_ip_refuses_a_missing_and_a_non_numeric_port() {
    let (state, root) = state_in("bad-port");

    assert_eq!(
        call(&state, "/api-chipper/use_ip").await,
        "error: must have port"
    );
    assert_eq!(
        call(&state, "/api-chipper/use_ip?port=eighty").await,
        "error: port is invalid"
    );
    assert!(
        !state.config().past_initial_setup,
        "a refused port leaves the configuration alone"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn use_ip_and_use_ep_answer_done_and_move_the_server_configuration() {
    let (state, root) = state_in("done");

    assert_eq!(call(&state, "/api-chipper/use_ip?port=8080").await, "done");
    let config = state.config();
    assert!(!config.server.epconfig);
    assert_eq!(config.server.port, "8080");
    assert!(config.past_initial_setup);

    assert_eq!(call(&state, "/api-chipper/use_ep").await, "done");
    let config = state.config();
    assert!(config.server.epconfig);
    assert_eq!(config.server.port, "443");

    assert_eq!(call(&state, "/api-chipper/restart").await, "done");
    assert_eq!(
        call(&state, "/api-chipper/nothing").await,
        "",
        "an unmatched path writes no body"
    );

    let _ = std::fs::remove_dir_all(root);
}
