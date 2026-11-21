//! The round trip (`common/roundtrip.rs`) on swgl's C++ backend.

mod common;

#[test]
fn replay_matches_live() {
    common::roundtrip::replay_matches_live(swgl::Backend::Cxx);
}
