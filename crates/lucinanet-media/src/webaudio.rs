/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! WebAudio on Juno: servo-media's own Rust graph (`servo_media_audio`) renders, this takes its output into the mixer, and
//! `decodeAudioData` decodes with symphonia.
//!
//! The original is the GStreamer backend's `audio_sink.rs` and `audio_decoder.rs`. Its sink is an appsrc holding at most one render
//! quantum, which says `SinkNeedData` to the render thread when it wants the next; here the mixer stream holds about 40 ms and a small
//! thread says `SinkNeedData` when it falls under that, so the graph runs a little ahead of the speaker and never far. Its decoder
//! gives the file's own channels at the context's rate; this gives two channels (Juno's speakers, and what `lucina_audio_decode`
//! makes), resampled to the context's rate. Recorded as "adapt" in docs/media.md.

use std::io::Cursor;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucina_audio_decode::Decoder;
use lucina_audio_decode::resample::Resampler;
use servo_media_audio::block::Chunk;
use servo_media_audio::decoder::{AudioDecoder, AudioDecoderCallbacks, AudioDecoderError, AudioDecoderOptions};
use servo_media_audio::render_thread::{AudioRenderThreadMsg, SinkEosCallback};
use servo_media_audio::sink::{AudioSink, AudioSinkError};
use servo_media_streams::MediaSocket;

use crate::mixer::{Mixer, RATE, Stream};

/// The graph renders until the stream holds this many samples (stereo pairs times two): 40 ms.
const AHEAD: usize = RATE as usize * 2 / 25;

pub struct LucinaAudioSink {
    stream: Arc<Stream>,
    state: Mutex<SinkState>,
}

struct SinkState {
    resampler: Option<Resampler>,
    stereo: Vec<i16>,
    bytes: Vec<u8>,
    samples: Vec<i16>,
}

impl LucinaAudioSink {
    pub fn new() -> LucinaAudioSink {
        let stream = Arc::new(Mixer::get().stream());
        stream.flush(0);
        LucinaAudioSink { stream, state: Mutex::new(SinkState { resampler: None, stereo: Vec::new(), bytes: Vec::new(), samples: Vec::new() }) }
    }
}

impl Default for LucinaAudioSink {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioSink for LucinaAudioSink {
    fn init(&self, sample_rate: f32, render_thread: Sender<AudioRenderThreadMsg>) -> Result<(), AudioSinkError> {
        let rate = sample_rate.round() as u32;
        if !(8_000..=192_000).contains(&rate) {
            return Err(AudioSinkError::Backend(format!("a rate of {sample_rate} Hz")));
        }
        self.state.lock().unwrap_or_else(|e| e.into_inner()).resampler = (rate != RATE).then(|| Resampler::new(rate, RATE));
        // Tells the render thread to go on whenever the stream falls under what it should hold; ends with the sink (a send fails).
        let stream = Arc::downgrade(&self.stream);
        std::thread::Builder::new()
            .name("lucinanet-webaudio".into())
            .spawn(move || {
                loop {
                    let Some(s) = stream.upgrade() else { return };
                    s.wait_below(AHEAD, Duration::from_millis(20));
                    let low = s.queued() < AHEAD;
                    drop(s);
                    if low && render_thread.send(AudioRenderThreadMsg::SinkNeedData).is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
            .map_err(|e| AudioSinkError::Backend(e.to_string()))?;
        Ok(())
    }

    fn init_stream(&self, _channels: u8, _sample_rate: f32, _socket: Box<dyn MediaSocket>) -> Result<(), AudioSinkError> {
        Err(AudioSinkError::Backend("MediaStream destinations are not supported on Juno yet".into()))
    }

    fn play(&self) -> Result<(), AudioSinkError> {
        self.stream.set_playing(true);
        Ok(())
    }

    fn stop(&self) -> Result<(), AudioSinkError> {
        self.stream.set_playing(false);
        Ok(())
    }

    fn has_enough_data(&self) -> bool {
        self.stream.queued() >= AHEAD
    }

    fn push_data(&self, mut chunk: Chunk) -> Result<(), AudioSinkError> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let st = &mut *st;
        st.stereo.clear();
        if let Some(block) = chunk.blocks.first_mut() {
            let channels = block.chan_count();
            if block.is_silence() || channels == 0 {
                st.stereo.resize(servo_media_audio::block::FRAMES_PER_BLOCK_USIZE * 2, 0);
            } else {
                let frames = block.data_chan(0).len();
                let left: Vec<f32> = block.data_chan(0).to_vec();
                let right: Vec<f32> = if channels > 1 { block.data_chan(1).to_vec() } else { left.clone() };
                for i in 0..frames {
                    st.stereo.push(to_i16(left[i]));
                    st.stereo.push(to_i16(right[i]));
                }
            }
        } else {
            st.stereo.resize(servo_media_audio::block::FRAMES_PER_BLOCK_USIZE * 2, 0);
        }
        let out: &[i16] = match st.resampler.as_mut() {
            Some(r) => {
                st.bytes.clear();
                r.process(&st.stereo, &mut st.bytes);
                st.samples.clear();
                st.samples.extend(st.bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
                &st.samples
            },
            None => &st.stereo,
        };
        let mut sent = 0;
        // The stream holds a second; `has_enough_data` keeps the graph far under that, so this rarely waits.
        while sent < out.len() {
            sent += self.stream.push(&out[sent..]);
            if sent < out.len() {
                self.stream.wait_for_room(Duration::from_millis(20));
            }
        }
        Ok(())
    }

    fn set_eos_callback(&self, _callback: SinkEosCallback) {}
}

fn to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

struct Samples(Vec<f32>);

impl AsRef<[f32]> for Samples {
    fn as_ref(&self) -> &[f32] {
        &self.0
    }
}

pub struct LucinaAudioDecoder;

impl AudioDecoder for LucinaAudioDecoder {
    fn decode(&self, data: Vec<u8>, callbacks: AudioDecoderCallbacks, options: Option<AudioDecoderOptions>) {
        let rate = options.map_or(44_100.0, |o| o.sample_rate).round() as u32;
        let spawned = std::thread::Builder::new().name("lucinanet-decode-audio".into()).spawn(move || {
            let mut decoder = match Decoder::from_reader(Cursor::new(data)) {
                Ok(d) => d,
                Err(e) => {
                    log::info!("lucinanet-media: decodeAudioData: {e}");
                    callbacks.error(AudioDecoderError::InvalidMediaFormat);
                    return;
                },
            };
            let mut bytes = Vec::new();
            loop {
                match decoder.next_block(&mut bytes) {
                    Ok(true) => {},
                    Ok(false) => break,
                    Err(e) => {
                        log::info!("lucinanet-media: decodeAudioData: {e}");
                        callbacks.error(AudioDecoderError::InvalidSample);
                        return;
                    },
                }
            }
            if (8_000..=192_000).contains(&rate) && rate != RATE {
                let stereo: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                let mut r = Resampler::new(RATE, rate);
                bytes.clear();
                r.process(&stereo, &mut bytes);
                r.flush(&mut bytes);
            }
            let frames = bytes.len() / 4;
            let (mut left, mut right) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
            for f in bytes.chunks_exact(4) {
                left.push(i16::from_le_bytes([f[0], f[1]]) as f32 / 32768.0);
                right.push(i16::from_le_bytes([f[2], f[3]]) as f32 / 32768.0);
            }
            callbacks.ready(2);
            callbacks.progress(Box::new(Samples(left)), 1);
            callbacks.progress(Box::new(Samples(right)), 2);
            callbacks.eos();
        });
        if let Err(e) = spawned {
            log::error!("lucinanet-media: no thread for decodeAudioData: {e}");
        }
    }
}
