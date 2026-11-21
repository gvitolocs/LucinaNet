/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#![crate_name = "swgl"]
#![crate_type = "lib"]

extern crate gleam;
extern crate lucinanet_raster;

mod rust_backend;
mod swgl_fns;
#[doc(hidden)]
pub mod replay;
pub mod trace;

pub use crate::swgl_fns::*;
/// Which rasterizer runs the GL calls: Rust by default; `LUCINANET_RASTER=swgl` (the C++ oracle) or [`select_backend`] before the first context.
pub use crate::rust_backend::{current as current_backend, select as select_backend, Backend};
