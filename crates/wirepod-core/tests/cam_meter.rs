//! Camera meters. Ports of Go tests 7 and 8,
//! `TestCamMeterKeepsRobotsApart` (`sdkapp_test.go:415-443`) and
//! `TestCamMeterCountsExactlyUnderConcurrency` (`sdkapp_test.go:449-496`).
//!
//! No runtime and no async: `std::thread::scope` covers the concurrent case.

use std::sync::Arc;

use wirepod_core::{CamMeters, Esn};

#[test]
fn meters_keep_robots_apart_and_an_unknown_esn_reads_zero() {
    let meters = CamMeters::new();
    let esn_a = Esn::new("00e20100");
    let esn_b = Esn::new("00e20101");

    let a = meters.get(&esn_a);
    let b = meters.get(&esn_b);
    assert!(!Arc::ptr_eq(&a, &b), "two ESNs share one meter");

    a.record(1500);
    b.record(30);
    b.record(10);

    assert_eq!(
        meters.read(&esn_a),
        (1500, 1),
        "esnA read back wrong totals"
    );
    assert_eq!(meters.read(&esn_b), (40, 2), "esnB read back wrong totals");

    // An ESN nobody has streamed reads zero rather than panicking, because
    // net_probe answers for a robot whose camera has never been opened.
    let before = meters.len();
    assert_eq!(before, 2, "only the two streamed ESNs should have meters");
    assert_eq!(
        meters.read(&Esn::new("00e20102")),
        (0, 0),
        "an unseen ESN did not read zero"
    );
    // Go's readCamMeter goes through getCamMeter and so inserts on read
    // (`robot.go:182-185`). The port deliberately does not.
    assert_eq!(
        meters.len(),
        before,
        "reading an unknown ESN inserted a meter"
    );
}

#[test]
fn the_meter_counts_exactly_under_concurrency() {
    const WRITERS: usize = 8;
    const PER_WRITER: usize = 2000;
    const FRAME_SIZE: u64 = 1234;

    let meters = CamMeters::new();
    let esn = Esn::new("00e20100");
    let meter = meters.get(&esn);

    std::thread::scope(|scope| {
        for _ in 0..WRITERS {
            let meter = Arc::clone(&meter);
            scope.spawn(move || {
                for _ in 0..PER_WRITER {
                    meter.record(FRAME_SIZE);
                }
            });
        }
        // Go runs a free-running reader through the same accessor net_probe
        // uses, so the map lookup runs concurrently with the lock-free adds.
        // Bounded here rather than spinning, because an unyielding spin would
        // starve the writers on a single-core runner (go-tests.md, test 8).
        scope.spawn(|| {
            for _ in 0..PER_WRITER {
                let _ = meters.read(&esn);
            }
        });
    });

    assert_eq!(
        meters.read(&esn),
        (19_744_000, 16_000),
        "an update was lost under concurrency"
    );
}
