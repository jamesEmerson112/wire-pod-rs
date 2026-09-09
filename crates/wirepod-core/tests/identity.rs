//! ESN normalization and the ownership generation counter.

use std::collections::HashSet;

use wirepod_core::{CamOwner, Esn, EventOwner, Generation};

#[test]
fn esn_is_trimmed_and_lowercased() {
    assert_eq!(Esn::new("  00E20100\t\n").as_str(), "00e20100");
    assert_eq!(Esn::new("00E20100"), Esn::new(" 00e20100 "));
    assert_eq!(Esn::new(" 00E20100 ").to_string(), "00e20100");
    assert!(Esn::new("   ").is_empty());

    // The normalization has to reach Hash too, because the meters and the
    // registry are ESN-keyed maps standing in for the Go EqualFold lookups.
    let mut keys = HashSet::new();
    keys.insert(Esn::new("00E20100"));
    assert!(keys.contains(&Esn::new(" 00e20100 ")));
}

#[test]
fn the_first_issued_generation_is_one_and_generations_order() {
    assert_eq!(Generation::first(), Generation::UNCLAIMED.next());
    assert!(!Generation::UNCLAIMED.is_claimed());
    assert!(Generation::first().is_claimed());
    assert!(Generation::first() < Generation::first().next());

    let cam = CamOwner::new();
    let (first, _) = cam.claim();
    assert_eq!(first, Generation::first());
    let (second, _) = cam.claim();
    assert_eq!(second, first.next());

    let events = EventOwner::new();
    let first = events.claim().expect("first claim refused");
    assert_eq!(first, Generation::first());
    events.stop();
    let second = events.claim().expect("claim after stop refused");
    assert_eq!(second, first.next());
}
