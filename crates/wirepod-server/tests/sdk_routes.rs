//! The `/api-sdk/*` routes M2 translated, driven through the router against the
//! loopback fake robot.
//!
//! The fake binds `127.0.0.1:0`, so nothing here binds a fixed port or reaches
//! the real robot. The settings routes reach the robot's REST surface at
//! `https://<ip>:443`, and the fixture's address already carries the fake's
//! ephemeral port, so that URL never parses and no request leaves the process.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Router;
use http::{Method, StatusCode, header};
use wirepod_core::{AppState, BotInfo, Esn, JdocsStore, RobotConnFactory};
use wirepod_server::test_support::{CEILING, Reply, request, send_to};
use wirepod_server::{build_router, literals};
use wirepod_vector::test_support::{FakeRobotHandle, ScriptedDoc, ScriptedJdoc, spawn_fake_robot};
use wirepod_vector::{TonicConnFactory, plaintext_builder};

const SERIAL: &str = "00303f28";

struct Fixture {
    router: Router,
    state: Arc<AppState>,
    handle: FakeRobotHandle,
    jdocs_path: PathBuf,
}

impl Fixture {
    async fn post(&self, uri: &str) -> Reply {
        send_to(&self.router, request(Method::POST, uri, Some(""))).await
    }

    async fn post_form(&self, uri: &str, body: &str) -> Reply {
        send_to(&self.router, request(Method::POST, uri, Some(body))).await
    }

    async fn finish(self) {
        self.handle.shutdown().await;
        let _ = std::fs::remove_file(&self.jdocs_path);
    }
}

/// A router whose registry dials the fake through the production client.
///
/// The jdocs store is rooted in the system temporary directory and named after
/// the fake's port, so `get_sdk_settings` writes nothing into the repository
/// and two tests running at once cannot collide.
async fn fixture() -> Fixture {
    let (addr, handle) = spawn_fake_robot().await;
    // The GUID is a placeholder. A real robot GUID never goes into a fixture.
    let bot_info: BotInfo = serde_json::from_str(&format!(
        concat!(
            r#"{{"global_guid":"global-guid-placeholder","robots":[{{"esn":"{esn}","#,
            r#""ip_address":"{ip}","guid":"robot-guid-placeholder","activated":true}}]}}"#
        ),
        esn = SERIAL,
        ip = addr
    ))
    .expect("parse the loopback fixture");

    let jdocs_path = std::env::temp_dir().join(format!("wirepod-rs-jdocs-{}.json", addr.port()));
    let factory: Arc<dyn RobotConnFactory> =
        Arc::new(TonicConnFactory::with_endpoint_builder(plaintext_builder()));
    let state = AppState::builder(factory)
        .bot_info(bot_info)
        .jdocs(JdocsStore::new(jdocs_path.to_string_lossy().into_owned()))
        .build();
    Fixture {
        router: build_router(Arc::clone(&state)),
        state,
        handle,
        jdocs_path,
    }
}

#[tokio::test]
async fn the_settings_routes_answer_done_and_hand_back_the_settings_jdoc() {
    tokio::time::timeout(CEILING, async {
        let fixture = fixture().await;
        fixture.handle.set_jdocs(vec![ScriptedJdoc {
            jdoc_type: 0,
            doc: Some(ScriptedDoc {
                doc_version: 3,
                fmt_version: 1,
                client_metadata: String::new(),
                json_doc: r#"{"clock_24_hour":true}"#.to_owned(),
            }),
        }]);

        let reply = fixture
            .post(&format!("/api-sdk/volume?serial={SERIAL}"))
            .await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.body, literals::DONE);

        // Go writes the two form values concatenated and unseparated.
        let reply = fixture
            .post_form(
                &format!("/api-sdk/custom_eye_color?serial={SERIAL}"),
                "hue=0.500&sat=1.000",
            )
            .await;
        assert_eq!(reply.body, "0.5001.000");

        let reply = fixture
            .post(&format!("/api-sdk/get_sdk_settings?serial={SERIAL}"))
            .await;
        assert_eq!(reply.body, r#"{"clock_24_hour":true}"#);
        assert!(fixture.handle.methods().contains(&"PullJdocs"));
        assert_eq!(
            fixture
                .handle
                .last_jdocs_request()
                .expect("request recorded")
                .jdoc_types,
            vec![0]
        );

        fixture.finish().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn the_face_routes_reach_the_enrollment_rpcs() {
    tokio::time::timeout(CEILING, async {
        let fixture = fixture().await;

        // The fake answers the default message, whose face list is empty, and
        // Go marshals a nil slice as `null`.
        let reply = fixture
            .post(&format!("/api-sdk/get_faces?serial={SERIAL}"))
            .await;
        assert_eq!(reply.body, "null");

        let reply = fixture
            .post(&format!(
                "/api-sdk/rename_face?serial={SERIAL}&id=3&oldname=a&newname=b"
            ))
            .await;
        assert_eq!(reply.body, literals::SUCCESS);

        let reply = fixture
            .post(&format!("/api-sdk/add_face?serial={SERIAL}&name=James"))
            .await;
        assert_eq!(reply.body, literals::SUCCESS);

        let methods = fixture.handle.methods();
        assert!(methods.contains(&"request_enrolled_names"), "{methods:?}");
        assert!(
            methods.contains(&"update_enrolled_face_by_id"),
            "{methods:?}"
        );
        assert!(methods.contains(&"app_intent"), "{methods:?}");

        fixture.finish().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn the_photo_routes_answer_null_and_gos_atoi_message() {
    tokio::time::timeout(CEILING, async {
        let fixture = fixture().await;

        let reply = fixture
            .post(&format!("/api-sdk/get_image_ids?serial={SERIAL}"))
            .await;
        assert_eq!(reply.body, "null");
        assert!(fixture.handle.methods().contains(&"photos_info"));

        // The parse failure never reaches the robot.
        let reply = fixture
            .post(&format!("/api-sdk/get_image?serial={SERIAL}&id=abc"))
            .await;
        assert_eq!(
            reply.body,
            r#"error: strconv.Atoi: parsing "abc": invalid syntax"#
        );
        assert!(!fixture.handle.methods().contains(&"photo"));

        let reply = fixture
            .post(&format!("/api-sdk/delete_image?serial={SERIAL}&id=1"))
            .await;
        assert_eq!(reply.body, literals::DONE);

        fixture.finish().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn the_motion_routes_write_no_body_and_mirror_mode_writes_success() {
    tokio::time::timeout(CEILING, async {
        let fixture = fixture().await;

        let reply = fixture
            .post(&format!(
                "/api-sdk/move_wheels?serial={SERIAL}&lw=50&rw=-50"
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.body, "");
        // Go writes zero bytes, so `net/http` sniffs nothing.
        assert_eq!(reply.header(header::CONTENT_TYPE), None);

        let reply = fixture
            .post(&format!("/api-sdk/mirror_mode?serial={SERIAL}&enable=true"))
            .await;
        assert_eq!(reply.body, literals::SUCCESS);

        let methods = fixture.handle.methods();
        assert!(methods.contains(&"drive_wheels"), "{methods:?}");
        assert!(methods.contains(&"enable_mirror_mode"), "{methods:?}");

        fixture.finish().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn say_text_and_get_battery_answer_the_documented_bodies() {
    tokio::time::timeout(CEILING, async {
        let fixture = fixture().await;

        let reply = fixture
            .post(&format!("/api-sdk/say_text?serial={SERIAL}&text=hello"))
            .await;
        assert_eq!(reply.body, literals::SUCCESS);
        assert!(fixture.handle.methods().contains(&"say_text"));

        // Go's `omitempty` drops every field of the zero reading.
        let reply = fixture
            .post(&format!("/api-sdk/get_battery?serial={SERIAL}"))
            .await;
        assert_eq!(reply.body, "{}");

        // The arm writes nothing on any path, including the one where no part
        // named `sound` arrived.
        let reply = fixture
            .post(&format!("/api-sdk/play_sound?serial={SERIAL}"))
            .await;
        assert_eq!(reply.body, "");

        let reply = fixture
            .post(&format!("/api-sdk/print_robot_info?serial={SERIAL}"))
            .await;
        assert!(reply.body.starts_with("&{00303f28 "), "{}", reply.body);

        fixture.finish().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn behavior_control_is_assumed_and_released_through_the_session_flag() {
    tokio::time::timeout(CEILING, async {
        let fixture = fixture().await;
        let esn = Esn::new(SERIAL);

        let reply = fixture
            .post(&format!(
                "/api-sdk/assume_behavior_control?serial={SERIAL}&priority=high"
            ))
            .await;
        assert_eq!(reply.body, literals::SUCCESS);
        let entry = fixture.state.registry().peek(&esn).expect("connected");
        assert!(entry.session.bc_assumption.load(Ordering::SeqCst));

        let reply = fixture
            .post(&format!(
                "/api-sdk/release_behavior_control?serial={SERIAL}"
            ))
            .await;
        assert_eq!(reply.body, literals::SUCCESS);
        assert!(!entry.session.bc_assumption.load(Ordering::SeqCst));

        fixture.finish().await;
    })
    .await
    .expect("within the ceiling");
}
