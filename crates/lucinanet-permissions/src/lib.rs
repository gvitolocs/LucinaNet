/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Site permissions: what is stored for an origin and feature, whether a request must prompt, be allowed or be
//! blocked, what a prompt answer changes (including the embargo after repeated dismissals), and the file format.
//! Pure: times arrive as arguments, nothing here reads a clock or touches a file.

#![forbid(unsafe_code)]

mod embargo;
mod feature;
mod origin;
mod store;

pub use feature::{Feature, Setting};
pub use origin::origin_of;
pub use store::{Answer, Decision, Entry, Expiry, Permissions, PermissionsError};
