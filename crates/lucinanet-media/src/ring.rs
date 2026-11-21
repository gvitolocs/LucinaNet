/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! What a hardware sink plays from: frames in from the mixer thread, frames out from the audio
//! callback, and the two counts the A/V clock is made of.
//!
//! The rules are the device's, not the platform's, and they are [`mixer::Nowhere`]'s rules with real
//! samples behind them: the queue is [`QUEUE`] frames deep, a write with nowhere to go is
//! `WouldBlock`, a frame counts as played only once the callback has really taken it (so a queue run
//! empty stops the clock instead of owing it the gap), and a flush drops what was queued rather than
//! leaving it to be played late.
//!
//! Nothing here names a platform, so every OS tests them — including the ones whose device the
//! [`Queue`] ends up standing in front of.
//!
//! [`mixer::Nowhere`]: crate::mixer::Nowhere

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Frames the device holds ahead of the speaker: 200 ms, as Juno's device queue and
/// [`crate::mixer::Nowhere`] do.
pub const QUEUE: usize = 9_600;
/// The mixer's format: interleaved stereo.
pub const CHANNELS: usize = 2;
/// A frame of it, in bytes: what a sink write counts in.
pub const FRAME: usize = CHANNELS * 2;

/// Interleaved s16 frames, [`QUEUE`] deep, in the order they are to be played.
pub struct Ring {
    buf: Vec<i16>,
    /// Index in `buf` of the oldest queued sample.
    head: usize,
    /// Samples queued.
    len: usize,
}

impl Default for Ring {
    fn default() -> Ring {
        Ring::new()
    }
}

impl Ring {
    pub fn new() -> Ring {
        Ring { buf: vec![0; QUEUE * CHANNELS], head: 0, len: 0 }
    }

    /// Frames that fit.
    pub fn room(&self) -> usize {
        (self.buf.len() - self.len) / CHANNELS
    }

    /// Frames waiting to be played.
    pub fn queued(&self) -> usize {
        self.len / CHANNELS
    }

    /// Queues as many whole frames of `bytes` (little-endian s16 stereo) as fits: the bytes taken.
    pub fn push_le(&mut self, bytes: &[u8]) -> usize {
        let n = bytes.len().min(self.room() * FRAME) & !(FRAME - 1);
        let mut at = (self.head + self.len) % self.buf.len();
        for frame in bytes[..n].chunks_exact(FRAME) {
            for c in 0..CHANNELS {
                self.buf[at] = i16::from_le_bytes([frame[c * 2], frame[c * 2 + 1]]);
                at = self.next(at);
            }
        }
        self.len += n / 2;
        n
    }

    /// Plays up to `out.len() / CHANNELS` frames into `out` as f32 (`/ 32768`) scaled by `gain`,
    /// silence past what the ring had: the frames taken.
    pub fn play_f32(&mut self, out: &mut [f32], gain: f32) -> usize {
        let frames = self.fit(out.len() / CHANNELS);
        let scale = gain / 32768.0;
        let mut at = self.head;
        for s in &mut out[..frames * CHANNELS] {
            *s = self.buf[at] as f32 * scale;
            at = self.next(at);
        }
        out[frames * CHANNELS..].fill(0.0);
        self.taken(frames, at);
        frames
    }

    /// The same into the device's own s16, clamped to what it can hold.
    pub fn play_i16(&mut self, out: &mut [i16], gain: f32) -> usize {
        let frames = self.fit(out.len() / CHANNELS);
        let mut at = self.head;
        for s in &mut out[..frames * CHANNELS] {
            *s = (self.buf[at] as f32 * gain).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            at = self.next(at);
        }
        out[frames * CHANNELS..].fill(0);
        self.taken(frames, at);
        frames
    }

    /// Drops everything queued.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// How many of `want` frames the ring can give a callback that is about to copy them out.
    fn fit(&self, want: usize) -> usize {
        want.min(self.queued())
    }

    /// Finishes a copy of `frames` frames that ended at read mark `at`.
    fn taken(&mut self, frames: usize, at: usize) {
        self.head = at;
        self.len -= frames * CHANNELS;
    }

    /// The sample after `at`: the copy walks the ring by hand, one wrap test per sample, which is
    /// what an audio thread can afford.
    fn next(&self, at: usize) -> usize {
        let n = at + 1;
        if n == self.buf.len() { 0 } else { n }
    }
}

/// The counts, and the level, as the audio thread and the mixer thread see them.
pub struct Counter {
    /// Frames queued to the device.
    written: AtomicU64,
    /// Frames the callback took.
    consumed: AtomicU64,
    /// Linear gain, held as f32 bits; [`Counter::set_volume`] sets it from dB.
    gain: AtomicU32,
}

impl Default for Counter {
    fn default() -> Counter {
        Counter::new()
    }
}

impl Counter {
    pub fn new() -> Counter {
        Counter { written: AtomicU64::new(0), consumed: AtomicU64::new(0), gain: AtomicU32::new(1.0f32.to_bits()) }
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    /// dB as a linear gain, never above 1.0: the device's ceiling, which is where the mixer puts the
    /// page's volume under (the -24 dB it opens with).
    pub fn set_volume(&self, db: i32) {
        let gain = 10f32.powf(db as f32 / 20.0).min(1.0);
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub fn add_written(&self, frames: u64) {
        self.written.fetch_add(frames, Ordering::Release);
    }

    pub fn add_consumed(&self, frames: u64) {
        self.consumed.fetch_add(frames, Ordering::Release);
    }

    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// Frames heard: what the callback took, which is never more than what was given it.
    pub fn played(&self) -> u64 {
        self.consumed.load(Ordering::Acquire).min(self.written.load(Ordering::Acquire))
    }

    /// A flush: the queued frames are gone, so they count as played rather than as owed.
    pub fn mark_drained(&self) {
        let written = self.written.load(Ordering::Acquire);
        self.consumed.store(written, Ordering::Release);
    }

    /// A new stream counts from 0 again.
    pub fn reset(&self) {
        self.written.store(0, Ordering::Release);
        self.consumed.store(0, Ordering::Release);
    }
}

/// A [`Ring`] and its [`Counter`]: the sink's queue, shared between the thread that writes it and
/// the thread that plays it.
pub struct Queue {
    ring: Mutex<Ring>,
    count: Counter,
}

impl Queue {
    pub fn new() -> Queue {
        Queue { ring: Mutex::new(Ring::new()), count: Counter::new() }
    }

    /// Mixer thread: queues as much of `bytes` as fits, `WouldBlock` when nothing does — the
    /// contract of a nonblocking device write, as [`crate::mixer::Nowhere`] keeps it.
    pub fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        let mut ring = lock(&self.ring);
        if ring.room() == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = ring.push_le(bytes);
        drop(ring);
        self.count.add_written((n / FRAME) as u64);
        Ok(n)
    }

    /// The ring for one callback, or `None` when the mixer thread holds it.
    ///
    /// A held lock means the mixer thread is between its memcpy and its count, microseconds; the
    /// audio thread cannot wait for it, so that callback is silence — and silence from a withheld or
    /// empty ring is not played, so the mixer's clock does not run ahead on it. Poisoning takes a
    /// panic in the memcpy above, which leaves the ring coherent.
    fn claim(&self) -> Option<std::sync::MutexGuard<'_, Ring>> {
        match self.ring.try_lock() {
            Ok(ring) => Some(ring),
            Err(std::sync::TryLockError::WouldBlock) => None,
            Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        }
    }

    /// The callback: plays `out`, counting only the frames the ring gave.
    pub fn play_f32(&self, out: &mut [f32]) -> usize {
        let gain = self.count.gain();
        let Some(mut ring) = self.claim() else {
            out.fill(0.0);
            return 0;
        };
        let frames = ring.play_f32(out, gain);
        drop(ring);
        self.count.add_consumed(frames as u64);
        frames
    }

    /// The same for a device whose samples are s16.
    pub fn play_i16(&self, out: &mut [i16]) -> usize {
        let gain = self.count.gain();
        let Some(mut ring) = self.claim() else {
            out.fill(0);
            return 0;
        };
        let frames = ring.play_i16(out, gain);
        drop(ring);
        self.count.add_consumed(frames as u64);
        frames
    }

    pub fn played(&self) -> u64 {
        self.count.played()
    }

    pub fn written(&self) -> u64 {
        self.count.written()
    }

    /// Frames that fit right now.
    pub fn room(&self) -> usize {
        lock(&self.ring).room()
    }

    pub fn queued(&self) -> usize {
        lock(&self.ring).queued()
    }

    pub fn set_volume(&self, db: i32) {
        self.count.set_volume(db);
    }

    pub fn gain(&self) -> f32 {
        self.count.gain()
    }

    /// Drops what is queued, counts stay in step.
    pub fn flush(&self) {
        lock(&self.ring).clear();
        self.count.mark_drained();
    }

    /// Let go of the device: everything starts over with the next stream.
    pub fn close(&self) {
        lock(&self.ring).clear();
        self.count.reset();
    }
}

impl Default for Queue {
    fn default() -> Queue {
        Queue::new()
    }
}

fn lock(ring: &Mutex<Ring>) -> std::sync::MutexGuard<'_, Ring> {
    ring.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `frames` frames of `v` on the left and `-v` on the right, as the mixer writes it.
    fn bytes(frames: usize, v: i16) -> Vec<u8> {
        let mut b = Vec::with_capacity(frames * FRAME);
        for _ in 0..frames {
            b.extend_from_slice(&v.to_le_bytes());
            b.extend_from_slice(&(-v).to_le_bytes());
        }
        b
    }

    #[test]
    fn a_full_queue_would_block_and_playing_frees_room() {
        let q = Queue::new();
        assert_eq!(q.room(), QUEUE);
        assert_eq!(q.write(&bytes(QUEUE, 100)).unwrap(), QUEUE * FRAME);
        assert_eq!(q.room(), 0);
        assert_eq!(q.write(&bytes(1, 100)).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        let mut out = vec![0f32; 480 * CHANNELS];
        assert_eq!(q.play_f32(&mut out), 480);
        assert_eq!(q.room(), 480);
    }

    #[test]
    fn a_write_takes_whole_frames_only() {
        let q = Queue::new();
        assert_eq!(q.write(&[0u8; 7]).unwrap(), FRAME, "the tail of a partial frame waits for the next write");
        assert_eq!(q.written(), 1);
        assert_eq!(q.queued(), 1);
    }

    #[test]
    fn the_callback_converts_and_applies_the_gain() {
        let q = Queue::new();
        q.set_volume(-6);
        let gain = q.gain();
        assert!((gain - 10f32.powf(-6.0 / 20.0)).abs() < 1e-6, "{gain}");
        q.write(&bytes(2, 10_000)).unwrap();
        let mut out = vec![7.0f32; 2 * CHANNELS];
        assert_eq!(q.play_f32(&mut out), 2);
        assert!((out[0] - 10_000.0 * gain / 32768.0).abs() < 1e-6, "{out:?}");
        assert!((out[1] + 10_000.0 * gain / 32768.0).abs() < 1e-6, "each channel is its own sample");
        assert_eq!(q.played(), 2);
        let mut s16 = vec![0i16; 2 * CHANNELS];
        q.write(&bytes(2, 10_000)).unwrap();
        assert_eq!(q.play_i16(&mut s16), 2);
        assert_eq!(s16[0], 5_011, "10000 * 10^(-6/20), truncated as the mixer truncates");
        assert_eq!(s16[1], -5_011, "and its sign kept");
    }

    #[test]
    fn the_gain_never_exceeds_the_device_ceiling() {
        let q = Queue::new();
        q.set_volume(6);
        assert_eq!(q.gain(), 1.0, "above 0 dB the device clamps, as Juno's kernel does");
        q.set_volume(0);
        assert_eq!(q.gain(), 1.0);
        q.set_volume(-48);
        assert!(q.gain() < 0.02 && q.gain() > 0.0, "{}", q.gain());
    }

    #[test]
    fn an_empty_ring_plays_silence_and_is_not_counted() {
        let q = Queue::new();
        q.write(&bytes(100, 100)).unwrap();
        let mut out = vec![7.0f32; 480 * CHANNELS];
        assert_eq!(q.play_f32(&mut out), 100, "only what was queued");
        assert!(out[100 * CHANNELS..].iter().all(|&s| s == 0.0), "the rest of the callback is silence");
        assert_eq!(q.played(), 100);
        assert_eq!(q.play_f32(&mut out), 0, "running dry does not advance the clock");
        assert_eq!(q.played(), 100);
    }

    #[test]
    fn a_flush_drops_the_queue_and_keeps_the_counts_in_step() {
        let q = Queue::new();
        q.write(&bytes(2_000, 100)).unwrap();
        let mut out = vec![0f32; 480 * CHANNELS];
        assert_eq!(q.play_f32(&mut out), 480);
        q.flush();
        assert_eq!(q.queued(), 0);
        assert_eq!(q.played(), 2_000, "queued frames are dropped, not owed");
        assert_eq!(q.room(), QUEUE, "and the whole queue is free again");
    }

    #[test]
    fn played_never_exceeds_written() {
        let q = Queue::new();
        q.write(&bytes(10, 1)).unwrap();
        q.count.add_consumed(1_000); // a callback ahead of the write count, or counts from a stale stream
        assert_eq!(q.played(), 10);
        q.close();
        assert_eq!(q.played(), 0, "close starts the stream over, like Nowhere");
        assert_eq!(q.written(), 0);
        assert_eq!(q.queued(), 0);
        assert_eq!(q.room(), QUEUE);
    }

    /// The Mixer paces itself on these numbers: it writes while `written < played + LEAD`
    /// (`mixer::LEAD`, 4800 frames) into a queue of `QUEUE`.
    #[test]
    fn the_lead_arithmetic_of_the_mixer() {
        let q = Queue::new();
        assert_eq!(q.write(&bytes(9_600, 1_000)).unwrap(), 9_600 * FRAME);
        let mut out = vec![0f32; 4_800 * CHANNELS];
        assert_eq!(q.play_f32(&mut out), 4_800);
        assert_eq!(q.played(), 4_800);
        assert_eq!(q.room(), 4_800);
        assert_eq!(q.write(&bytes(4_800, 1_000)).unwrap(), 4_800 * FRAME);
        assert_eq!(q.write(&bytes(1, 0)).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(q.written(), 14_400, "written counts every frame ever queued");
        assert_eq!(q.written() - q.played(), QUEUE as u64, "and the queue is full again");
    }

    #[test]
    fn the_ring_wraps_without_losing_order() {
        let mut ring = Ring::new();
        let mut out = vec![0f32; CHANNELS];
        for i in 0..20_000u32 {
            // One frame in, one frame out, over the end of the buffer and back.
            assert_eq!(ring.push_le(&bytes(1, i as i16)), FRAME);
            assert_eq!(ring.play_f32(&mut out, 1.0), 1);
            assert_eq!(out[0], (i as i16) as f32 / 32768.0, "frame {i}");
            assert_eq!(out[1], -(i as i16) as f32 / 32768.0);
            assert_eq!(ring.queued(), 0);
        }
    }
}
