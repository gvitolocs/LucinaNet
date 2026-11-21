/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `glsl-to-rust <name>.vert <name>.frag`: prints the Rust module for one program, like `glsl-to-cxx`.

use glsl_to_rust::translate;
fn main() {
    println!("{}", translate(&mut std::env::args()));
}
