/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Passes the target triple to `src/build.rs`, which runs cc outside build scripts too (tests, the binary).
fn main() {
    println!("cargo:rustc-env=GLSL_TO_RUST_TARGET={}", std::env::var("TARGET").unwrap());
    println!("cargo:rerun-if-changed=build.rs");
}
