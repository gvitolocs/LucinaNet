/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Media Source Extensions: what a player fed by a `MediaSource` is asked by script.
//!
//! The DOM (MediaSource, SourceBuffer) keeps the specification's state, events and task queueing; the backend keeps the
//! byte stream parsers and the track buffers (W3C Media Source Extensions, 3.5.8 coded frame processing and the algorithms
//! around it) and plays from them. Every method has a default that refuses, so a backend without MSE (GStreamer, OHOS, the
//! dummy) is unchanged and `MediaSource.isTypeSupported` answers false through `can_play_type` there. The methods are on
//! [`crate::Player`], prefixed `mse_`.
//!
//! Times are seconds as in the specification; `f64::INFINITY` is the append window's open end and an unknown duration.

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// A SourceBuffer, as the backend numbers it.
pub type SourceBufferId = usize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum AppendMode {
    Segments,
    Sequence,
}

/// A track an initialization segment announced.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MseTrack {
    /// The byte stream's track id.
    pub id: u64,
    pub video: bool,
    /// As in a `codecs=` parameter: "avc1.64001f", "mp4a.40.2", "vp09", "opus".
    pub codec: String,
}

/// What one `appendBuffer` did, for the steps the DOM runs after it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct AppendResult {
    /// An initialization segment was parsed: the initialization segment received algorithm runs with these tracks.
    pub init_segment: Option<Vec<MseTrack>>,
    /// The duration the initialization segment states, if any (the spec's "duration ... from the initialization segment").
    pub init_duration: Option<f64>,
    /// Coded frames were added.
    pub added_frames: bool,
    /// `buffered` after the append.
    pub buffered: Vec<Range<f64>>,
    /// `timestampOffset` after the append ("sequence" mode moves it).
    pub timestamp_offset: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum MseError {
    /// The MIME type or a codec is not supported (NotSupportedError).
    NotSupported,
    /// The append error algorithm, with the parser's reason.
    Decode(String),
    /// Coded frame eviction could not make room (QuotaExceededError).
    QuotaExceeded,
    /// This backend has no MSE.
    NoMse,
}
