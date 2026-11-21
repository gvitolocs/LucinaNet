/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The decisions Verus checks, as shipped (scripts/verify-shipped.sh): the buffering flow, the A/V clock, MSE buffered ranges
//! and the video caps.
pub mod buffered;
pub mod caps;
pub mod clock;
pub mod flow;
