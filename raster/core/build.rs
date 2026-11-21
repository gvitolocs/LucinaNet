/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Translates WebRender's shaders (swgl/res, the shader list of swgl's build.rs) into one Rust module per program
// with raster/glsl-to-rust, as swgl's build.rs does with glsl-to-cxx; `crate::shaders` includes the result.
fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let shader_dir = manifest.join("../../swgl/res");
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("shaders");
    std::fs::create_dir_all(&out_dir).unwrap();
    for source in glsl_to_rust::build::write_shaders(&shader_dir, &out_dir) {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../glsl-to-rust/src");
}
