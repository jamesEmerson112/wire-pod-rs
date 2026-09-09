//! Camera ownership. Port of Go test 2,
//! `TestReleaseCamStreamIgnoresSupersededOwner` (`sdkapp_test.go:131-164`).
//!
//! No runtime: the state machine is synchronous by design.

use wirepod_core::CamOwner;

#[test]
fn release_ignores_a_superseded_owner() {
    let owner = CamOwner::new();

    let (first, displaced) = owner.claim();
    assert!(
        !displaced,
        "first claim reported displacing an owner that did not exist"
    );

    let (second, displaced) = owner.claim();
    assert!(
        displaced,
        "second claim did not report displacing the first"
    );
    assert_ne!(second, first, "second claim reused the first generation");

    assert!(
        !owner.release(first),
        "the displaced handler was allowed to release the new owner's feed"
    );
    assert!(
        owner.is_streaming(),
        "the displaced handler cleared the streaming flag under the new owner"
    );
    assert_eq!(
        owner.current(),
        Some(second),
        "the displaced handler took the registry entry with it"
    );

    assert!(
        owner.release(second),
        "the current owner could not release its own feed"
    );
    assert!(
        !owner.is_streaming(),
        "streaming still set after the owner released"
    );
    assert_eq!(owner.current(), None, "the entry survived its own release");
}

/// Go's `stopCamStream` clears the flag and leaves the registry entry in place,
/// because the departing handler's own release is what deletes it and issues
/// the disable (`robot.go:136-144`).
#[test]
fn stop_clears_the_flag_and_keeps_the_owner() {
    let owner = CamOwner::new();
    let (generation, _) = owner.claim();

    owner.stop();
    assert!(!owner.is_streaming(), "stop left the streaming flag set");
    assert_eq!(
        owner.current(),
        Some(generation),
        "stop deleted the owner entry"
    );
    assert!(
        owner.release(generation),
        "the owner could not release after a stop"
    );
}
