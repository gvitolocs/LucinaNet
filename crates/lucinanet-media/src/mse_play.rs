/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Playing from Media Source Extensions' track buffers: the same decode thread, pacing and clock as a file, with the packets
//! taken from what script appended instead of from a demuxer.
//!
//! - [`MseDemux`] is a `lucina_video::Demuxer` over the video track buffer: packets in decode order from a cursor; it waits
//!   when the next one has not been appended yet (the element is then starved, as a stalled network leaves it) and ends only
//!   after `endOfStream()`.
//! - [`MseSound`] is the sound: coded frames from the audio track buffer through `lucina_audio_decode::packet::PacketDecoder`
//!   into the player's mixer stream, the clock following it as for a file (`sound.rs`).
//!
//! The spec's seek with MSE ("seeking" waits until the new position is buffered) falls out of the waiting: a seek moves the
//! cursors to the random access point before the time, and they wait there until it is appended.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use lucina_audio_decode::packet::{AudioConfig, PacketDecoder};
use lucina_video::mse::{AudioCodec, TrackKind};
use lucina_video::{AudioOutput, Codec, Demuxer, Error, Packet, Track};

use crate::mixer::{Mixer, RATE, Stream};
use crate::mse::SourceBufferCore;

/// Everything a MediaSource holds, shared by script's calls, the decode thread and the sound thread.
pub struct MseShared {
    pub state: Mutex<MseState>,
    pub cv: Condvar,
    /// Which of the sound producer's three waits it is parked in, for the player's clock-stall watchdog to read without
    /// disturbing the producer: 0 working, 1 start, 2 frame, 3 room.
    pub producer_wait: AtomicU8,
    /// The recovery switch of `LUCINANET_MEDIA_WALLCLOCK_FALLBACK`: the sound reports no position at all, so the clock runs on
    /// the system clock, as it does while there is no audio sink (Firefox's rule). Survives a re-open, which makes a new sound.
    pub wall_only: std::sync::atomic::AtomicBool,
}

/// `MseShared::producer_wait` codes.
pub const PRODUCER_WORKING: u8 = 0;
pub const PRODUCER_START: u8 = 1;
pub const PRODUCER_FRAME: u8 = 2;
pub const PRODUCER_ROOM: u8 = 3;

/// The producer's wait as the watchdog prints it.
pub fn producer_wait_name(code: u8) -> &'static str {
    match code {
        PRODUCER_START => "start",
        PRODUCER_FRAME => "frame",
        PRODUCER_ROOM => "room",
        _ => "working",
    }
}

#[derive(Default)]
pub struct MseState {
    pub buffers: Vec<Option<SourceBufferCore>>,
    /// MediaSource.duration in microseconds; `None` NaN, `Some(i64::MAX)` +Infinity.
    pub duration_us: Option<i64>,
    pub ended: bool,
    pub closed: bool,
    /// Bumped by every seek, so a waiting reader knows to look again.
    pub seeks: u64,
}

impl MseShared {
    pub fn new() -> Arc<MseShared> {
        Arc::new(MseShared {
            state: Mutex::new(MseState::default()),
            cv: Condvar::new(),
            producer_wait: AtomicU8::new(PRODUCER_WORKING),
            wall_only: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, MseState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wakes every reader: frames were appended or removed, the stream ended, or the player is going away.
    pub fn changed(&self) {
        self.cv.notify_all();
    }

    /// Every SourceBuffer has had its first initialization segment (spec: HAVE_METADATA).
    pub fn all_initialized(&self) -> bool {
        let s = self.lock();
        let live: Vec<&SourceBufferCore> = s.buffers.iter().flatten().collect();
        !live.is_empty() && live.iter().all(|b| b.init().is_some())
    }

    /// The end of all buffered audio, across every live track: the highest presentation end any audio track reaches. The watchdog
    /// uses this rather than the track the producer is reading, so a producer parked on stale ids (P1) still counts as "audio is
    /// buffered beyond the position".
    pub fn audio_buffered_end_us(&self) -> Option<i64> {
        let s = self.lock();
        let mut end: Option<i64> = None;
        for b in s.buffers.iter().flatten() {
            for t in b.tracks.values() {
                if matches!(t.kind, Some(TrackKind::Audio { .. })) {
                    if let Some(e) = t.highest_pts() {
                        end = Some(end.map_or(e, |x: i64| x.max(e)));
                    }
                }
            }
        }
        end
    }
}

impl MseState {
    /// The buffer and track id holding the first track of this kind.
    fn track_of(&self, video: bool) -> Option<(usize, u64)> {
        for (i, b) in self.buffers.iter().enumerate() {
            let Some(b) = b else { continue };
            for (id, t) in &b.tracks {
                if t.is_video() == video {
                    return Some((i, *id));
                }
            }
        }
        None
    }
}

/// The video track as packets.
pub struct MseDemux {
    shared: Arc<MseShared>,
    buffer: usize,
    track_id: u64,
    track: Track,
    /// The last packet handed out, by its key in the track buffer.
    cursor: Option<(i64, u64)>,
    /// Where a seek asked to start: the first packet is the random access point at or before it.
    start_at: Option<i64>,
    seen_seeks: u64,
}

impl MseDemux {
    /// The first video track, once its initialization segment is in.
    pub fn open(shared: Arc<MseShared>) -> Option<MseDemux> {
        let s = shared.lock();
        let (buffer, track_id) = s.track_of(true)?;
        let b = s.buffers[buffer].as_ref()?;
        let Some(TrackKind::Video { codec, width, height, config }) = b.tracks[&track_id].kind.clone() else { return None };
        let duration_us = s.duration_us.filter(|d| *d != i64::MAX);
        let audio = s.track_of(false).map(|_| String::from("audio"));
        let seeks = s.seeks;
        drop(s);
        Some(MseDemux {
            shared,
            buffer,
            track_id,
            // The parser hands H.264 and HEVC as Annex B with parameter sets before key frames, as the file demuxers do: the
            // decoders need nothing from `config` here.
            track: Track { codec, width, height, duration_us, config, audio },
            cursor: None,
            start_at: Some(0),
            seen_seeks: seeks,
        })
    }
}

impl Demuxer for MseDemux {
    fn track(&self) -> &Track {
        &self.track
    }

    fn next_packet(&mut self) -> Result<Option<Packet>, Error> {
        let shared = self.shared.clone();
        let mut s = shared.lock();
        loop {
            if s.closed {
                return Ok(None);
            }
            let Some(Some(b)) = s.buffers.get(self.buffer) else { return Ok(None) };
            if !b.tracks.contains_key(&self.track_id) {
                // A second initialization segment maps the tracks to new ids (`init_received`, by kind order), and this reader's
                // (buffer, track id) is a snapshot of its own: follow the kind it reads instead of reporting an end of stream
                // that the data never reached (case F2).
                let Some((id, _)) = b.tracks.iter().find(|(_, t)| t.is_video()) else { return Ok(None) };
                let id = *id;
                trace!("video track {} is gone after a new initialization segment: reading {id}", self.track_id);
                self.track_id = id;
            }
            let Some(t) = b.tracks.get(&self.track_id) else { return Ok(None) };
            if let Some(at) = self.start_at {
                // After a start or a seek: the random access point before the time, once it is there.
                if let Some(k) = t.from_rap(at) {
                    let f = t.get(k).expect("listed");
                    self.start_at = None;
                    self.cursor = Some(k);
                    return Ok(Some(Packet { data: f.data.clone(), pts_us: f.pts_us, key: f.key }));
                }
            } else if let Some((k, f)) = t.next_after(self.cursor) {
                self.cursor = Some(k);
                return Ok(Some(Packet { data: f.data.clone(), pts_us: f.pts_us, key: f.key }));
            }
            if s.ended {
                return Ok(None);
            }
            s = shared.cv.wait_timeout(s, Duration::from_millis(250)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn seek_to(&mut self, pts_us: i64) -> Result<i64, Error> {
        let s = self.shared.lock();
        self.seen_seeks = s.seeks;
        self.start_at = Some(pts_us);
        self.cursor = None;
        // Where it will land, if that is buffered already; the time asked otherwise (it lands there once appended).
        let landed = s
            .buffers
            .get(self.buffer)
            .and_then(|b| b.as_ref())
            .and_then(|b| b.tracks.get(&self.track_id))
            .and_then(|t| t.from_rap(pts_us).and_then(|k| t.get(k)).map(|f| f.pts_us));
        Ok(landed.unwrap_or(pts_us))
    }
}

/// What the container said, as the packet decoder takes it.
fn audio_config(codec: &AudioCodec, config: &[u8]) -> Option<AudioConfig> {
    Some(match codec {
        AudioCodec::Aac => AudioConfig::Aac { asc: config.to_vec() },
        AudioCodec::Mp3 => AudioConfig::Mp3,
        AudioCodec::Vorbis => AudioConfig::Vorbis { codec_private: config.to_vec() },
        AudioCodec::Flac => AudioConfig::Flac { streaminfo: config.to_vec() },
        AudioCodec::Opus => AudioConfig::Opus { head: config.to_vec() },
        AudioCodec::Other(_) => return None,
    })
}

enum Cmd {
    Start(i64),
    Paused(bool),
    Stop,
}

/// The producer's three waits, named: which one it is parked in is the discriminator between F-b (no start command ever
/// arrives / the stream is not `playing`), P1 (the frame wait, on stale ids) and F-a (the room wait, on a `playing=false` ring).
const W_START: &str = "start";
const W_FRAME: &str = "frame";
const W_ROOM: &str = "room";

/// Where the producer's wait trace currently stands, so ENTER/LEAVE is logged once per transition and a still-waiting heartbeat
/// every 2 s, not on every 50-100 ms turn of the loop.
struct WaitTrace {
    kind: Option<&'static str>,
    since: std::time::Instant,
}

impl WaitTrace {
    fn new() -> WaitTrace {
        WaitTrace { kind: None, since: std::time::Instant::now() }
    }
}

/// The producer's state as one line, for the traces: only called when tracing, since it locks the shared buffers.
fn probe(shared: &MseShared, buffer: usize, track_id: u64, cursor: Option<(i64, u64)>, start_at: Option<i64>) -> String {
    let s = shared.lock();
    let b = s.buffers.get(buffer).and_then(|x| x.as_ref());
    let generation = b.map(|b| b.generation).unwrap_or(0);
    let t = b.and_then(|b| b.tracks.get(&track_id));
    let cur_pts = t.and_then(|t| cursor.and_then(|k| t.get(k)).map(|f| f.pts_us));
    let end = t.and_then(|t| t.highest_pts());
    let frames = t.map(|t| t.frame_count()).unwrap_or(0);
    let buffers = s.buffers.iter().filter(|x| x.is_some()).count();
    format!("cursor_pts={cur_pts:?} start_at={start_at:?} frames={frames} buffered_end_us={end:?} generation={generation} track={track_id} buffers={buffers} ended={} seeks={}", s.ended, s.seeks)
}

/// ENTER a wait, or the 2 s heartbeat if the producer is still parked in the same one.
fn wait_enter(tr: &mut WaitTrace, kind: &'static str, fields: &str) {
    let now = std::time::Instant::now();
    match tr.kind {
        Some(k) if k == kind => {
            trace!("audio wait {kind} still ({} ms): {fields}", now.duration_since(tr.since).as_millis());
            tr.since = now;
        },
        Some(k) => {
            trace!("audio wait {k} LEAVE after {} ms", now.duration_since(tr.since).as_millis());
            tr.kind = Some(kind);
            tr.since = now;
            trace!("audio wait {kind} ENTER: {fields}");
        },
        None => {
            tr.kind = Some(kind);
            tr.since = now;
            trace!("audio wait {kind} ENTER: {fields}");
        },
    }
}

/// LEAVE the current wait because the producer made progress.
fn wait_progress(tr: &mut WaitTrace) {
    if let Some(k) = tr.kind.take() {
        trace!("audio wait {k} LEAVE: progress, {} ms", tr.since.elapsed().as_millis());
    }
}

/// The sound of an MSE stream.
pub struct MseSound {
    stream: Arc<Stream>,
    commands: Option<std::sync::mpsc::Sender<Cmd>>,
    thread: Option<std::thread::JoinHandle<()>>,
    shared: Arc<MseShared>,
}

impl MseSound {
    /// The first audio track, if it is one the packet decoder takes.
    pub fn open(shared: Arc<MseShared>) -> Option<MseSound> {
        let (buffer, track_id, config, rate, channels) = {
            let s = shared.lock();
            let (buffer, track_id) = s.track_of(false)?;
            let b = s.buffers[buffer].as_ref()?;
            let Some(TrackKind::Audio { codec, config, rate, channels }) = b.tracks[&track_id].kind.clone() else { return None };
            let Some(c) = audio_config(&codec, &config) else {
                log::info!("lucinanet-media: MSE sound {codec:?} has no decoder here; the picture plays alone");
                return None;
            };
            (buffer, track_id, c, rate, channels)
        };
        let decoder = match PacketDecoder::new(&config, rate, channels) {
            Ok(d) => d,
            Err(e) => {
                log::info!("lucinanet-media: MSE sound: {e}");
                return None;
            },
        };
        let stream = Arc::new(Mixer::get().stream());
        let (tx, rx) = std::sync::mpsc::channel();
        let (st, sh) = (stream.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("lucinanet-mse-sound".into())
            .spawn(move || run_sound(decoder, st, sh, buffer, track_id, rx))
            .ok();
        Some(MseSound { stream, commands: Some(tx), thread, shared })
    }

    pub fn set_gain(&self, gain: f32) {
        self.stream.set_gain(gain);
    }

    pub fn drained(&self) -> bool {
        self.stream.drained()
    }

    /// Samples waiting in the mixer for this stream.
    pub fn queued(&self) -> usize {
        self.stream.queued()
    }

    /// The mixer's playing flag for this stream: the F-b path leaves this false with data ready to play.
    pub fn playing(&self) -> bool {
        self.stream.playing()
    }

    /// The mixer's held position for this stream. `Some` while `playing()` is true is the F4 latch: the mixer drains the ring but
    /// `position_us` keeps answering the pause-time frame, so the clock stands still for ever.
    pub fn held_frames(&self) -> Option<u64> {
        self.stream.held_frames()
    }

    /// Which of the producer's three waits it is parked in right now.
    pub fn producer_wait(&self) -> &'static str {
        producer_wait_name(self.shared.producer_wait.load(Ordering::Relaxed))
    }

    /// The end of all buffered audio, from any live audio track (see `MseShared::audio_buffered_end_us`).
    pub fn buffered_end_us(&self) -> Option<i64> {
        self.shared.audio_buffered_end_us()
    }

    /// The buffers this sound reads: the player's recovery opens another sound over the same MediaSource.
    pub fn shared_ref(&self) -> &Arc<MseShared> {
        &self.shared
    }

    /// The wall-clock fallback (`LUCINANET_MEDIA_WALLCLOCK_FALLBACK=1`): report no position, so the clock runs on the system
    /// clock while the sound carries on underneath. Firefox does this only when there is no sink at all.
    pub fn set_wall_only(&self, on: bool) {
        self.shared.wall_only.store(on, Ordering::Relaxed);
        self.shared.changed();
    }

    fn send(&self, c: Cmd) {
        if let Some(tx) = &self.commands {
            let _ = tx.send(c);
        }
        self.shared.changed();
    }
}

impl Drop for MseSound {
    fn drop(&mut self) {
        self.commands = None;
        self.stream.set_playing(false);
        self.shared.changed();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl AudioOutput for MseSound {
    fn position_us(&self) -> Option<i64> {
        // The wall-clock fallback: no position at all, so `Clock::now_us` uses the system clock.
        if self.shared.wall_only.load(Ordering::Relaxed) {
            return None;
        }
        self.stream.position_us()
    }
    fn start(&mut self, pts_us: i64) {
        self.send(Cmd::Start(pts_us));
    }
    fn set_paused(&mut self, paused: bool) {
        self.stream.set_playing(!paused);
        self.send(Cmd::Paused(paused));
    }
    fn seek(&mut self, _pts_us: i64) {
        // Counted, so a trace of a stopped feed can tell a seek's stop from a resume that never arrived: the readers used to say
        // `seeks=0` whatever happened, because nothing incremented it.
        self.shared.lock().seeks += 1;
        self.stream.set_playing(false);
        self.send(Cmd::Stop);
    }
}

fn run_sound(
    mut decoder: PacketDecoder,
    stream: Arc<Stream>,
    shared: Arc<MseShared>,
    mut buffer: usize,
    mut track_id: u64,
    commands: std::sync::mpsc::Receiver<Cmd>,
) {
    let mut paused = true;
    let mut cursor: Option<(i64, u64)> = None;
    let mut running = false;
    // Samples (stereo pairs) to drop before the start time, within the first frame.
    let mut skip_until: Option<i64> = None;
    let mut bytes = Vec::new();
    let mut samples: Vec<i16> = Vec::new();
    let mut sent = 0usize;
    let mut tr = WaitTrace::new();
    loop {
        loop {
            match commands.try_recv() {
                Ok(Cmd::Start(pts)) => {
                    trace!("audio cmd Start pts={pts} paused={paused} running={running}");
                    decoder.reset();
                    stream.flush(pts);
                    stream.set_playing(!paused);
                    samples.clear();
                    sent = 0;
                    // From the frame that holds `pts`: the last starting at or before it (every audio frame is a random access
                    // point). The cursor is the frame before it, as `next_after` hands out what follows the cursor.
                    let s = shared.lock();
                    let t = s.buffers.get(buffer).and_then(|b| b.as_ref()).and_then(|b| b.tracks.get(&track_id));
                    cursor = t.and_then(|t| t.from_rap(pts).and_then(|k| t.prev_key(k)));
                    drop(s);
                    skip_until = Some(pts);
                    running = true;
                    shared.producer_wait.store(PRODUCER_WORKING, Ordering::Relaxed);
                    wait_progress(&mut tr);
                },
                Ok(Cmd::Paused(p)) => {
                    trace!("audio cmd Paused {p}");
                    paused = p;
                    stream.set_playing(!p);
                },
                Ok(Cmd::Stop) => {
                    trace!("audio cmd Stop");
                    running = false;
                    stream.flush(0);
                },
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        }
        {
            // A second initialization segment maps the tracks to the ids it declares, by kind order (`init_received`). The
            // (buffer, track id) resolved at open can then name nothing while the frames — which rode along with the track
            // buffer — sit under another id of the same kind, and reading nothing looked like the end of the sound
            // (case F2: no reader watched for it).
            let s = shared.lock();
            if s.buffers.get(buffer).and_then(|b| b.as_ref()).and_then(|b| b.tracks.get(&track_id)).is_none()
                && let Some((nb, nt)) = s.track_of(false)
            {
                trace!("audio track {track_id} is gone after a new initialization segment: reading {nt} in buffer {nb}");
                buffer = nb;
                track_id = nt;
            }
        }
        if !running {
            // Waiting for a start: a seek or play() that never arrives leaves the sound here. F-b's "resume that never reaches
            // set_playing(true)" would instead show a Start that set `running` but left `playing=false` in the mixer.
            shared.producer_wait.store(PRODUCER_START, Ordering::Relaxed);
            if crate::tracing() {
                let f = format!(
                    "queued={} playing={} {}",
                    stream.queued(),
                    stream.playing(),
                    probe(&shared, buffer, track_id, cursor, skip_until)
                );
                wait_enter(&mut tr, W_START, &f);
            }
            let s = shared.lock();
            if s.closed {
                return;
            }
            drop(shared.cv.wait_timeout(s, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()));
            continue;
        }
        if sent < samples.len() {
            sent += stream.push(&samples[sent..]);
            if sent < samples.len() {
                // The ring is full and not draining: a `playing=false` stream never mixes, so this is where F-b stalls.
                shared.producer_wait.store(PRODUCER_ROOM, Ordering::Relaxed);
                if crate::tracing() {
                    let f = format!(
                        "queued={} playing={} position_us={:?} {}",
                        stream.queued(),
                        stream.playing(),
                        stream.position_us(),
                        probe(&shared, buffer, track_id, cursor, skip_until)
                    );
                    wait_enter(&mut tr, W_ROOM, &f);
                }
                stream.wait_for_room(Duration::from_millis(50));
            } else {
                shared.producer_wait.store(PRODUCER_WORKING, Ordering::Relaxed);
                wait_progress(&mut tr);
            }
            continue;
        }
        // The next coded frame.
        let next = {
            let s = shared.lock();
            if s.closed {
                return;
            }
            let t = s.buffers.get(buffer).and_then(|b| b.as_ref()).and_then(|b| b.tracks.get(&track_id));
            match t.and_then(|t| t.next_after(cursor)) {
                Some((k, f)) => Some((k, f.pts_us, f.duration_us, f.data.clone())),
                None => {
                    if s.ended {
                        stream.set_ended();
                        running = false;
                    } else {
                        // Nothing after the cursor while frames may exist before/behind it (F-a) or under other ids (P1): the
                        // buffered end and the generation say which.
                        shared.producer_wait.store(PRODUCER_FRAME, Ordering::Relaxed);
                        if crate::tracing() {
                            let f = format!(
                                "queued={} playing={} {}",
                                stream.queued(),
                                stream.playing(),
                                probe(&shared, buffer, track_id, cursor, skip_until)
                            );
                            wait_enter(&mut tr, W_FRAME, &f);
                        }
                        drop(shared.cv.wait_timeout(s, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()));
                    }
                    None
                },
            }
        };
        let Some((k, pts, duration, data)) = next else { continue };
        shared.producer_wait.store(PRODUCER_WORKING, Ordering::Relaxed);
        cursor = Some(k);
        bytes.clear();
        if let Err(e) = decoder.decode(&data, &mut bytes) {
            log::warn!("lucinanet-media: MSE sound stopped: {e}");
            running = false;
            stream.set_ended();
            continue;
        }
        samples.clear();
        samples.extend(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        sent = 0;
        if let Some(at) = skip_until.take()
            && at > pts
        {
            let frames = samples.len() / 2;
            let drop_frames = (((at - pts) as u64 * RATE as u64 / 1_000_000) as usize).min(frames);
            // A frame wholly before the time: drop it and look at the next with the same start time.
            if drop_frames >= frames && pts + duration <= at {
                skip_until = Some(at);
            }
            samples.drain(..drop_frames * 2);
        }
    }
}

/// The codec a `Codec` from the parser decodes with: as the file path (`lucina_video_play::decode::Decoder::for_track`).
pub fn video_codec_supported(codec: Codec) -> bool {
    matches!(codec, Codec::H264 | Codec::Vp9 | Codec::Av1) || (codec == Codec::Hevc && crate::mime::venus_present())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mse::TrackBuffer;
    use lucina_video::mse::{Format, Frame};

    /// `n` video frames of 40 ms, the first a random access point.
    fn video(n: usize) -> TrackBuffer {
        let mut t = TrackBuffer::new(TrackKind::Video { codec: Codec::H264, width: 64, height: 64, config: vec![] });
        for i in 0..n {
            let pts = i as i64 * 40_000;
            t.insert(Frame { track: 1, pts_us: pts, dts_us: pts, duration_us: 40_000, key: i == 0, data: vec![0u8; 8] });
        }
        t
    }

    /// case F2. A reader's `(buffer, track id)` is a snapshot of its own, and the next initialization segment maps the
    /// tracks onto the ids it declares (`init_received`, by kind order): the track moves, the frames with it, the snapshot stays.
    /// Finding nothing under the old id was the end of the stream, so a reader that outlived a re-key stopped the picture and
    /// — the audio reader does the same — reported the sound as over, with seconds of it still buffered.
    #[test]
    fn a_reader_follows_its_track_when_a_new_initialization_segment_moves_it() {
        let shared = MseShared::new();
        {
            let mut s = shared.lock();
            let mut core = SourceBufferCore::new(Format::IsoBmff, true);
            core.tracks.insert(1, video(10));
            s.buffers.push(Some(core));
        }
        let mut demux = MseDemux::open(shared.clone()).expect("a video track to read");
        assert!(demux.next_packet().unwrap().is_some(), "the first packet of the track");
        {
            let mut s = shared.lock();
            let core = s.buffers[0].as_mut().expect("the buffer");
            let moved = core.tracks.remove(&1).expect("the track");
            core.tracks.insert(7, moved);
            core.generation += 1;
            // Ended, so a reader that finds nothing says so in this test instead of waiting for a frame nobody appended.
            s.ended = true;
        }
        assert_eq!(demux.track_id, 1, "the reader holds the id of the first initialization segment");
        assert!(demux.next_packet().unwrap().is_some(), "the frames are still there, under another id: the stream has not ended");
        assert_eq!(demux.track_id, 7, "and the reader reads them");
    }
}
