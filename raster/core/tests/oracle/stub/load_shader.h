/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Test-only stand-in for the load_shader.h that swgl's build generates. gl.cc declares load_shader(name) itself;
// shim_t.cc defines it after including gl.cc, with one hand-written program ("flat") for the rasterizer oracle
// (tests/rasterize_flat.rs) and nothing for any other name.
