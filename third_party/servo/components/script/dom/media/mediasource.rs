/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! <https://www.w3.org/TR/media-source-2/#mediasource>
//!
//! The DOM side of Media Source Extensions: the specification's state, checks, events and task
//! queueing. The byte stream parsers and the track buffers are the player's
//! (`servo_media::player::mse`), reached through the media element this MediaSource is attached to.

use std::cell::Cell;
use std::sync::{Arc, Mutex};

use dom_struct::dom_struct;
use js::context::JSContext;
use js::rust::HandleObject;
use script_bindings::reflector::reflect_dom_object_with_proto;
use servo_media::player::Player;
use servo_media::player::mse::MseError;
use servo_media::{ServoMedia, SupportsMediaType};
use stylo_atoms::Atom;

use crate::dom::bindings::codegen::Bindings::HTMLMediaElementBinding::HTMLMediaElementMethods;
use crate::dom::bindings::codegen::Bindings::MediaSourceBinding::{
    EndOfStreamError, MediaSourceMethods, ReadyState,
};
use crate::dom::bindings::error::{Error, ErrorResult, Fallible};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::num::Finite;
use crate::dom::bindings::refcounted::Trusted;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{Dom, DomRoot, MutNullableDom};
use crate::dom::bindings::str::DOMString;
use crate::dom::eventtarget::EventTarget;
use crate::dom::html::embedded_content::media_command;
use crate::dom::html::htmlmediaelement::HTMLMediaElement;
use crate::dom::media::sourcebuffer::SourceBuffer;
use crate::dom::media::sourcebufferlist::SourceBufferList;
use crate::dom::window::Window;

/// Chromium accepts one audio and one video SourceBuffer plus one more; a fourth is a
/// QuotaExceededError.
const MAX_SOURCE_BUFFERS: usize = 3;

/// Queues a task to fire a simple event named `name` at `target`. MSE leaves the task source open;
/// the media element task source keeps these events ordered with the element's own.
pub(crate) fn queue_mse_event(target: &EventTarget, name: &'static str) {
    let target_ = Trusted::new(target);
    target
        .global()
        .task_manager()
        .media_element_task_source()
        .queue(task!(mse_event: move |cx| {
            target_.root().fire_event(cx, Atom::from(name));
        }));
}

/// The container types this implementation's byte stream formats cover: ISO BMFF and WebM.
fn is_mse_container(mime: &str) -> bool {
    let essence = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        essence.as_str(),
        "video/mp4" | "audio/mp4" | "video/webm" | "audio/webm"
    )
}

/// <https://www.w3.org/TR/media-source-2/#dom-mediasource-istypesupported>
pub(crate) fn is_type_supported(mime: &str) -> bool {
    // Step 1. If type is an empty string, then return false.
    // Step 2. If type does not contain a valid MIME type string, then return false.
    // Step 3-4. If type contains a media type or media subtype that the MediaSource does not
    // support, or a codec the MediaSource does not support, then return false.
    // Step 5. Return true.
    !mime.trim().is_empty()
        && is_mse_container(mime)
        && ServoMedia::get().can_play_type(mime) != SupportsMediaType::No
}

#[dom_struct]
pub(crate) struct MediaSource {
    eventtarget: EventTarget,
    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-readystate>
    ready_state: Cell<ReadyState>,
    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-duration>
    duration: Cell<f64>,
    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-sourcebuffers>
    source_buffers: Dom<SourceBufferList>,
    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-activesourcebuffers>
    active_source_buffers: Dom<SourceBufferList>,
    /// The media element this MediaSource is attached to, while it is.
    media_element: MutNullableDom<HTMLMediaElement>,
    /// <https://www.w3.org/TR/media-source-2/#dfn-live-seekable-range>
    ///
    /// Kept, but the media element's `seekable` does not use it yet.
    /// Start and end of the live seekable range; NaN when there is none.
    live_seekable_start: Cell<f64>,
    live_seekable_end: Cell<f64>,
}

impl MediaSource {
    fn new_inherited(
        source_buffers: &SourceBufferList,
        active_source_buffers: &SourceBufferList,
    ) -> MediaSource {
        MediaSource {
            eventtarget: EventTarget::new_inherited(),
            ready_state: Cell::new(ReadyState::Closed),
            duration: Cell::new(f64::NAN),
            source_buffers: Dom::from_ref(source_buffers),
            active_source_buffers: Dom::from_ref(active_source_buffers),
            media_element: Default::default(),
            live_seekable_start: Cell::new(f64::NAN),
            live_seekable_end: Cell::new(f64::NAN),
        }
    }

    fn new_with_proto(
        cx: &mut JSContext,
        window: &Window,
        proto: Option<HandleObject>,
    ) -> DomRoot<MediaSource> {
        let source_buffers = SourceBufferList::new(cx, window);
        let active_source_buffers = SourceBufferList::new(cx, window);
        reflect_dom_object_with_proto(
            cx,
            Box::new(MediaSource::new_inherited(
                &source_buffers,
                &active_source_buffers,
            )),
            window,
            proto,
        )
    }

    pub(crate) fn ready_state(&self) -> ReadyState {
        self.ready_state.get()
    }

    pub(crate) fn media_element(&self) -> Option<DomRoot<HTMLMediaElement>> {
        self.media_element.get()
    }

    /// The player of the attached media element.
    fn player(&self) -> Option<Arc<Mutex<dyn Player>>> {
        self.media_element.get()?.player()
    }

    /// Runs `f` on the attached media element's player; `None` when there is none.
    pub(crate) fn with_player<R>(&self, f: impl FnOnce(&dyn Player) -> R) -> Option<R> {
        let player = self.player()?;
        let guard = player.lock().ok()?;
        Some(f(&*guard))
    }

    /// <https://www.w3.org/TR/media-source-2/#mediasource-attach>
    ///
    /// The steps for a MediaSource whose readyState is "closed", once the element's player took it
    /// (`Player::mse_attach`); the element runs step 1 and the rest of the resource fetch
    /// algorithm.
    pub(crate) fn attach(&self, element: &HTMLMediaElement) {
        self.media_element.set(Some(element));

        // Step 2. Set the readyState attribute to "open".
        self.ready_state.set(ReadyState::Open);

        // Step 3. Queue a task to fire an event named sourceopen at the MediaSource.
        queue_mse_event(self.upcast(), "sourceopen");
    }

    /// <https://www.w3.org/TR/media-source-2/#mediasource-detach>
    pub(crate) fn detach(&self) {
        // LucinaNet: observation only. `closed` is produced here and nowhere else at
        // runtime, and the element has not yet paused or reset its position, so a close while
        // the page was playing (the `media-closed-while-playing` signature) is visible in the product log.
        if let Some(element) = self.media_element.get() {
            media_command::log_closed_while_playing(
                element.Paused(),
                element.Ended(),
                *element.CurrentTime(),
                true,
                element.ReadyState(),
                self.source_buffers.buffers().len(),
                self.active_source_buffers.buffers().len(),
            );
        }

        // Step 1. Set the readyState attribute to "closed".
        self.ready_state.set(ReadyState::Closed);

        // Step 2. Update duration to NaN.
        self.duration.set(f64::NAN);

        // Step 3. Remove all the SourceBuffer objects from activeSourceBuffers.
        self.active_source_buffers.clear();

        // Step 4. Queue a task to fire an event named removesourcebuffer at activeSourceBuffers.
        queue_mse_event(self.active_source_buffers.upcast(), "removesourcebuffer");

        // Step 5. Remove all the SourceBuffer objects from sourceBuffers.
        let buffers = self.source_buffers.buffers();
        self.source_buffers.clear();
        for buffer in &buffers {
            let id = buffer.id();
            self.with_player(|player| player.mse_remove_source_buffer(id));
            buffer.removed_from_parent();
        }

        // Step 6. Queue a task to fire an event named removesourcebuffer at sourceBuffers.
        queue_mse_event(self.source_buffers.upcast(), "removesourcebuffer");

        // Step 7. Queue a task to fire an event named sourceclose at the MediaSource.
        queue_mse_event(self.upcast(), "sourceclose");

        self.media_element.set(None);
    }

    /// The prepare append algorithm, the remove(), mode and timestampOffset setters and
    /// changeType() all run these steps: "If the readyState attribute of the parent media source is
    /// in the "ended" state then run the following steps: Set the readyState attribute of the
    /// parent media source to "open". Queue a task to fire an event named sourceopen at the parent
    /// media source."
    pub(crate) fn reopen_if_ended(&self) {
        if self.ready_state.get() != ReadyState::Ended {
            return;
        }
        self.ready_state.set(ReadyState::Open);
        self.with_player(|player| player.mse_reopen());
        queue_mse_event(self.upcast(), "sourceopen");
    }

    /// The largest end time of the track buffer ranges of every SourceBuffer in sourceBuffers; also
    /// stands for the "highest presentation timestamp of any buffered coded frames", which the
    /// backend does not report apart.
    fn highest_end_time(&self) -> f64 {
        self.source_buffers
            .buffers()
            .iter()
            .filter_map(|buffer| {
                let id = buffer.id();
                self.with_player(|player| player.mse_buffered(id))
            })
            .flat_map(|ranges| ranges.into_iter().map(|range| range.end))
            .fold(0., f64::max)
    }

    /// The initialization segment received algorithm's step 1. In the specification it runs before
    /// the segment's coded frames exist; here the player has parsed them already, so this does not
    /// compare the new duration with the buffered media as the duration change algorithm would.
    pub(crate) fn set_duration_from_initialization_segment(&self, new_duration: f64) {
        self.duration.set(new_duration);
        self.with_player(|player| player.mse_set_duration(new_duration));
    }

    /// Coded frame processing, its last step: if the media segment contains data beyond the current
    /// duration, run the duration change algorithm with new duration set to the maximum of the
    /// current duration and the group end timestamp (the highest end time buffered).
    pub(crate) fn extend_duration_to_buffered_media(&self) {
        let duration = self.duration.get();
        let highest_end_time = self.highest_end_time();
        if !duration.is_nan() && highest_end_time > duration {
            let _ = self.duration_change(highest_end_time);
        }
    }

    /// <https://www.w3.org/TR/media-source-2/#duration-change-algorithm>
    pub(crate) fn duration_change(&self, new_duration: f64) -> ErrorResult {
        // Step 1. If the current value of duration is equal to new duration, then return.
        let duration = self.duration.get();
        if duration == new_duration || (duration.is_nan() && new_duration.is_nan()) {
            return Ok(());
        }

        // Step 2. If new duration is less than the highest presentation timestamp of any buffered
        // coded frames for all SourceBuffer objects in sourceBuffers, then throw an
        // InvalidStateError exception and abort these steps.
        let highest_end_time = self.highest_end_time();
        if new_duration < highest_end_time {
            return Err(Error::InvalidState(Some(
                "The new duration is lower than the buffered media".into(),
            )));
        }

        // Step 3. Let highest end time be the largest track buffer ranges end time across all the
        // track buffers in all SourceBuffer objects in sourceBuffers.
        // Step 4. If new duration is less than highest end time, then update new duration to equal
        // highest end time.
        // (Step 2 already refused such a value, as the two times are the same here.)

        // Step 5. Update duration to new duration.
        self.duration.set(new_duration);

        // Step 6. Update the media.duration to new duration and run the HTMLMediaElement duration
        // change algorithm. The player sends DurationChanged to the element, which runs it.
        self.with_player(|player| player.mse_set_duration(new_duration));
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#end-of-stream-algorithm>
    pub(crate) fn end_of_stream(&self, error: Option<EndOfStreamError>) {
        // Step 1. Change the readyState attribute value to "ended".
        self.ready_state.set(ReadyState::Ended);

        // Step 2. Queue a task to fire an event named sourceended at the MediaSource.
        queue_mse_event(self.upcast(), "sourceended");

        match error {
            // Step 3. If error is not set:
            None => {
                // Step 3.1. Run the duration change algorithm with new duration set to the largest
                // track buffer ranges end time across all the track buffers in all SourceBuffer
                // objects in sourceBuffers. That value is never below the buffered media, so this
                // cannot throw.
                let _ = self.duration_change(self.highest_end_time());

                // Step 3.2. Notify the media element that it now has all of the media data.
                self.with_player(|player| player.mse_end_of_stream());
            },
            // If error is set to "network" / "decode": the media element's error steps for its
            // readyState.
            Some(error) => {
                if let Some(element) = self.media_element.get() {
                    element.media_source_end_of_stream_error(error == EndOfStreamError::Decode);
                }
            },
        }
    }

    /// The initialization segment received algorithm, steps 5 and 7: the buffer joins
    /// activeSourceBuffers.
    pub(crate) fn activate_source_buffer(&self, buffer: &SourceBuffer) {
        if self.active_source_buffers.contains(buffer) {
            return;
        }
        // activeSourceBuffers keeps the order of sourceBuffers.
        let ordered: Vec<_> = self
            .source_buffers
            .buffers()
            .into_iter()
            .filter(|b| &**b == buffer || self.active_source_buffers.contains(b))
            .collect();
        self.active_source_buffers.clear();
        for b in &ordered {
            self.active_source_buffers.push(b);
        }
        // Queue a task to fire an event named addsourcebuffer at activeSourceBuffers.
        queue_mse_event(self.active_source_buffers.upcast(), "addsourcebuffer");
    }

    fn any_source_buffer_updating(&self) -> bool {
        self.source_buffers.buffers().iter().any(|b| b.updating())
    }
}

impl MediaSourceMethods<crate::DomTypeHolder> for MediaSource {
    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-constructor>
    fn Constructor(
        cx: &mut JSContext,
        window: &Window,
        proto: Option<HandleObject>,
    ) -> DomRoot<MediaSource> {
        MediaSource::new_with_proto(cx, window, proto)
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-sourcebuffers>
    fn SourceBuffers(&self) -> DomRoot<SourceBufferList> {
        DomRoot::from_ref(&*self.source_buffers)
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-activesourcebuffers>
    fn ActiveSourceBuffers(&self) -> DomRoot<SourceBufferList> {
        DomRoot::from_ref(&*self.active_source_buffers)
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-readystate>
    fn ReadyState(&self) -> ReadyState {
        self.ready_state.get()
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-duration>
    fn Duration(&self) -> f64 {
        // If the readyState attribute is "closed" then it MUST return NaN.
        if self.ready_state.get() == ReadyState::Closed {
            return f64::NAN;
        }
        self.duration.get()
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-duration>
    fn SetDuration(&self, value: f64) -> ErrorResult {
        // Step 1. If the value being set is negative or NaN then throw a TypeError exception and
        // abort these steps.
        if value.is_nan() || value < 0. {
            return Err(Error::Type(
                c"The duration must be a non-negative number".to_owned(),
            ));
        }

        // Step 2. If the readyState attribute is not "open" then throw an InvalidStateError
        // exception and abort these steps.
        if self.ready_state.get() != ReadyState::Open {
            return Err(Error::InvalidState(Some(
                "The MediaSource is not open".into(),
            )));
        }

        // Step 3. If the updating attribute equals true on any SourceBuffer in sourceBuffers, then
        // throw an InvalidStateError exception and abort these steps.
        if self.any_source_buffer_updating() {
            return Err(Error::InvalidState(Some(
                "A SourceBuffer is updating".into(),
            )));
        }

        // Step 4. Run the duration change algorithm with new duration set to the value being
        // assigned to this attribute.
        self.duration_change(value)
    }

    // <https://www.w3.org/TR/media-source-2/#dom-mediasource-onsourceopen>
    event_handler!(sourceopen, GetOnsourceopen, SetOnsourceopen);

    // <https://www.w3.org/TR/media-source-2/#dom-mediasource-onsourceended>
    event_handler!(sourceended, GetOnsourceended, SetOnsourceended);

    // <https://www.w3.org/TR/media-source-2/#dom-mediasource-onsourceclose>
    event_handler!(sourceclose, GetOnsourceclose, SetOnsourceclose);

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-canconstructindedicatedworker>
    fn CanConstructInDedicatedWorker(_window: &Window) -> bool {
        false
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-addsourcebuffer>
    fn AddSourceBuffer(
        &self,
        cx: &mut JSContext,
        type_: DOMString,
    ) -> Fallible<DomRoot<SourceBuffer>> {
        let mime = type_.str().to_string();

        // Step 1. If type is an empty string then throw a TypeError exception and abort these
        // steps.
        if mime.is_empty() {
            return Err(Error::Type(c"The type is empty".to_owned()));
        }

        // Step 2. If type contains a MIME type that is not supported or contains a MIME type that
        // is not supported with the types specified for the other SourceBuffer objects in
        // sourceBuffers, then throw a NotSupportedError exception and abort these steps.
        if !is_type_supported(&mime) {
            return Err(Error::NotSupported(Some(format!(
                "{mime} is not supported"
            ))));
        }

        // Step 3. If the user agent can't handle any more SourceBuffer objects or if creating a
        // SourceBuffer based on type would result in an unsupported SourceBuffer configuration,
        // then throw a QuotaExceededError exception and abort these steps.
        if self.source_buffers.len() >= MAX_SOURCE_BUFFERS {
            return Err(Error::QuotaExceeded {
                quota: None,
                requested: None,
            });
        }

        // Step 4. If the readyState attribute is not in the "open" state then throw an
        // InvalidStateError exception and abort these steps.
        if self.ready_state.get() != ReadyState::Open {
            return Err(Error::InvalidState(Some(
                "The MediaSource is not open".into(),
            )));
        }

        let id = match self.with_player(|player| player.mse_add_source_buffer(&mime)) {
            Some(Ok(id)) => id,
            Some(Err(MseError::QuotaExceeded)) => {
                return Err(Error::QuotaExceeded {
                    quota: None,
                    requested: None,
                });
            },
            Some(Err(_)) => {
                return Err(Error::NotSupported(Some(format!(
                    "{mime} is not supported"
                ))));
            },
            None => {
                return Err(Error::InvalidState(Some(
                    "The MediaSource is not attached".into(),
                )));
            },
        };

        // Step 5. Create a new SourceBuffer object and associated resources.
        // Step 6. Set the generate timestamps flag on the new object to the value in the "Generate
        // Timestamps Flag" column of the byte stream format registry entry that is associated with
        // type. (False for ISO BMFF and WebM.)
        // Step 7. If the generate timestamps flag equals true: Set the mode attribute on the new
        // object to "sequence". Otherwise: Set the mode attribute on the new object to "segments".
        let source_buffer = SourceBuffer::new(cx, self.global().as_window(), self, id, false);

        // Step 8. Add the new object to sourceBuffers and queue a task to fire an event named
        // addsourcebuffer at sourceBuffers.
        self.source_buffers.push(&source_buffer);
        queue_mse_event(self.source_buffers.upcast(), "addsourcebuffer");

        // Step 9. Return the new object.
        Ok(source_buffer)
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-removesourcebuffer>
    fn RemoveSourceBuffer(&self, source_buffer: &SourceBuffer) -> ErrorResult {
        // Step 1. If sourceBuffer specifies an object that is not in sourceBuffers then throw a
        // NotFoundError exception and abort these steps.
        if !self.source_buffers.contains(source_buffer) {
            return Err(Error::NotFound(None));
        }

        // Step 2. If the sourceBuffer.updating attribute equals true, then run the following steps:
        // Step 2.1. Abort the buffer append algorithm if it is running.
        // Step 2.2. Set the sourceBuffer.updating attribute to false.
        // Step 2.3. Queue a task to fire an event named abort at sourceBuffer.
        // Step 2.4. Queue a task to fire an event named updateend at sourceBuffer.
        source_buffer.abort_pending_update();

        // Steps 3-5. (The audio, video and text track lists of a SourceBuffer are not there yet.)

        // Step 6. If sourceBuffer is in activeSourceBuffers, then remove sourceBuffer from
        // activeSourceBuffers and queue a task to fire an event named removesourcebuffer at the
        // SourceBufferList returned by activeSourceBuffers.
        if self.active_source_buffers.remove(source_buffer) {
            queue_mse_event(self.active_source_buffers.upcast(), "removesourcebuffer");
        }

        // Step 7. Remove sourceBuffer from sourceBuffers and queue a task to fire an event named
        // removesourcebuffer at the SourceBufferList returned by sourceBuffers.
        self.source_buffers.remove(source_buffer);
        queue_mse_event(self.source_buffers.upcast(), "removesourcebuffer");

        // Step 8. Destroy all resources for sourceBuffer.
        let id = source_buffer.id();
        self.with_player(|player| player.mse_remove_source_buffer(id));
        source_buffer.removed_from_parent();
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-endofstream>
    fn EndOfStream(&self, error: Option<EndOfStreamError>) -> ErrorResult {
        // Step 1. If the readyState attribute is not in the "open" state then throw an
        // InvalidStateError exception and abort these steps.
        if self.ready_state.get() != ReadyState::Open {
            return Err(Error::InvalidState(Some(
                "The MediaSource is not open".into(),
            )));
        }

        // Step 2. If the updating attribute equals true on any SourceBuffer in sourceBuffers, then
        // throw an InvalidStateError exception and abort these steps.
        if self.any_source_buffer_updating() {
            return Err(Error::InvalidState(Some(
                "A SourceBuffer is updating".into(),
            )));
        }

        // Step 3. Run the end of stream algorithm with the error parameter set to error.
        self.end_of_stream(error);
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-setliveseekablerange>
    fn SetLiveSeekableRange(&self, start: Finite<f64>, end: Finite<f64>) -> ErrorResult {
        let (start, end) = (*start, *end);
        // Step 1. If the readyState attribute is not "open" then throw an InvalidStateError
        // exception and abort these steps.
        if self.ready_state.get() != ReadyState::Open {
            return Err(Error::InvalidState(Some(
                "The MediaSource is not open".into(),
            )));
        }

        // Step 2. If start is negative or greater than end, then throw a TypeError exception and
        // abort these steps.
        if start < 0. || start > end {
            return Err(Error::Type(c"Invalid live seekable range".to_owned()));
        }

        // Step 3. Set live seekable range to be a new normalized TimeRanges object containing a
        // single range whose start position is start and end position is end.
        self.live_seekable_start.set(start);
        self.live_seekable_end.set(end);
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-clearliveseekablerange>
    fn ClearLiveSeekableRange(&self) -> ErrorResult {
        // Step 1. If the readyState attribute is not "open" then throw an InvalidStateError
        // exception and abort these steps.
        if self.ready_state.get() != ReadyState::Open {
            return Err(Error::InvalidState(Some(
                "The MediaSource is not open".into(),
            )));
        }

        // Step 2. If live seekable range contains a range, then set live seekable range to be a new
        // empty TimeRanges object.
        self.live_seekable_start.set(f64::NAN);
        self.live_seekable_end.set(f64::NAN);
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-mediasource-istypesupported>
    fn IsTypeSupported(_window: &Window, type_: DOMString) -> bool {
        is_type_supported(&type_.str())
    }
}
