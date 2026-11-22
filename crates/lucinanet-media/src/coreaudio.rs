/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The sound device on macOS: CoreAudio, through `cpal`.
//!
//! Juno's `/dev/lucina-audio` is not here and there is no other program's device to share: CoreAudio
//! is the sound server, and an output stream is the device. The Mixer mixes to 16-bit stereo at
//! 48 kHz, so the stream is built at exactly that — 2 channels, a supported range covering 48 kHz,
//! in the device's own float if it has one (CoreAudio's native format is f32) and s16 otherwise. A
//! device that offers no 48 kHz stereo is refused and [`open`] fails, which is what sends the Mixer
//! to its stand-in; resampling to whatever the device wants is a later step.
//!
//! The queue, its counts and the rules they obey are [`crate::ring`] — tested on every OS. What is
//! macOS-specific is the transport: the callback runs on CoreAudio's realtime thread, where the only
//! acceptable waits are none at all, so it copies out of the ring under `try_lock` and plays silence
//! for that one callback if the mixer thread holds it. Silence is not counted as played, so the
//! mixer's clock neither gains nor loses on a lock miss, exactly as it does on a device run dry.
//!
//! The stream is built in [`open`] but started by the first [`write`]: cpal's streams do not run
//! until `play`, so nothing consumes a callback of silence while the Mixer is still deciding whether
//! this device is worth having, and `set_volume` (-24 dB) has certainly been stored by then. Building
//! it in `open` is the point: a device that refuses a stream fails where the Mixer can still fall
//! back, not in the middle of a page.
//!
//! # What `played_frames` still gets wrong
//!
//! A frame is counted when the callback takes it, but it reaches the speaker one device buffer
//! later, so the clock this sink drives leads the sound by that much. cpal reports both instants —
//! `OutputCallbackInfo::timestamp()` carries `playback` against `callback` — so the gap is measurable
//! and could be subtracted from the count later. It is not corrected here.
//!
//! How large it is: **not measured**. This is type-checked for `aarch64-apple-darwin` by a cross
//! check on Linux x86_64, which is as far as it can go without a Mac: no CoreAudio, so no
//! callbacks and no timestamps to read. On a Mac, log `info.timestamp().playback -
//! info.timestamp().callback` per callback on the built-in output (a few device buffers of a few ms
//! each is typical; Bluetooth headphones are far worse, up to ~100 ms) and record the median here in
//! frames.
//!
//! [`open`]: Sink::open
//! [`write`]: Sink::write

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use lucina_audio_decode::device::Sink;

use crate::ring::{self, Queue};

/// What the Mixer writes, and so what the stream is built at.
const RATE: u32 = crate::mixer::RATE;
const CHANNELS: u16 = ring::CHANNELS as u16;

/// The device format this sink can fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    F32,
    I16,
}

/// The Mac's output, as the Mixer's [`Sink`].
pub struct CoreAudioSink {
    /// Shared with the callback.
    queue: Arc<Queue>,
    /// Built by `open`, started by the first `write`, let go by `close`.
    stream: Option<Stream>,
    /// Set by cpal's error callback: the stream is done, and only a reopened device can play again.
    dead: Arc<AtomicBool>,
    started: bool,
}

impl CoreAudioSink {
    pub fn new() -> CoreAudioSink {
        CoreAudioSink {
            queue: Arc::new(Queue::new()),
            stream: None,
            dead: Arc::new(AtomicBool::new(false)),
            started: false,
        }
    }

    /// 48 kHz stereo in the device's own float, or in s16 if that is all it has: `None` when it has
    /// neither, which is a device this sink cannot take.
    fn pick(device: &Device) -> Option<(StreamConfig, Format)> {
        let mut s16 = None;
        for range in device.supported_output_configs().ok()? {
            if range.channels() != CHANNELS {
                continue;
            }
            let format = match range.sample_format() {
                SampleFormat::F32 => Format::F32,
                SampleFormat::I16 => Format::I16,
                _ => continue,
            };
            let Some(config) = range.try_with_sample_rate(RATE).map(|config| config.config()) else {
                continue;
            };
            if format == Format::F32 {
                return Some((config, format));
            }
            s16 = Some((config, format));
        }
        s16
    }

    /// The first frame is what starts the stream: before it there is nothing to play.
    fn start(&mut self) -> io::Result<()> {
        self.started = true;
        let Some(stream) = self.stream.as_ref() else {
            return Err(io::Error::from(io::ErrorKind::NotConnected));
        };
        if let Err(e) = stream.play() {
            self.dead.store(true, Ordering::Release);
            return Err(io::Error::other(format!("the CoreAudio stream would not start ({e})")));
        }
        Ok(())
    }
}

impl Sink for CoreAudioSink {
    fn open(&mut self) -> io::Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| io::Error::other("CoreAudio reports no default output device"))?;
        let (config, format) = CoreAudioSink::pick(&device).ok_or_else(|| {
            io::Error::other(format!("the default output device has no 48 kHz stereo {CHANNELS}-channel format in f32 or s16"))
        })?;
        log::debug!("lucinanet-media: CoreAudio output: {config:?} {format:?}");
        let f32_queue = self.queue.clone();
        let fill = move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
            // A frame is two samples; anything else is not the stream that was opened.
            if data.len() % ring::CHANNELS == 0 {
                f32_queue.play_f32(data);
            } else {
                data.fill(0.0);
            }
        };
        let i16_queue = self.queue.clone();
        let fill_i16 = move |data: &mut [i16], _info: &cpal::OutputCallbackInfo| {
            if data.len() % ring::CHANNELS == 0 {
                i16_queue.play_i16(data);
            } else {
                data.fill(0);
            }
        };
        let failed = self.dead.clone();
        let once = Arc::new(AtomicBool::new(false));
        let error = move |e: cpal::Error| {
            // Once: this runs on the audio thread, and a device that is gone says so on every
            // callback. The mixer learns from `dead` instead of from more logging.
            if !once.swap(true, Ordering::Relaxed) {
                log::warn!("lucinanet-media: the CoreAudio stream failed ({e}); sound stops until the device is reopened");
            }
            failed.store(true, Ordering::Release);
        };
        let stream = match format {
            Format::F32 => device.build_output_stream(config, fill, error, None),
            Format::I16 => device.build_output_stream(config, fill_i16, error, None),
        }
        .map_err(io::Error::other)?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Lets go of the stream: what CoreAudio still holds is its own business now.
    fn close(&mut self) {
        if let Some(stream) = self.stream.take() {
            // cpal stops a stream when dropped; a device that will not pause says so, and it is
            // dropped anyway.
            if let Err(e) = stream.pause() {
                log::debug!("lucinanet-media: the CoreAudio stream would not pause ({e})");
            }
        }
        self.started = false;
        self.dead.store(false, Ordering::Release);
        self.queue.close();
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.dead.load(Ordering::Acquire) {
            // The mixer's own fallback path for a device that has stopped taking frames.
            return Err(io::Error::other("the CoreAudio stream is gone"));
        }
        if !self.started {
            self.start()?;
        }
        self.queue.write(bytes)
    }

    fn flush(&mut self) {
        self.queue.flush();
    }

    fn set_volume(&mut self, db: i32) {
        self.queue.set_volume(db);
    }

    fn played_frames(&mut self) -> u64 {
        self.queue.played()
    }
}
