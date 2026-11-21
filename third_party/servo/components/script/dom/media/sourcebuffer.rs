/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! <https://www.w3.org/TR/media-source-2/#sourcebuffer>
//!
//! The player parses appended bytes synchronously (`Player::mse_append`), so the coded frame
//! eviction of the prepare append algorithm can still throw. Every observable step after it
//! (updatestart, the initialization segment received algorithm, update, updateend, the append error
//! algorithm) is queued in the order the specification gives them.

use std::cell::Cell;

use dom_struct::dom_struct;
use js::context::JSContext;
use script_bindings::buffer_source::get_buffer_source_copy;
use script_bindings::reflector::reflect_dom_object;
use servo_media::player::mse::{
    AppendMode as MseAppendMode, AppendResult, MseError, SourceBufferId,
};

use crate::dom::bindings::codegen::Bindings::HTMLMediaElementBinding::HTMLMediaElementMethods;
use crate::dom::bindings::codegen::Bindings::MediaSourceBinding::{
    EndOfStreamError, MediaSourceMethods, ReadyState,
};
use crate::dom::bindings::codegen::Bindings::SourceBufferBinding::{
    AppendMode, SourceBufferMethods,
};
use crate::dom::bindings::codegen::UnionTypes::ArrayBufferViewOrArrayBuffer;
use crate::dom::bindings::error::{Error, ErrorResult, Fallible};
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::num::Finite;
use crate::dom::bindings::refcounted::Trusted;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{DomRoot, MutNullableDom};
use crate::dom::bindings::str::DOMString;
use crate::dom::eventtarget::EventTarget;
use crate::dom::media::mediasource::{MediaSource, is_type_supported, queue_mse_event};
use crate::dom::timeranges::{TimeRanges, TimeRangesContainer};
use crate::dom::window::Window;

#[dom_struct]
pub(crate) struct SourceBuffer {
    eventtarget: EventTarget,
    /// The parent media source; `None` once this buffer was removed from it.
    parent: MutNullableDom<MediaSource>,
    /// The player's number for this buffer.
    id: SourceBufferId,
    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-mode>
    mode: Cell<AppendMode>,
    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-updating>
    updating: Cell<bool>,
    /// The range removal algorithm is running.
    removing: Cell<bool>,
    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-timestampoffset>
    timestamp_offset: Cell<f64>,
    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-appendwindowstart>
    append_window_start: Cell<f64>,
    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-appendwindowend>
    append_window_end: Cell<f64>,
    /// <https://www.w3.org/TR/media-source-2/#dfn-generate-timestamps-flag>
    generate_timestamps: Cell<bool>,
    /// <https://www.w3.org/TR/media-source-2/#dfn-first-initialization-segment-received-flag>
    first_init_segment_received: Cell<bool>,
    /// <https://www.w3.org/TR/media-source-2/#dfn-pending-initialization-segment-for-changetype-flag>
    pending_init_segment_for_change_type: Cell<bool>,
    /// Incremented when a running buffer append or range removal is aborted, so its queued task
    /// does not fire update and updateend.
    update_generation: Cell<u32>,
}

impl SourceBuffer {
    fn new_inherited(
        parent: &MediaSource,
        id: SourceBufferId,
        generate_timestamps: bool,
    ) -> SourceBuffer {
        SourceBuffer {
            eventtarget: EventTarget::new_inherited(),
            parent: MutNullableDom::new(Some(parent)),
            id,
            mode: Cell::new(if generate_timestamps {
                AppendMode::Sequence
            } else {
                AppendMode::Segments
            }),
            updating: Cell::new(false),
            removing: Cell::new(false),
            timestamp_offset: Cell::new(0.),
            append_window_start: Cell::new(0.),
            append_window_end: Cell::new(f64::INFINITY),
            generate_timestamps: Cell::new(generate_timestamps),
            first_init_segment_received: Cell::new(false),
            pending_init_segment_for_change_type: Cell::new(false),
            update_generation: Cell::new(0),
        }
    }

    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        parent: &MediaSource,
        id: SourceBufferId,
        generate_timestamps: bool,
    ) -> DomRoot<SourceBuffer> {
        reflect_dom_object(
            cx,
            Box::new(SourceBuffer::new_inherited(parent, id, generate_timestamps)),
            window,
        )
    }

    pub(crate) fn id(&self) -> SourceBufferId {
        self.id
    }

    pub(crate) fn updating(&self) -> bool {
        self.updating.get()
    }

    /// This buffer was removed from its parent media source (removeSourceBuffer() or detaching).
    pub(crate) fn removed_from_parent(&self) {
        self.parent.set(None);
        self.update_generation.set(self.update_generation.get() + 1);
    }

    /// The parent media source, or the InvalidStateError for "this object has been removed from the
    /// sourceBuffers attribute of the parent media source".
    fn parent_or_throw(&self) -> Fallible<DomRoot<MediaSource>> {
        self.parent.get().ok_or_else(|| {
            Error::InvalidState(Some(
                "The SourceBuffer was removed from its MediaSource".into(),
            ))
        })
    }

    fn throw_if_updating(&self) -> ErrorResult {
        if self.updating.get() {
            return Err(Error::InvalidState(Some(
                "The SourceBuffer is updating".into(),
            )));
        }
        Ok(())
    }

    /// The removed and updating checks most setters and methods start with.
    fn parent_if_idle(&self) -> Fallible<DomRoot<MediaSource>> {
        let parent = self.parent_or_throw()?;
        self.throw_if_updating()?;
        Ok(parent)
    }

    /// removeSourceBuffer() step 2 and abort() step 4: "If the updating attribute equals true, then
    /// run the following steps: Abort the buffer append algorithm if it is running. Set the
    /// updating attribute to false. Queue a task to fire an event named abort at this SourceBuffer
    /// object. Queue a task to fire an event named updateend at this SourceBuffer object."
    pub(crate) fn abort_pending_update(&self) {
        if !self.updating.get() {
            return;
        }
        self.update_generation.set(self.update_generation.get() + 1);
        self.updating.set(false);
        self.removing.set(false);
        queue_mse_event(self.upcast(), "abort");
        queue_mse_event(self.upcast(), "updateend");
    }

    /// The end of an update: "Set the updating attribute to false. Queue a task to fire an event
    /// named update at this SourceBuffer object. Queue a task to fire an event named updateend at
    /// this SourceBuffer object."
    fn finish_update(&self) {
        self.updating.set(false);
        queue_mse_event(self.upcast(), "update");
        queue_mse_event(self.upcast(), "updateend");
    }

    /// <https://www.w3.org/TR/media-source-2/#sourcebuffer-prepare-append>
    ///
    /// Steps 1-4; steps 5-6, the coded frame eviction, are the player's and run with the append.
    fn prepare_append(&self) -> Fallible<DomRoot<MediaSource>> {
        // Step 1. If the SourceBuffer has been removed from the sourceBuffers attribute of the
        // parent media source then throw an InvalidStateError exception and abort these steps.
        let parent = self.parent_or_throw()?;

        // Step 2. If the updating attribute equals true, then throw an InvalidStateError exception
        // and abort these steps.
        self.throw_if_updating()?;

        // Step 3. Let recent element error be determined as follows: If the MediaSource was
        // constructed in a Window, let recent element error be true if the HTMLMediaElement's error
        // attribute is not null. If recent element error is true, then throw an InvalidStateError
        // exception and abort these steps.
        if parent
            .media_element()
            .is_some_and(|element| element.in_error_state())
        {
            return Err(Error::InvalidState(Some(
                "The media element has an error".into(),
            )));
        }

        // Step 4. If the readyState attribute of the parent media source is in the "ended" state
        // then run the following steps: ...
        parent.reopen_if_ended();
        Ok(parent)
    }

    /// <https://www.w3.org/TR/media-source-2/#sourcebuffer-buffer-append>
    ///
    /// The steps after the player parsed the bytes.
    fn buffer_append_done(&self, result: Result<AppendResult, MseError>, generation: u32) {
        let Some(parent) = self.parent.get() else {
            return;
        };
        let aborted = generation != self.update_generation.get();
        match result {
            Ok(result) => {
                // The segment parser loop moved timestampOffset in "sequence" mode. What was parsed
                // before an abort() stays parsed, so this and the initialization segment run even
                // then.
                self.timestamp_offset.set(result.timestamp_offset);
                if let Some(tracks) = result.init_segment {
                    if !self.initialization_segment_received(
                        &parent,
                        !tracks.is_empty(),
                        result.init_duration,
                    ) {
                        if !aborted {
                            self.append_error(&parent);
                        }
                        return;
                    }
                }
                // Coded frames past the duration extend it (coded frame processing, last step).
                parent.extend_duration_to_buffered_media();
                if aborted {
                    return;
                }
                // Step 2.3 of the segment parser loop has run out of data; the buffer append
                // algorithm ends:
                // Step 6. Set the updating attribute to false.
                // Step 7. Queue a task to fire an event named update at this SourceBuffer object.
                // Step 8. Queue a task to fire an event named updateend at this SourceBuffer
                // object.
                self.finish_update();
            },
            Err(_) => {
                // The segment parser loop failed and ran the append error algorithm.
                if !aborted {
                    self.append_error(&parent);
                }
            },
        }
    }

    /// <https://www.w3.org/TR/media-source-2/#sourcebuffer-init-segment-received>
    ///
    /// Answers false when the append error algorithm must run.
    fn initialization_segment_received(
        &self,
        parent: &MediaSource,
        has_tracks: bool,
        init_duration: Option<f64>,
    ) -> bool {
        // Step 1. Update the duration attribute if it currently equals NaN: If the initialization
        // segment contains a duration: Run the duration change algorithm with new duration set to
        // the duration in the initialization segment. Otherwise: Run the duration change algorithm
        // with new duration set to positive Infinity.
        // A fragmented MP4 says 0 when it does not know: that is no duration.
        if parent.Duration().is_nan() {
            parent.set_duration_from_initialization_segment(
                init_duration
                    .filter(|duration| *duration > 0.)
                    .unwrap_or(f64::INFINITY),
            );
        }

        // Step 2. If the initialization segment has no audio, video, or text tracks, then run the
        // append error algorithm and abort these steps.
        if !has_tracks {
            return false;
        }

        // Step 3. If the first initialization segment received flag is true, then run the following
        // steps: the track count and codec checks are the player's, which fails the append when
        // they do not hold.

        // Step 4. Let active track flag equal false.
        // Step 5. If the first initialization segment received flag is false, then run the
        // following steps:
        if !self.first_init_segment_received.get() {
            // Steps 5.1-5.3. An enabled audio track or a selected video track sets active track
            // flag to true. The audio, video and text track objects are not there yet; the first
            // audio and the first video track of the first initialization segment are enabled and
            // selected, so any track makes this buffer active.
            // Step 5.4. If active track flag equals true, then run the following steps: Add this
            // SourceBuffer to activeSourceBuffers. Queue a task to fire an event named
            // addsourcebuffer at activeSourceBuffers.
            parent.activate_source_buffer(self);

            // Step 5.5. Set first initialization segment received flag to true.
            self.first_init_segment_received.set(true);
        }

        // Step 6. Set pending initialization segment for changeType flag to false.
        self.pending_init_segment_for_change_type.set(false);

        // Step 7. If the active track flag equals true, then run the following steps: (done in step
        // 5.4).
        // Step 8. If the HTMLMediaElement.readyState attribute is HAVE_NOTHING, then run the
        // following steps: the player reports the metadata, and the element moves to HAVE_METADATA
        // from its event.
        true
    }

    /// <https://www.w3.org/TR/media-source-2/#sourcebuffer-append-error>
    fn append_error(&self, parent: &MediaSource) {
        // Step 1. Run the reset parser state algorithm.
        let id = self.id;
        parent.with_player(|player| player.mse_reset_parser(id));

        // Step 2. Set the updating attribute to false.
        self.updating.set(false);

        // Step 3. Queue a task to fire an event named error at this SourceBuffer object.
        queue_mse_event(self.upcast(), "error");

        // Step 4. Queue a task to fire an event named updateend at this SourceBuffer object.
        queue_mse_event(self.upcast(), "updateend");

        // Step 5. Run the end of stream algorithm with the error parameter set to "decode".
        parent.end_of_stream(Some(EndOfStreamError::Decode));
    }

    /// Queues a task that runs `steps` unless the update was aborted or this buffer removed
    /// meanwhile.
    fn queue_update_steps(&self, steps: impl FnOnce(&SourceBuffer, u32) + Send + 'static) {
        let this = Trusted::new(self);
        let generation = self.update_generation.get();
        self.global()
            .task_manager()
            .media_element_task_source()
            .queue(task!(source_buffer_update: move || {
                let this = this.root();
                steps(&this, generation);
            }));
    }
}

impl SourceBufferMethods<crate::DomTypeHolder> for SourceBuffer {
    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-mode>
    fn Mode(&self) -> AppendMode {
        self.mode.get()
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-mode>
    fn SetMode(&self, value: AppendMode) -> ErrorResult {
        // Step 1. If this object has been removed from the sourceBuffers attribute of the parent
        // media source, then throw an InvalidStateError exception and abort these steps.
        // Step 2. If the updating attribute equals true, then throw an InvalidStateError exception
        // and abort these steps.
        let parent = self.parent_if_idle()?;

        // Step 3. Let new mode equal the new value being assigned to this attribute.
        // Step 4. If generate timestamps flag equals true and new mode equals "segments", then
        // throw a TypeError exception and abort these steps.
        if self.generate_timestamps.get() && value == AppendMode::Segments {
            return Err(Error::Type(
                c"This byte stream format generates timestamps; the mode must be \"sequence\""
                    .to_owned(),
            ));
        }

        // Step 5. If the readyState attribute of the parent media source is in the "ended" state
        // then run the following steps: ...
        parent.reopen_if_ended();

        // Step 6. If the append state equals PARSING_MEDIA_SEGMENT, then throw an InvalidStateError
        // and abort these steps. (The player parses every append to its end, so a SourceBuffer is
        // never left in that state between tasks.)

        // Step 7. If the new mode equals "sequence", then set the group start timestamp to the
        // group end timestamp.
        // Step 8. Update the attribute to new mode.
        self.mode.set(value);
        let mode = match value {
            AppendMode::Segments => MseAppendMode::Segments,
            AppendMode::Sequence => MseAppendMode::Sequence,
        };
        let id = self.id;
        parent.with_player(|player| player.mse_set_mode(id, mode));
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-updating>
    fn Updating(&self) -> bool {
        self.updating.get()
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-buffered>
    fn GetBuffered(&self, cx: &mut JSContext) -> Fallible<DomRoot<TimeRanges>> {
        // Step 1. If this object has been removed from the sourceBuffers attribute of the parent
        // media source then throw an InvalidStateError exception and abort these steps.
        let parent = self.parent_or_throw()?;

        // Steps 2-5. The intersection of the track buffers' ranges, which the player keeps.
        let id = self.id;
        let mut buffered = TimeRangesContainer::default();
        for range in parent
            .with_player(|player| player.mse_buffered(id))
            .unwrap_or_default()
        {
            let _ = buffered.add(range.start, range.end);
        }
        Ok(TimeRanges::new(cx, self.global().as_window(), buffered))
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-timestampoffset>
    fn TimestampOffset(&self) -> Finite<f64> {
        Finite::wrap(self.timestamp_offset.get())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-timestampoffset>
    fn SetTimestampOffset(&self, value: Finite<f64>) -> ErrorResult {
        // Step 1. Let new timestamp offset equal the new value being assigned to this attribute.
        let value = *value;

        // Step 2. If this object has been removed from the sourceBuffers attribute of the parent
        // media source, then throw an InvalidStateError exception and abort these steps.
        // Step 3. If the updating attribute equals true, then throw an InvalidStateError exception
        // and abort these steps.
        let parent = self.parent_if_idle()?;

        // Step 4. If the readyState attribute of the parent media source is in the "ended" state
        // then run the following steps: ...
        parent.reopen_if_ended();

        // Step 5. If the append state equals PARSING_MEDIA_SEGMENT, then throw an InvalidStateError
        // and abort these steps. (Never the case between tasks, see the mode setter.)

        // Step 6. If the mode attribute equals "sequence", then set the group start timestamp to
        // new timestamp offset. (The player does.)
        // Step 7. Update the attribute to new timestamp offset.
        self.timestamp_offset.set(value);
        let id = self.id;
        parent.with_player(|player| player.mse_set_timestamp_offset(id, value));
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-appendwindowstart>
    fn AppendWindowStart(&self) -> Finite<f64> {
        Finite::wrap(self.append_window_start.get())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-appendwindowstart>
    fn SetAppendWindowStart(&self, value: Finite<f64>) -> ErrorResult {
        let value = *value;

        // Step 1. If this object has been removed from the sourceBuffers attribute of the parent
        // media source, then throw an InvalidStateError exception and abort these steps.
        // Step 2. If the updating attribute equals true, then throw an InvalidStateError exception
        // and abort these steps.
        let parent = self.parent_if_idle()?;

        // Step 3. If the new value is less than 0 or greater than or equal to appendWindowEnd then
        // throw a TypeError exception and abort these steps.
        if value < 0. || value >= self.append_window_end.get() {
            return Err(Error::Type(
                c"appendWindowStart must be in [0, appendWindowEnd)".to_owned(),
            ));
        }

        // Step 4. Update the attribute to the new value.
        self.append_window_start.set(value);
        let (id, end) = (self.id, self.append_window_end.get());
        parent.with_player(|player| player.mse_set_append_window(id, value, end));
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-appendwindowend>
    fn AppendWindowEnd(&self) -> f64 {
        self.append_window_end.get()
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-appendwindowend>
    fn SetAppendWindowEnd(&self, value: f64) -> ErrorResult {
        // Step 1. If this object has been removed from the sourceBuffers attribute of the parent
        // media source, then throw an InvalidStateError exception and abort these steps.
        // Step 2. If the updating attribute equals true, then throw an InvalidStateError exception
        // and abort these steps.
        let parent = self.parent_if_idle()?;

        // Step 3. If the new value equals NaN, then throw a TypeError and abort these steps.
        if value.is_nan() {
            return Err(Error::Type(c"appendWindowEnd must not be NaN".to_owned()));
        }

        // Step 4. If the new value is less than or equal to appendWindowStart then throw a
        // TypeError exception and abort these steps.
        if value <= self.append_window_start.get() {
            return Err(Error::Type(
                c"appendWindowEnd must be greater than appendWindowStart".to_owned(),
            ));
        }

        // Step 5. Update the attribute to the new value.
        self.append_window_end.set(value);
        let (id, start) = (self.id, self.append_window_start.get());
        parent.with_player(|player| player.mse_set_append_window(id, start, value));
        Ok(())
    }

    // <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-onupdatestart>
    event_handler!(updatestart, GetOnupdatestart, SetOnupdatestart);

    // <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-onupdate>
    event_handler!(update, GetOnupdate, SetOnupdate);

    // <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-onupdateend>
    event_handler!(updateend, GetOnupdateend, SetOnupdateend);

    // <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-onerror>
    event_handler!(error, GetOnerror, SetOnerror);

    // <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-onabort>
    event_handler!(abort, GetOnabort, SetOnabort);

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-appendbuffer>
    fn AppendBuffer(&self, data: ArrayBufferViewOrArrayBuffer) -> ErrorResult {
        // Step 1. Run the prepare append algorithm.
        let parent = self.prepare_append()?;
        let current_time = parent
            .media_element()
            .map_or(0., |element| *element.CurrentTime());

        // Step 2. Add data to the end of the [[input buffer]].
        // Step 5 is run here, synchronously, as the prepare append algorithm's coded frame eviction
        // (its steps 5-6) is in the same player call: "If the [[buffer full flag]] equals true,
        // then throw a QuotaExceededError exception and abort these steps" must come before
        // updating changes.
        let bytes = get_buffer_source_copy((&data).into());
        let id = self.id;
        let result = parent
            .with_player(|player| player.mse_append(id, bytes, current_time))
            .unwrap_or(Err(MseError::NoMse));
        if result == Err(MseError::QuotaExceeded) {
            return Err(Error::QuotaExceeded {
                quota: None,
                requested: None,
            });
        }

        // Step 3. Set the updating attribute to true.
        self.updating.set(true);

        // Step 4. Queue a task to fire an event named updatestart at this SourceBuffer object.
        queue_mse_event(self.upcast(), "updatestart");

        // Step 5. Asynchronously run the buffer append algorithm.
        self.queue_update_steps(move |this, generation| {
            this.buffer_append_done(result, generation);
        });
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-abort>
    fn Abort(&self) -> ErrorResult {
        // Step 1. If this object has been removed from the sourceBuffers attribute of the parent
        // media source then throw an InvalidStateError exception and abort these steps.
        let parent = self.parent_or_throw()?;

        // Step 2. If the readyState attribute of the parent media source is not in the "open" state
        // then throw an InvalidStateError exception and abort these steps.
        if parent.ready_state() != ReadyState::Open {
            return Err(Error::InvalidState(Some(
                "The MediaSource is not open".into(),
            )));
        }

        // Step 3. If the range removal algorithm is running, then throw an InvalidStateError
        // exception and abort these steps.
        if self.removing.get() {
            return Err(Error::InvalidState(Some("A remove() is running".into())));
        }

        // Step 4. If the updating attribute equals true, then run the following steps: ...
        self.abort_pending_update();

        // Step 5. Run the reset parser state algorithm.
        let id = self.id;
        parent.with_player(|player| player.mse_reset_parser(id));

        // Step 6. Set appendWindowStart to the presentation start time.
        self.append_window_start.set(0.);

        // Step 7. Set appendWindowEnd to positive Infinity.
        self.append_window_end.set(f64::INFINITY);
        parent.with_player(|player| player.mse_set_append_window(id, 0., f64::INFINITY));
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-changetype>
    fn ChangeType(&self, type_: DOMString) -> ErrorResult {
        let mime = type_.str().to_string();

        // Step 1. If type is an empty string then throw a TypeError exception and abort these
        // steps.
        if mime.is_empty() {
            return Err(Error::Type(c"The type is empty".to_owned()));
        }

        // Step 2. If this object has been removed from the sourceBuffers attribute of the parent
        // media source, then throw an InvalidStateError exception and abort these steps.
        // Step 3. If the updating attribute equals true, then throw an InvalidStateError exception
        // and abort these steps.
        let parent = self.parent_if_idle()?;

        // Step 4. If type contains a MIME type that is not supported or contains a MIME type that
        // is not supported with the types specified (currently or previously) of SourceBuffer
        // objects in the sourceBuffers attribute of the parent media source, then throw a
        // NotSupportedError exception and abort these steps.
        if !is_type_supported(&mime) {
            return Err(Error::NotSupported(Some(format!(
                "{mime} is not supported"
            ))));
        }

        // Step 5. If the readyState attribute of the parent media source is in the "ended" state
        // then run the following steps: ...
        parent.reopen_if_ended();

        // Step 6. Run the reset parser state algorithm.
        let id = self.id;
        parent.with_player(|player| player.mse_reset_parser(id));

        // The player takes the new type for the next initialization segment, or refuses the
        // combination (step 4).
        match parent.with_player(|player| player.mse_change_type(id, &mime)) {
            Some(Ok(())) => {},
            _ => {
                return Err(Error::NotSupported(Some(format!(
                    "{mime} is not supported"
                ))));
            },
        }

        // Step 7. Update the generate timestamps flag on this SourceBuffer object to the value in
        // the "Generate Timestamps Flag" column of the byte stream format registry entry that is
        // associated with type. (False for ISO BMFF and WebM, the only formats here.)
        self.generate_timestamps.set(false);

        // Step 8. If the generate timestamps flag equals true: Set the mode attribute on this
        // SourceBuffer object to "sequence", including running the associated steps for that
        // attribute being set. Otherwise: Keep the previous value of the mode attribute on this
        // SourceBuffer object, without running any associated steps for that attribute being set.
        if self.generate_timestamps.get() {
            self.SetMode(AppendMode::Sequence)?;
        }

        // Step 9. Set pending initialization segment for changeType flag to true.
        self.pending_init_segment_for_change_type.set(true);
        Ok(())
    }

    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebuffer-remove>
    fn Remove(&self, start: Finite<f64>, end: f64) -> ErrorResult {
        let start = *start;

        // Step 1. If this object has been removed from the sourceBuffers attribute of the parent
        // media source then throw an InvalidStateError exception and abort these steps.
        // Step 2. If the updating attribute equals true, then throw an InvalidStateError exception
        // and abort these steps.
        let parent = self.parent_if_idle()?;

        // Step 3. If duration equals NaN, then throw a TypeError exception and abort these steps.
        let duration = parent.Duration();
        if duration.is_nan() {
            return Err(Error::Type(c"The duration is NaN".to_owned()));
        }

        // Step 4. If start is negative or greater than duration, then throw a TypeError exception
        // and abort these steps.
        if start < 0. || start > duration {
            return Err(Error::Type(c"start must be in [0, duration]".to_owned()));
        }

        // Step 5. If end is less than or equal to start or end equals NaN, then throw a TypeError
        // exception and abort these steps.
        if end.is_nan() || end <= start {
            return Err(Error::Type(c"end must be greater than start".to_owned()));
        }

        // Step 6. If the readyState attribute of the parent media source is in the "ended" state
        // then run the following steps: ...
        parent.reopen_if_ended();

        // Step 7. Run the range removal algorithm with start and end as the start and end of the
        // removal range.
        // <https://www.w3.org/TR/media-source-2/#sourcebuffer-range-removal>
        // Step 1. Let start equal the starting presentation timestamp for the removal range.
        // Step 2. Let end equal the end presentation timestamp for the removal range.
        // Step 3. Set the updating attribute to true.
        self.updating.set(true);
        self.removing.set(true);

        // Step 4. Queue a task to fire an event named updatestart at this SourceBuffer object.
        queue_mse_event(self.upcast(), "updatestart");

        // Step 5. Return control to the caller and run the rest of the steps asynchronously.
        self.queue_update_steps(move |this, generation| {
            if generation != this.update_generation.get() {
                return;
            }
            let Some(parent) = this.parent.get() else {
                return;
            };
            // Step 6. Run the coded frame removal algorithm with start and end as the start and end
            // of the removal range.
            let id = this.id;
            parent.with_player(|player| player.mse_remove(id, start, end));
            this.removing.set(false);

            // Step 7. Set the updating attribute to false.
            // Step 8. Queue a task to fire an event named update at this SourceBuffer object.
            // Step 9. Queue a task to fire an event named updateend at this SourceBuffer object.
            this.finish_update();
        });
        Ok(())
    }
}
