/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! What our side of the engine seam decides from bits and numbers it is handed. Every file here is checked by Verus as it ships
//! (`scripts/verify-shipped.sh`), and `scripts/boundary-mutants.sh` shows the proofs refuse planted bugs.

pub mod bounds;
pub mod id_bits;
pub mod roots;
pub mod value_bits;
