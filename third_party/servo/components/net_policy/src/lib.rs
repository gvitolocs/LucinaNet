/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The connector's rules as pure integer functions, so Verus can read the very files (`//@` lines hold the proof, see
//! `verify.sh` and `mutants.sh`). The Servo `net` crate keeps only the glue: sockets, locks and wakeups.
//! Numbers and where each comes from: LucinaNet `docs/specs/connector.md`.
pub mod dns;
pub mod slots;
