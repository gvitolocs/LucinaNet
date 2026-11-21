/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::ops::Range;
use std::time::Duration;

extern crate servo_media_streams as streams;
extern crate servo_media_traits;

pub mod audio;
pub mod context;
pub mod metadata;
pub mod mse;
pub mod video;

use serde::{Deserialize, Serialize};
use servo_base::generic_channel::{self, GenericSender};
use servo_media_traits::MediaInstance;
use streams::registry::MediaStreamId;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PlaybackState {
    Stopped,
    Buffering,
    Paused,
    Playing,
}

#[derive(Debug, PartialEq)]
pub enum PlayerError {
    /// Backend specific error.
    Backend(String),
    /// Could not push buffer contents to the player.
    BufferPushFailed,
    /// The player cannot consume more data.
    EnoughData,
    /// Setting End Of Stream failed.
    EOSFailed,
    /// The media stream is not seekable.
    NonSeekableStream,
    /// Tried to seek out of range.
    SeekOutOfRange,
    /// Setting an audio or video stream failed.
    /// Possibly because the type of source is not PlayerSource::Stream.
    SetStreamFailed,
    // Setting an audio or video track failed.
    SetTrackFailed,
}

pub type SeekLockMsg = (bool, GenericSender<()>);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SeekLock {
    pub lock_channel: GenericSender<SeekLockMsg>,
}

impl SeekLock {
    pub fn unlock(&self, result: bool) {
        let (ack_sender, ack_recv) =
            generic_channel::channel::<()>().expect("Could not create IPC channel");
        self.lock_channel.send((result, ack_sender)).unwrap();
        ack_recv.recv().unwrap()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum PlayerEvent {
    EndOfStream,
    /// The player has enough data. The client should stop pushing data into.
    EnoughData,
    Error(String),
    VideoFrameUpdated,
    MetadataUpdated(metadata::Metadata),
    // The `None` value means the duration is unknown, in which case this is likely a live stream.
    DurationChanged(Option<Duration>),
    /// The internal player queue is running out of data. The client should start
    /// pushing more data.
    NeedData,
    PositionChanged(f64),
    /// The player needs the data to perform a seek to the given offset in bytes.
    /// The next push_data should get the buffers from the new offset.
    /// The player will be blocked until the user unlocks it through
    /// the given SeekLock instance.
    /// This event is only received for seekable stream types.
    SeekData(u64, SeekLock),
    /// The player has performed a seek to the given time offset in seconds.
    SeekDone(f64),
    StateChanged(PlaybackState),
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum StreamType {
    /// No seeking is supported in the stream, such as a live stream.
    Stream,
    /// The stream is seekable.
    Seekable,
}

pub trait Player: Send + MediaInstance {
    fn play(&self) -> Result<(), PlayerError>;
    fn pause(&self) -> Result<(), PlayerError>;
    fn paused(&self) -> bool;
    fn can_resume(&self) -> bool;
    fn stop(&self) -> Result<(), PlayerError>;
    fn seek(&self, time: f64) -> Result<(), PlayerError>;
    fn seekable(&self) -> Vec<Range<f64>>;
    fn set_mute(&self, muted: bool) -> Result<(), PlayerError>;
    fn muted(&self) -> bool;
    fn set_volume(&self, volume: f64) -> Result<(), PlayerError>;
    fn volume(&self) -> f64;
    fn set_input_size(&self, size: u64) -> Result<(), PlayerError>;
    fn set_seekable(&self, seekable: bool) -> Result<(), PlayerError>;
    fn set_download_buffering_enabled(&self, enabled: bool) -> Result<(), PlayerError>;
    fn set_playback_rate(&self, playback_rate: f64) -> Result<(), PlayerError>;
    fn playback_rate(&self) -> f64;
    fn push_data(&self, data: Vec<u8>) -> Result<(), PlayerError>;
    fn end_of_stream(&self) -> Result<(), PlayerError>;
    /// Get the list of time ranges in seconds that have been buffered.
    fn buffered(&self) -> Vec<Range<f64>>;
    /// Set the stream to be played by the player.
    /// Only a single stream of the same type (audio or video) can be set.
    /// Subsequent calls with a stream of the same type will override the previously
    /// set stream.
    /// This method requires the player to be constructed with StreamType::Stream.
    /// It is important to give the correct value of `only_stream` indicating
    /// that the audio or video stream being set is the only one expected.
    /// Subsequent calls to `set_stream` after the `only_stream` flag has been
    /// set to true will fail.
    fn set_stream(&self, stream: &MediaStreamId, only_stream: bool) -> Result<(), PlayerError>;
    /// If player's rendering draws using GL textures
    fn render_use_gl(&self) -> bool;
    fn set_audio_track(&self, stream_index: i32, enabled: bool) -> Result<(), PlayerError>;
    fn set_video_track(&self, stream_index: i32, enabled: bool) -> Result<(), PlayerError>;

    /// LucinaNet: the video frames this player has shown or dropped so far, and of those how many
    /// were dropped, for `HTMLVideoElement.getVideoPlaybackQuality()`. `None` where the backend does
    /// not count them.
    fn playback_quality(&self) -> Option<(u64, u64)> {
        None
    }

    // Media Source Extensions (LucinaNet), see `mse`. A backend without them keeps these defaults.
    /// This player gets its media from a MediaSource from now on, never through `push_data`.
    fn mse_attach(&self) -> Result<(), mse::MseError> {
        Err(mse::MseError::NoMse)
    }
    fn mse_add_source_buffer(&self, _mime: &str) -> Result<mse::SourceBufferId, mse::MseError> {
        Err(mse::MseError::NoMse)
    }
    fn mse_remove_source_buffer(&self, _id: mse::SourceBufferId) {}
    /// The buffer append algorithm on `data` (prepare append's coded frame eviction included, which may refuse with
    /// `QuotaExceeded` before anything is parsed). `current_time` is the media element's for eviction.
    fn mse_append(
        &self,
        _id: mse::SourceBufferId,
        _data: Vec<u8>,
        _current_time: f64,
    ) -> Result<mse::AppendResult, mse::MseError> {
        Err(mse::MseError::NoMse)
    }
    /// Coded frame removal of `[start, end)`; answers the new `buffered`.
    fn mse_remove(&self, _id: mse::SourceBufferId, _start: f64, _end: f64) -> Vec<Range<f64>> {
        Vec::new()
    }
    /// The reset parser state algorithm (`abort()`, a parse error, `changeType`).
    fn mse_reset_parser(&self, _id: mse::SourceBufferId) {}
    fn mse_set_mode(&self, _id: mse::SourceBufferId, _mode: mse::AppendMode) {}
    fn mse_set_timestamp_offset(&self, _id: mse::SourceBufferId, _offset: f64) {}
    fn mse_set_append_window(&self, _id: mse::SourceBufferId, _start: f64, _end: f64) {}
    /// `changeType(type)`: the next initialization segment may be of this type.
    fn mse_change_type(&self, _id: mse::SourceBufferId, _mime: &str) -> Result<(), mse::MseError> {
        Err(mse::MseError::NoMse)
    }
    fn mse_buffered(&self, _id: mse::SourceBufferId) -> Vec<Range<f64>> {
        Vec::new()
    }
    /// `MediaSource.duration` changed (also from the duration change algorithm); infinity for a live stream, NaN unknown.
    fn mse_set_duration(&self, _duration: f64) {}
    /// `endOfStream()` without an error: the last frames are the end; `buffered` extends to the highest end time.
    fn mse_end_of_stream(&self) {}
    /// `ended` left again by an append or a remove.
    fn mse_reopen(&self) {}
}
