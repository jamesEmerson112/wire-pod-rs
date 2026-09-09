//! The timing constants are asserted so nobody changes them silently.

use std::time::Duration;

use wirepod_core::Timings;

#[test]
fn default_timings_hold_the_go_values() {
    let timings = Timings::default();
    assert_eq!(timings.settle, Duration::from_millis(500));
    assert_eq!(timings.probe, Duration::from_secs(5));
    assert_eq!(timings.enable, Duration::from_secs(5));
    assert_eq!(timings.disconnect_settle, Duration::from_secs(3));
    assert_eq!(timings.idle, Duration::from_secs(300));
}

#[test]
fn instant_timings_are_all_zero() {
    let timings = Timings::instant();
    assert_eq!(timings.settle, Duration::ZERO);
    assert_eq!(timings.probe, Duration::ZERO);
    assert_eq!(timings.enable, Duration::ZERO);
    assert_eq!(timings.disconnect_settle, Duration::ZERO);
    assert_eq!(timings.idle, Duration::ZERO);
}
