// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! lucina-fontsan: a pure-Rust replacement for fontsan (OTS) in LucinaNet's
//! Servo. Decodes WOFF/WOFF2, applies OTS's accept/reject decisions and table
//! whitelist, and emits a sanitized sfnt (or ttcf) with bounded work on
//! hostile input. Behaviour mirrors OTS as vendored in fontsan 0.7.0.

mod bounds;
mod buf;
mod font;
mod tables;
mod woff;
mod woff2;

use std::fmt;

/// Error returned by [`process`]. Servo only matches Ok/Err and logs `{:?}`.
#[derive(Debug)]
pub enum Error {
    /// The input is not a font OTS would accept; the string says what was wrong.
    InvalidFont(&'static str),
    /// The sanitized output would exceed the 300 MiB output limit
    /// (ots_glue.cc stream size).
    TooLarge(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidFont(m) => write!(f, "invalid font: {}", m),
            Error::TooLarge(m) => write!(f, "output too large: {}", m),
        }
    }
}

impl std::error::Error for Error {}

/// Sanitize a downloaded web font or FontFace ArrayBuffer, as
/// `fontsan::process` does in Servo's font_context.
pub fn process(data: &[u8]) -> Result<Vec<u8>, Error> {
    if data.len() < 4 {
        return Err(Error::InvalidFont("file less than 4 bytes"));
    }
    if &data[0..4] == b"wOFF" {
        woff::process_woff(data)
    } else if &data[0..4] == b"wOF2" {
        woff2::process_woff2(data)
    } else if &data[0..4] == b"ttcf" {
        font::process_ttc(data, None)
    } else {
        font::process_ttf(data, 0)
    }
}
