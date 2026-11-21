/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A player's sound: decoded on a thread of its own, into its stream in the mixer, and the clock its pictures follow.
//!
//! The original is the Video app's `lucina_video_play::sound`: the sound is read as a second file (symphonia demuxes for itself, so a
//! seek in one never waits for the other) and says where the speakers have got to through `lucina_video::AudioOutput`. What differs
//! is the output: that one owns the device through `lucina_audio_decode::engine`; this one is one stream of LucinaNet's mixer, since
//! a page may play several things at once. A start or a seek lands on the packet before the time asked, and the samples before it are
//! dropped here, so sound starts exactly where the picture does.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use lucina_audio_decode::Decoder;
use lucina_video::AudioOutput;

use crate::mixer::{Mixer, RATE, Stream};

enum Cmd {
    Start(i64),
    Paused(bool),
    Stop,
}

pub struct Sound {
    stream: Arc<Stream>,
    commands: Option<Sender<Cmd>>,
    thread: Option<JoinHandle<()>>,
    /// The decoder failed for good: the clock falls back to the wall.
    failed: Arc<AtomicBool>,
}

impl Sound {
    pub fn new(decoder: Decoder) -> Sound {
        let stream = Arc::new(Mixer::get().stream());
        let (commands, received) = channel();
        let failed = Arc::new(AtomicBool::new(false));
        let (s, f) = (stream.clone(), failed.clone());
        let thread = std::thread::Builder::new()
            .name("lucinanet-sound".into())
            .spawn(move || run(decoder, s, received, f))
            .ok();
        Sound { stream, commands: Some(commands), thread, failed }
    }

    fn send(&self, c: Cmd) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(c);
        }
    }

    pub fn set_gain(&self, gain: f32) {
        self.stream.set_gain(gain);
    }

    /// All of it decoded and heard.
    pub fn drained(&self) -> bool {
        self.failed.load(Ordering::Relaxed) || self.stream.drained()
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.commands = None;
        self.stream.set_playing(false);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl AudioOutput for Sound {
    fn position_us(&self) -> Option<i64> {
        if self.failed.load(Ordering::Relaxed) {
            return None;
        }
        self.stream.position_us()
    }
    fn start(&mut self, pts_us: i64) {
        self.send(Cmd::Start(pts_us));
    }
    fn set_paused(&mut self, paused: bool) {
        // At once, not through the thread: a pause is heard within the mixer's lead.
        self.stream.set_playing(!paused);
        self.send(Cmd::Paused(paused));
    }
    fn seek(&mut self, _pts_us: i64) {
        self.stream.set_playing(false);
        self.send(Cmd::Stop);
    }
}

fn run(mut decoder: Decoder, stream: Arc<Stream>, commands: Receiver<Cmd>, failed: Arc<AtomicBool>) {
    let mut paused = true;
    // Where the decoder is, in microseconds; a fresh decoder is at 0.
    let mut at_us: Option<i64> = Some(0);
    let mut decoding = false;
    // Samples (stereo pairs) still to drop before the start time.
    let mut skip: u64 = 0;
    let mut bytes: Vec<u8> = Vec::new();
    let mut samples: Vec<i16> = Vec::new();
    // Decoded and not yet in the stream: `pending[sent..]`.
    let mut pending: Vec<i16> = Vec::new();
    let mut sent = 0usize;
    loop {
        // Commands first; wait for one when there is nothing to decode.
        let c = if decoding {
            match commands.try_recv() {
                Ok(c) => Some(c),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match commands.recv_timeout(Duration::from_secs(1)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        if let Some(c) = c {
            match c {
                Cmd::Start(pts) => {
                    let pts = pts.max(0);
                    let landed = if at_us == Some(pts) {
                        pts
                    } else {
                        match decoder.seek_ms(pts as u64 / 1000) {
                            Ok(ms) => (ms as i64 * 1000).min(pts),
                            Err(e) => {
                                log::warn!("lucinanet-media: the sound cannot seek ({e}); the picture goes on alone");
                                failed.store(true, Ordering::Relaxed);
                                decoding = false;
                                continue;
                            },
                        }
                    };
                    skip = ((pts - landed) as u64 * RATE as u64) / 1_000_000;
                    stream.flush(pts);
                    stream.set_playing(!paused);
                    pending.clear();
                    sent = 0;
                    at_us = None;
                    decoding = true;
                },
                Cmd::Paused(p) => {
                    paused = p;
                    stream.set_playing(!p);
                },
                Cmd::Stop => {
                    decoding = false;
                    stream.flush(0);
                    at_us = None;
                },
            }
            continue;
        }
        // Room in the stream for what is in hand?
        if sent < pending.len() {
            sent += stream.push(&pending[sent..]);
            if sent < pending.len() {
                stream.wait_for_room(Duration::from_millis(50));
            }
            continue;
        }
        bytes.clear();
        match decoder.next_block(&mut bytes) {
            Ok(true) => {
                samples.clear();
                samples.extend(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
                let frames = samples.len() as u64 / 2;
                let drop_frames = skip.min(frames);
                skip -= drop_frames;
                pending.clear();
                pending.extend_from_slice(&samples[drop_frames as usize * 2..]);
                sent = 0;
            },
            Ok(false) => {
                stream.set_ended();
                decoding = false;
            },
            Err(e) => {
                log::warn!("lucinanet-media: the sound stopped: {e}");
                stream.set_ended();
                failed.store(true, Ordering::Relaxed);
                decoding = false;
            },
        }
    }
}
