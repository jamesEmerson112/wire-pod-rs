//! Stim stream ownership. Ports of Go tests 5 and 6,
//! `TestClaimEventStreamRefusesWhileOwned` (`sdkapp_test.go:338-360`) and
//! `TestSupersededReceiverCannotWriteStimState` (`sdkapp_test.go:364-390`).
//!
//! No runtime: the state machine is synchronous by design.

use wirepod_core::{EventOwner, StimSample};

/// The double click: re-selecting Stim must reuse the running stream rather
/// than start a second one, and a begin arriving straight after a stop must not
/// be turned away.
#[test]
fn claim_refuses_while_owned_and_is_admitted_after_a_stop() {
    let owner = EventOwner::new();

    assert!(owner.claim().is_some(), "first claim refused");
    assert!(owner.is_streaming(), "the claim did not mark the stream");

    assert!(
        owner.claim().is_none(),
        "second claim was admitted while the stream was already owned"
    );

    owner.stop();
    assert!(!owner.is_streaming(), "stop left the stream marked");

    assert!(
        owner.claim().is_some(),
        "claim refused after a stop; a begin right after a stop must not be turned away"
    );
}

#[test]
fn a_superseded_receiver_cannot_write_stim() {
    let owner = EventOwner::new();

    let old = owner.claim().expect("first claim refused");
    assert!(
        owner.write_stim(old, StimSample::new(0.25, 1.0)),
        "the owner's write was refused"
    );
    assert_eq!(owner.stim().value, 0.25, "owner write did not land");

    owner.stop();
    // Go zeroes the value in the same critical section as the release
    // (`robot.go:258`). The Go test never observes it, so this port does.
    assert_eq!(
        owner.stim(),
        StimSample::ZERO,
        "stop left a stale reading behind"
    );

    let new = owner.claim().expect("could not claim after stop");
    assert!(
        owner.write_stim(new, StimSample::new(0.75, 1.0)),
        "the new owner's write was refused"
    );

    // The superseded receiver tries to publish one last reading.
    assert!(
        !owner.write_stim(old, StimSample::new(0.1, 1.0)),
        "a superseded receiver's write was accepted"
    );
    assert_eq!(
        owner.stim().value,
        0.75,
        "a superseded receiver overwrote the current owner's value"
    );
}
