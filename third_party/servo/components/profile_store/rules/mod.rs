/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! What must hold whatever is in a file or a page: Verus checks these files themselves,
//! their proof being in `//@` lines; LucinaNet `scripts/verify-shipped.sh` runs it, and
//! `verification/*/mutants.sh` plants bugs that it must refuse.

pub mod bounds;
pub mod evict;
