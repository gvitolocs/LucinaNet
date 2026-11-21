/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Page zoom for LucinaNet's browser: the level tables of Firefox and Chromium, the Ctrl +/-/0 step
//! rules, and the memory of what level each site was left at.
//!
//! Servo already applies zoom (`WebView::set_page_zoom`, feature map "Zoom"); what was missing is the
//! decision of *which* levels to offer and how to move between them, and where a site's level is kept.
//! That is all of this crate, and nothing else: no engine calls, no UI, no file I/O.
//!
//! - [`levels`]: [`Profile`], the table of the profile ([`levels::levels`], [`levels::min`],
//!   [`levels::max`]), the steps ([`levels::zoom_in`], [`levels::zoom_out`], [`levels::reset`]), the
//!   helpers ([`levels::snap`], [`levels::clamp`]) and the two forms a level is shown or applied in
//!   ([`levels::factor`] for `set_page_zoom`, [`levels::label`] for the toolbar).
//! - [`site`]: [`site::site_key`] (the host, so a scheme or a port does not make a new site),
//!   [`SiteZoom`] - the rows, the default zoom setting, and their JSON form.
//!
//! Pure and deterministic: the same table and the same keystrokes always give the same level. Persistence
//! is [`SiteZoom::to_json`] and [`SiteZoom::from_json`] between values and text; the file itself is the
//! wiring piece that follows.
//!
//! Behaviour follows the originals at the pinned revisions, with the differences recorded in
//! `PORTING.md` and the Chromium notice in `NOTICE`.

#![forbid(unsafe_code)]

pub mod levels;
pub mod site;

pub use levels::DEFAULT;
pub use levels::Profile;
pub use levels::Percent;
pub use levels::clamp;
pub use levels::factor;
pub use levels::label;
pub use levels::levels;
pub use levels::max;
pub use levels::min;
pub use levels::reset;
pub use levels::snap;
pub use levels::zoom_in;
pub use levels::zoom_out;
pub use site::SiteZoom;
pub use site::ZoomError;
pub use site::site_key;
