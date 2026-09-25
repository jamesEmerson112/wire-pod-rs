//! The nav map feed over a real gRPC connection to the fake robot on loopback.

use std::sync::Arc;
use std::time::Duration;

use wirepod_core::robot::navmap::{NavMapFrame, NavMapInfo, NavMapQuad};
use wirepod_core::{ConnTarget, Esn, RobotConn, RobotConnFactory};
use wirepod_vector::test_support::{FakeRobotHandle, spawn_fake_robot};
use wirepod_vector::{TonicConnFactory, plaintext_builder};

const CEILING: Duration = Duration::from_secs(5);

/// The GUID the tests authenticate with. Never a real one.
const GUID: &str = "<guid>";

async fn connect() -> (Arc<dyn RobotConn>, FakeRobotHandle) {
    let (addr, handle) = spawn_fake_robot().await;
    let factory = TonicConnFactory::with_endpoint_builder(plaintext_builder());
    let target = ConnTarget {
        esn: Esn::new("00303F28"),
        ip: addr.to_string(),
        guid: GUID.to_owned(),
    };
    let conn = factory.connect(&target).await.expect("dial the fake robot");
    (conn, handle)
}

fn map() -> NavMapFrame {
    NavMapFrame {
        origin_id: 7,
        info: NavMapInfo {
            root_depth: 1,
            root_size_mm: 256.0,
            root_center_x: 64.0,
            root_center_y: -16.0,
        },
        quads: (0..4)
            .map(|index| NavMapQuad {
                content: index + 1,
                depth: 0,
                rgba: 0xff00_00ff + index as u32,
            })
            .collect(),
    }
}

#[tokio::test]
async fn maps_arrive_as_sent_and_the_period_goes_out_in_seconds() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        let mut feed = conn
            .open_nav_map_feed(Duration::from_millis(500))
            .await
            .expect("open the nav map feed");
        assert_eq!(handle.last_nav_map_period(), Some(0.5));
        assert_eq!(
            handle
                .calls()
                .iter()
                .find(|call| call.method == "NavMapFeed")
                .and_then(|call| call.authorization.as_deref()),
            Some("Bearer <guid>")
        );

        handle.push_nav_map(&map());
        assert_eq!(feed.next().await.expect("receive"), Some(map()));
        handle.end_nav_map_feed();
        assert_eq!(feed.next().await.expect("a clean end"), None);
        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}

#[tokio::test]
async fn a_zero_period_is_never_sent() {
    tokio::time::timeout(CEILING, async {
        let (conn, handle) = connect().await;
        let feed = conn
            .open_nav_map_feed(Duration::ZERO)
            .await
            .expect("open the nav map feed");
        assert_eq!(handle.last_nav_map_period(), Some(0.1));
        // A stream still open would hold the server's graceful shutdown.
        drop(feed);
        handle.end_nav_map_feed();
        handle.shutdown().await;
    })
    .await
    .expect("within the ceiling");
}
