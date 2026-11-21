//! The round trip (`common/roundtrip.rs`) on the Rust backend (`LUCINANET_RASTER=rust`). A test binary of its own,
//! since the backend is chosen once per process.

mod common;

#[test]
fn replay_matches_live() {
    common::roundtrip::replay_matches_live(swgl::Backend::Rust);
}
