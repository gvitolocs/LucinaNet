/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use js::context::JSContext;
use script_bindings::reflector::{Reflector, reflect_dom_object};

use crate::dom::bindings::codegen::Bindings::PerformanceBinding::DOMHighResTimeStamp;
use crate::dom::bindings::codegen::Bindings::VideoPlaybackQualityBinding::VideoPlaybackQualityMethods;
use crate::dom::bindings::num::Finite;
use crate::dom::bindings::root::DomRoot;
use crate::dom::window::Window;

/// <https://w3c.github.io/media-playback-quality/#videoplaybackquality-interface>
///
/// LucinaNet: the counts come from the media backend (`Player::playback_quality`), so that a page
/// such as YouTube's player can see frames being dropped and step its quality down.
#[dom_struct]
pub(crate) struct VideoPlaybackQuality {
    reflector_: Reflector,
    creation_time: f64,
    dropped_video_frames: u32,
    total_video_frames: u32,
}

impl VideoPlaybackQuality {
    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        creation_time: DOMHighResTimeStamp,
        total_video_frames: u32,
        dropped_video_frames: u32,
    ) -> DomRoot<VideoPlaybackQuality> {
        reflect_dom_object(
            cx,
            Box::new(VideoPlaybackQuality {
                reflector_: Reflector::new(),
                creation_time: *creation_time,
                dropped_video_frames,
                total_video_frames,
            }),
            window,
        )
    }
}

impl VideoPlaybackQualityMethods<crate::DomTypeHolder> for VideoPlaybackQuality {
    /// <https://w3c.github.io/media-playback-quality/#dom-videoplaybackquality-creationtime>
    fn CreationTime(&self) -> DOMHighResTimeStamp {
        Finite::wrap(self.creation_time)
    }

    /// <https://w3c.github.io/media-playback-quality/#dom-videoplaybackquality-droppedvideoframes>
    fn DroppedVideoFrames(&self) -> u32 {
        self.dropped_video_frames
    }

    /// <https://w3c.github.io/media-playback-quality/#dom-videoplaybackquality-totalvideoframes>
    fn TotalVideoFrames(&self) -> u32 {
        self.total_video_frames
    }

    /// <https://w3c.github.io/media-playback-quality/#dom-videoplaybackquality-corruptedvideoframes>
    fn CorruptedVideoFrames(&self) -> u32 {
        0
    }
}
