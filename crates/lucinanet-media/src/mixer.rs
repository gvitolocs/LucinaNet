/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Every sound LucinaNet makes, mixed into Juno's one sound stream.
//!
//! Juno's `/dev/lucina-audio` takes 16-bit stereo at 48 kHz from one program at a time (`lucina_audio_decode::device`). A page may play
//! several `<audio>` and `<video>` elements and AudioContexts at once, as every browser allows, so each is a [`Stream`] here and one
//! thread sums them into the device. GStreamer's original gives each pipeline its own audio sink and leaves mixing to the sound server
//! (PulseAudio); Juno has none, so the browser is its own.
//!
//! Each stream knows where it is at the speaker: every mix block records which of the stream's frames went into which device frames,
//! and the device's played count says which of those have been heard (`rules::clock`, which Verus checks). That position is the A/V
//! clock a `<video>` follows.
//!
//! The device is opened when something plays and let go 2 s after the last sound, so the Video app or the music player can have it.
//! Where there is no device (the build host, QEMU, the device taken by another program) a stand-in plays at real time to nowhere, so the clock
//! and everything that follows it behave the same; `LUCINANET_MEDIA_WAV=<path>` also writes what is mixed to a WAV file, for tests.
//! `LUCINANET_MEDIA_SINK=none` takes that stand-in from the start, without asking any device for a stream, which is how a headless run
//! of a page with sound keeps the room quiet.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use lucina_audio_decode::device::{Device, Sink};

use crate::rules::clock;

pub const RATE: u32 = 48_000;
/// Frames in one mix block: 10 ms.
const BLOCK: usize = 480;
/// Mixed ahead of what the device has played, at most: what a pause or a volume change waits to be heard.
const LEAD: u64 = 4_800;
/// What a stream holds before `push` waits: 1 s.
pub const STREAM_CAP: usize = RATE as usize * 2;
/// The device's own level when LucinaNet opens it, as the Video app and the music player start; a page's volume scales under it.
const DEVICE_DB: i32 = -24;
/// Let go of the device after this much silence.
const IDLE_CLOSE: Duration = Duration::from_secs(2);

/// One mix block's record for a stream (see `rules::clock::frames_at`).
#[derive(Clone, Copy, Debug)]
struct Mark {
    dev_start: u64,
    stream_start: u64,
    taken: u64,
}

struct StreamState {
    /// Interleaved stereo at 48 kHz, waiting to be mixed.
    ring: VecDeque<i16>,
    playing: bool,
    gain: f32,
    /// The presentation time of the stream's frame 0 since the last flush.
    start_pts: i64,
    /// Frames mixed since the last flush.
    taken: u64,
    marks: VecDeque<Mark>,
    /// Frames heard, when paused: the position stays there.
    held: Option<u64>,
    /// No more will be pushed: the stream is over once its ring is empty and heard.
    ended: bool,
    /// The device generation the marks are in; a reopened device counts from 0 again.
    generation: u64,
}

struct MixState {
    streams: Vec<Option<StreamState>>,
    /// Frames written since the device was opened.
    written: u64,
    /// The device's played count, as last read.
    played: u64,
    generation: u64,
}

/// What a stream needs of the device to say where it is.
struct DeviceAt {
    played: u64,
    generation: u64,
}

struct Shared {
    state: Mutex<MixState>,
    cv: Condvar,
}

/// The one mixer.
pub struct Mixer(Arc<Shared>);

static MIXER: OnceLock<Mixer> = OnceLock::new();

impl Mixer {
    pub fn get() -> &'static Mixer {
        MIXER.get_or_init(|| Mixer::start(Box::new(open_sink)))
    }

    /// With the sinks a test gives.
    pub fn start(open: Box<dyn Fn() -> Box<dyn Sink> + Send>) -> Mixer {
        let shared = Arc::new(Shared {
            state: Mutex::new(MixState { streams: Vec::new(), written: 0, played: 0, generation: 0 }),
            cv: Condvar::new(),
        });
        let for_thread = shared.clone();
        std::thread::Builder::new()
            .name("lucinanet-mixer".into())
            .spawn(move || run(for_thread, open))
            .expect("the mixer's thread");
        Mixer(shared)
    }

    pub fn stream(&self) -> Stream {
        let mut s = lock(&self.0);
        let state = StreamState {
            ring: VecDeque::new(),
            playing: false,
            gain: 1.0,
            start_pts: 0,
            taken: 0,
            marks: VecDeque::new(),
            held: None,
            ended: false,
            generation: s.generation,
        };
        let id = match s.streams.iter().position(Option::is_none) {
            Some(i) => {
                s.streams[i] = Some(state);
                i
            },
            None => {
                s.streams.push(Some(state));
                s.streams.len() - 1
            },
        };
        Stream { shared: self.0.clone(), id }
    }
}

fn lock(shared: &Shared) -> MutexGuard<'_, MixState> {
    shared.state.lock().unwrap_or_else(|e| e.into_inner())
}

/// One sound in the mix.
pub struct Stream {
    shared: Arc<Shared>,
    id: usize,
}

impl Stream {
    fn with<T>(&self, f: impl FnOnce(&mut StreamState, &DeviceAt) -> T) -> T {
        let mut s = lock(&self.shared);
        let dev = DeviceAt { played: s.played, generation: s.generation };
        let st = s.streams[self.id].as_mut().expect("a live stream");
        f(st, &dev)
    }

    /// Takes as much of `samples` (interleaved stereo) as fits, and says how many samples that was.
    pub fn push(&self, samples: &[i16]) -> usize {
        let n = self.with(|st, _| {
            let room = STREAM_CAP.saturating_sub(st.ring.len());
            let n = samples.len().min(room) & !1;
            st.ring.extend(&samples[..n]);
            st.ended = false;
            n
        });
        self.shared.cv.notify_all();
        n
    }

    /// Samples waiting to be mixed.
    pub fn queued(&self) -> usize {
        self.with(|st, _| st.ring.len())
    }

    /// The stream's playing flag, as the mixer has it: what gates whether its ring is drained into the device.
    pub fn playing(&self) -> bool {
        self.with(|st, _| st.playing)
    }

    /// The frames the position is held at while the stream is paused, `None` while it plays.
    pub fn held_frames(&self) -> Option<u64> {
        self.with(|st, _| st.held)
    }

    /// Waits until there is room for some samples, or `limit` has passed.
    pub fn wait_for_room(&self, limit: Duration) {
        self.wait_below(STREAM_CAP, limit);
    }

    /// Waits until fewer than `samples` wait to be mixed, or `limit` has passed.
    pub fn wait_below(&self, samples: usize, limit: Duration) {
        let mut s = lock(&self.shared);
        let until = Instant::now() + limit;
        loop {
            let st = s.streams[self.id].as_ref().expect("a live stream");
            if st.ring.len() < samples {
                return;
            }
            let now = Instant::now();
            if now >= until {
                return;
            }
            s = self.shared.cv.wait_timeout(s, until - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    #[track_caller]
    pub fn set_playing(&self, playing: bool) {
        let (was, latched, noop, held, queued) = self.with(|st, mix| {
            let was = st.playing;
            if was != playing {
                if !playing {
                    st.held = Some(heard(st, mix));
                }
                st.playing = playing;
            }
            // A resume releases the latch whether or not the flag changed. `playing` is written from two places (the player's
            // thread and the sound thread, each with its own copy), so a resume can arrive with `held` still set from a pause
            // and the flag already true: the mixer would go on draining the ring while `position_us` answered the pause-time
            // frame for ever (case F4).
            let latched = if playing { st.held.take() } else { None };
            if let Some(h) = latched {
                // Frames mixed but not heard before the pause are lost with the device's queue only if it drained; count from what
                // was heard.
                st.marks.clear();
                st.taken = h;
            }
            (was, latched, was == playing, st.held, st.ring.len())
        });
        // Who turned it and to what, and where the position is held after a pause: the F-b path is a resume that never reaches
        // `playing=true`, so a transition that stops at false, or no transition at all, is what the trace has to show.
        if let Some(h) = latched {
            let at = std::panic::Location::caller();
            let what = if noop { "already playing" } else { "resumed" };
            trace!("mixer stream {} set_playing(true) {what}, latch {h} released from {}:{}", self.id, at.file(), at.line());
        } else if !noop {
            let at = std::panic::Location::caller();
            trace!(
                "mixer stream {} playing {was} -> {playing} held={held:?} queued={queued} from {}:{}",
                self.id,
                at.file(),
                at.line()
            );
        }
        self.shared.cv.notify_all();
    }

    /// The page's volume, 0 to 1, with mute applied by the caller.
    pub fn set_gain(&self, gain: f32) {
        self.with(|st, _| st.gain = gain.clamp(0.0, 1.0));
    }

    /// Drops what waits, and counts from `start_pts` with the next sample pushed (a seek, or a new start).
    pub fn flush(&self, start_pts: i64) {
        self.with(|st, mix| {
            st.ring.clear();
            st.marks.clear();
            st.taken = 0;
            st.held = if st.playing { None } else { Some(0) };
            st.start_pts = start_pts.clamp(-clock::MAX_PTS, clock::MAX_PTS);
            st.ended = false;
            st.generation = mix.generation;
        });
        self.shared.cv.notify_all();
    }

    /// Nothing more will be pushed.
    pub fn set_ended(&self) {
        self.with(|st, _| st.ended = true);
        self.shared.cv.notify_all();
    }

    /// Ended, and everything pushed has been heard.
    pub fn drained(&self) -> bool {
        self.with(|st, mix| st.ended && st.ring.is_empty() && heard(st, mix) >= st.taken)
    }

    /// The presentation time at the speaker, once a frame from the last flush has been heard.
    pub fn position_us(&self) -> Option<i64> {
        self.with(|st, mix| {
            let h = heard(st, mix);
            if h == 0 && st.held.is_none_or(|x| x == 0) && !st.marks.front().is_some_and(|m| mix.played > m.dev_start) {
                return None;
            }
            Some(clock::position_us(st.start_pts, h.min(clock::MAX_FRAMES), RATE as u64))
        })
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        lock(&self.shared).streams[self.id] = None;
        self.shared.cv.notify_all();
    }
}

/// Frames of the stream heard so far.
fn heard(st: &StreamState, mix: &DeviceAt) -> u64 {
    if let Some(h) = st.held {
        return h;
    }
    if st.generation != mix.generation {
        return st.taken;
    }
    // The last block that has begun playing.
    let Some(m) = st.marks.iter().rev().find(|m| m.dev_start <= mix.played).or(st.marks.front()) else {
        return st.taken;
    };
    clock::frames_at(mix.played, m.dev_start, m.stream_start, m.taken)
}

fn run(shared: Arc<Shared>, open: Box<dyn Fn() -> Box<dyn Sink> + Send>) {
    let mut sink: Option<Box<dyn Sink>> = None;
    let mut last_sound = Instant::now();
    let mut pending: Vec<u8> = Vec::new();
    let mut mix = vec![0i32; BLOCK * 2];
    let mut last_report = Instant::now();
    loop {
        let mut s = lock(&shared);
        // Every 2 s, what each stream looks like from the mixer: the clock chain's own numbers, so a frozen position can be
        // told from a stream that is simply not `playing`.
        if crate::tracing() && last_report.elapsed() >= Duration::from_secs(2) {
            last_report = Instant::now();
            let dev = DeviceAt { played: s.played, generation: s.generation };
            for (i, slot) in s.streams.iter().enumerate() {
                if let Some(st) = slot {
                    trace!(
                        "mixer stream {i}: taken={} heard={} held={:?} queued={} playing={} | device written={} played={}",
                        st.taken,
                        heard(st, &dev),
                        st.held,
                        st.ring.len(),
                        st.playing,
                        s.written,
                        s.played
                    );
                }
            }
        }
        if let Some(d) = sink.as_mut() {
            s.played = d.played_frames().min(s.written);
        }
        let generation = s.generation;
        let (played, written) = (s.played, s.written);
        for st in s.streams.iter_mut().flatten() {
            // Marks wholly heard, but the last, are done with.
            while st.marks.len() > 1 && st.marks[1].dev_start <= played {
                st.marks.pop_front();
            }
            if st.generation != generation {
                st.generation = generation;
                st.marks.clear();
            }
        }
        let wanted = s.streams.iter().flatten().any(|st| st.playing && !st.ring.is_empty());
        if !wanted && pending.is_empty() {
            if sink.is_some() && last_sound.elapsed() >= IDLE_CLOSE && played >= written {
                if let Some(mut d) = sink.take() {
                    d.close();
                }
                s.written = 0;
                s.played = 0;
                s.generation += 1;
            }
            let wait = if sink.is_some() { Duration::from_millis(20) } else { Duration::from_millis(500) };
            drop(shared.cv.wait_timeout(s, wait).unwrap_or_else(|e| e.into_inner()));
            continue;
        }
        if sink.is_none() {
            let mut d = open();
            d.set_volume(DEVICE_DB);
            sink = Some(d);
            s.written = 0;
            s.played = 0;
            s.generation += 1;
            let g = s.generation;
            for st in s.streams.iter_mut().flatten() {
                st.generation = g;
                st.marks.clear();
            }
        }
        last_sound = Instant::now();
        if pending.is_empty() && s.written < s.played + LEAD {
            mix.fill(0);
            let dev_start = s.written;
            for st in s.streams.iter_mut().flatten().filter(|st| st.playing) {
                let take = (st.ring.len() / 2).min(BLOCK);
                for (i, v) in st.ring.drain(..take * 2).enumerate() {
                    mix[i] += (v as f32 * st.gain) as i32;
                }
                if take > 0 {
                    st.marks.push_back(Mark { dev_start, stream_start: st.taken, taken: take as u64 });
                    st.taken += take as u64;
                }
            }
            pending.reserve(BLOCK * 4);
            for v in &mix {
                pending.extend_from_slice(&((*v).clamp(i16::MIN as i32, i16::MAX as i32) as i16).to_le_bytes());
            }
            s.written += BLOCK as u64;
            shared.cv.notify_all();
        }
        drop(s);
        let d = sink.as_mut().expect("opened above");
        if !pending.is_empty() {
            match d.write(&pending) {
                Ok(n) => {
                    pending.drain(..n);
                },
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(4)),
                Err(e) => {
                    log::warn!("lucinanet-media: the sound device failed ({e}); playing to nowhere");
                    d.close();
                    *d = Box::new(Nowhere::default());
                },
            }
        } else {
            // Ahead by LEAD: wait for the device to play some.
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// The real device, or the stand-in when it cannot be had; with `LUCINANET_MEDIA_WAV`, a copy to a file too.
fn open_sink() -> Box<dyn Sink> {
    let base: Box<dyn Sink> = base_sink();
    match std::env::var_os("LUCINANET_MEDIA_WAV") {
        Some(path) => match Wav::create(&path) {
            Ok(w) => Box::new(Tee { sink: base, wav: w }),
            Err(e) => {
                log::warn!("lucinanet-media: cannot write {path:?}: {e}");
                base
            },
        },
        None => base,
    }
}

/// What `LUCINANET_MEDIA_SINK` was set to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SinkChoice {
    /// Unset or empty: play to whatever device there is.
    Device,
    /// `none` or `null`: play to nothing, and never ask a device for one.
    Nowhere,
    /// A value that is neither, kept for the log: it behaves as `Device`, since a silent run nobody asked
    /// for is as much a mistake as a loud run somebody did.
    Unknown(std::ffi::OsString),
}

/// `LUCINANET_MEDIA_SINK` as a [`SinkChoice`], of the value and not of the process: a test hands the string it wants to
/// check, as `set_var` would race every other test in the binary.
pub fn sink_choice(raw: Option<&std::ffi::OsStr>) -> SinkChoice {
    let word = match raw {
        None => return SinkChoice::Device,
        Some(v) if v.is_empty() => return SinkChoice::Device,
        Some(v) => v,
    };
    match word.to_str() {
        Some(s) if s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("null") => SinkChoice::Nowhere,
        _ => SinkChoice::Unknown(word.to_os_string()),
    }
}

/// Said once, however many times the Mixer lets the device go and comes back to it.
static SINK_NONE_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the Mixer writes to: the Mac's CoreAudio output where there is one, Juno's own device
/// elsewhere, and the stand-in when neither can be had. `LUCINANET_MEDIA_SINK=none` takes it to the stand-in
/// without asking either, so a headless run cannot be heard.
fn base_sink() -> Box<dyn Sink> {
    match sink_choice(std::env::var_os("LUCINANET_MEDIA_SINK").as_deref()) {
        SinkChoice::Nowhere => {
            if !SINK_NONE_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                log::info!("lucinanet-media: sound sink none (LUCINANET_MEDIA_SINK=none): nothing is played");
            }
            return Box::new(Nowhere::default());
        },
        SinkChoice::Unknown(v) =>
            log::warn!("lucinanet-media: unknown LUCINANET_MEDIA_SINK {v:?}: playing to the device as if it were unset"),
        SinkChoice::Device => {},
    }
    #[cfg(target_os = "macos")]
    {
        let mut coreaudio = crate::coreaudio::CoreAudioSink::new();
        match coreaudio.open() {
            Ok(()) => return Box::new(coreaudio),
            Err(e) => log::info!("lucinanet-media: no CoreAudio output ({e}): trying Juno's device"),
        }
    }
    let mut device = Device::new();
    match device.open() {
        Ok(()) => Box::new(device),
        Err(e) => {
            log::info!("lucinanet-media: no sound device ({e}): sound plays to nowhere, at real time");
            Box::new(Nowhere::default())
        },
    }
}

/// Plays at real time to nowhere: the device's queue and played count, without the device. As a device does, it stops counting
/// when it has nothing left to play: a gap does not become time to make up.
#[derive(Default)]
pub struct Nowhere {
    /// When `played` was last brought up to date.
    last: Option<Instant>,
    played: u64,
    written: u64,
}

impl Nowhere {
    /// The device's own queue is about this long.
    const QUEUE: u64 = 9_600;

    fn advance(&mut self) {
        let now = Instant::now();
        if let Some(t) = self.last {
            let frames = (now.duration_since(t).as_micros() as u64 * RATE as u64) / 1_000_000;
            if frames > 0 {
                self.played = (self.played + frames).min(self.written);
                // Whole frames only, so time is never lost to rounding.
                self.last = Some(t + Duration::from_micros(frames * 1_000_000 / RATE as u64));
            }
        } else {
            self.last = Some(now);
        }
        if self.played >= self.written {
            // Empty: time stands until the next write.
            self.last = None;
        }
    }
}

impl Sink for Nowhere {
    fn open(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn close(&mut self) {
        *self = Nowhere::default();
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.advance();
        let room = (self.played + Nowhere::QUEUE).saturating_sub(self.written) as usize * 4;
        if room == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = bytes.len().min(room) & !3;
        self.written += n as u64 / 4;
        if self.last.is_none() && n > 0 {
            self.last = Some(Instant::now());
        }
        Ok(n)
    }
    fn flush(&mut self) {}
    fn set_volume(&mut self, _db: i32) {}
    fn played_frames(&mut self) -> u64 {
        self.advance();
        self.played
    }
}

/// A WAV file of 16-bit stereo at 48 kHz, its sizes written as it grows.
struct Wav {
    file: BufWriter<File>,
    bytes: u32,
}

impl Wav {
    fn create(path: &std::ffi::OsStr) -> io::Result<Wav> {
        let mut file = BufWriter::new(File::create(path)?);
        let mut h = Vec::with_capacity(44);
        h.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&RATE.to_le_bytes());
        h.extend_from_slice(&(RATE * 4).to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(b"data\0\0\0\0");
        file.write_all(&h)?;
        Ok(Wav { file, bytes: 0 })
    }

    fn append(&mut self, b: &[u8]) -> io::Result<()> {
        self.file.write_all(b)?;
        self.bytes = self.bytes.saturating_add(b.len() as u32);
        let at = self.file.stream_position()?;
        self.file.seek(SeekFrom::Start(4))?;
        self.file.write_all(&(36 + self.bytes).to_le_bytes())?;
        self.file.seek(SeekFrom::Start(40))?;
        self.file.write_all(&self.bytes.to_le_bytes())?;
        self.file.seek(SeekFrom::Start(at))?;
        self.file.flush()
    }
}

struct Tee {
    sink: Box<dyn Sink>,
    wav: Wav,
}

impl Sink for Tee {
    fn open(&mut self) -> io::Result<()> {
        self.sink.open()
    }
    fn close(&mut self) {
        self.sink.close();
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.sink.write(bytes)?;
        let _ = self.wav.append(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) {
        self.sink.flush();
    }
    fn set_volume(&mut self, db: i32) {
        self.sink.set_volume(db);
    }
    fn played_frames(&mut self) -> u64 {
        self.sink.played_frames()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    /// A device that records what it got and plays when told.
    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<(Vec<i16>, u64)>>);

    impl Sink for Fake {
        fn open(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn close(&mut self) {}
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut g = self.0.lock().unwrap();
            if g.0.len() as u64 / 2 >= g.1 + 2 * LEAD {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            g.0.extend(bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])));
            Ok(bytes.len())
        }
        fn flush(&mut self) {}
        fn set_volume(&mut self, _: i32) {}
        fn played_frames(&mut self) -> u64 {
            self.0.lock().unwrap().1
        }
    }

    fn wait_until(mut f: impl FnMut() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn two_streams_sum_and_each_knows_its_position() {
        let fake = Fake::default();
        let f2 = fake.clone();
        let mixer = Mixer::start(Box::new(move || Box::new(f2.clone())));
        let a = mixer.stream();
        let b = mixer.stream();
        a.flush(1_000_000);
        b.flush(0);
        a.push(&vec![100; 960 * 2]);
        b.push(&vec![-30; 480 * 2]);
        a.set_playing(true);
        b.set_playing(true);
        wait_until(|| fake.0.lock().unwrap().0.len() >= 960 * 2);
        {
            let g = fake.0.lock().unwrap();
            assert_eq!(g.0[0], 70, "summed");
            assert_eq!(g.0[480 * 2], 100, "b has ended");
        }
        assert_eq!(a.position_us(), None, "nothing heard yet");
        fake.0.lock().unwrap().1 = 240;
        wait_until(|| a.position_us() == Some(1_005_000));
        assert_eq!(b.position_us(), Some(5_000));
        fake.0.lock().unwrap().1 = 960;
        wait_until(|| a.position_us() == Some(1_020_000));
        assert_eq!(b.position_us(), Some(10_000), "b stops at its last frame");
    }

    #[test]
    fn a_pause_holds_the_position_and_volume_scales() {
        let fake = Fake::default();
        let f2 = fake.clone();
        let mixer = Mixer::start(Box::new(move || Box::new(f2.clone())));
        let a = mixer.stream();
        a.flush(0);
        a.set_gain(0.5);
        a.push(&vec![1000; 4800 * 2]);
        a.set_playing(true);
        wait_until(|| !fake.0.lock().unwrap().0.is_empty());
        assert_eq!(fake.0.lock().unwrap().0[0], 500);
        fake.0.lock().unwrap().1 = 480;
        wait_until(|| a.position_us() == Some(10_000));
        a.set_playing(false);
        fake.0.lock().unwrap().1 = 4800;
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(a.position_us(), Some(10_000));
    }

    /// case F4: the position latch outliving the pause it was set for. `playing` has two writers — the player's thread
    /// writes it directly, the sound thread writes it from its own copy of the truth — so a resume can arrive with the flag
    /// already true and `held` still set. The mixer then goes on draining the ring while `position_us` answers the pause-time
    /// frame, and the page's clock stands still for as long as it is asked. A resume releases the latch, flag changed or not.
    #[test]
    fn a_resume_releases_the_position_latch_even_if_it_was_already_playing() {
        // The stand-in sink, which hears frames in real time: the position moves by itself, so what the test holds still is the
        // latch and nothing else.
        let mixer = Mixer::start(Box::new(|| Box::new(Nowhere::default())));
        let a = mixer.stream();
        a.flush(0);
        a.push(&vec![0i16; 24_000 * 2]); // 500 ms, enough to outlast the waits below on a loaded machine
        a.set_playing(true);
        wait_until(|| a.position_us().is_some_and(|us| us > 0));
        a.set_playing(false);
        let frozen = a.position_us();
        assert!(frozen.is_some_and(|us| us > 0), "the pause holds the position it was at");
        // What the other writer leaves behind: the flag back to true, the latch of the pause still held.
        a.with(|st, _| st.playing = true);
        assert_eq!(a.position_us(), frozen, "the clock still answers the pause-time frame");
        // The resume the page asks for. The flag does not move: that is exactly when the latch has to go.
        a.set_playing(true);
        wait_until(|| a.position_us().is_some_and(|us| Some(us) > frozen));
        assert!(a.position_us().is_some_and(|us| Some(us) > frozen), "the clock moves again after the resume");
    }

    #[test]
    fn nowhere_plays_at_real_time_and_stops_when_empty() {
        let mut n = Nowhere::default();
        assert_eq!(n.write(&[0u8; 4 * 9_600]).unwrap(), 4 * 9_600);
        assert!(n.write(&[0u8; 4]).is_err());
        std::thread::sleep(Duration::from_millis(50));
        let p = n.played_frames();
        assert!((2_000..4_000).contains(&p), "{p}");
        // Run dry, wait, then write: the wait is not counted.
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(n.played_frames(), 9_600);
        std::thread::sleep(Duration::from_millis(100));
        n.write(&[0u8; 4 * 480]).unwrap();
        assert_eq!(n.played_frames(), 9_600);
        std::thread::sleep(Duration::from_millis(5));
        let p = n.played_frames();
        assert!((9_600..10_080).contains(&p), "{p}");
    }

    #[test]
    fn the_sink_choice_reads_a_value_and_not_the_process() {
        assert_eq!(sink_choice(None), SinkChoice::Device, "unset plays as it always did");
        assert_eq!(sink_choice(Some(OsStr::new(""))), SinkChoice::Device, "empty asks for nothing");
        assert_eq!(sink_choice(Some(OsStr::new("none"))), SinkChoice::Nowhere);
        assert_eq!(sink_choice(Some(OsStr::new("null"))), SinkChoice::Nowhere);
        assert_eq!(sink_choice(Some(OsStr::new("NONE"))), SinkChoice::Nowhere, "a capital must not turn the sound back on");
        for garbage in ["off", "0", "false", "nod", " none"] {
            match sink_choice(Some(OsStr::new(garbage))) {
                SinkChoice::Unknown(v) => assert_eq!(v, garbage, "kept for the warning"),
                other => panic!("{garbage:?} is not unknown to it: {other:?}"),
            }
        }
    }

    #[test]
    fn none_is_the_stand_in_and_a_whole_ring_is_heard_without_a_device() {
        // What the `none` choice opens. `base_sink` is not called, so this says nothing about a device and plays
        // nothing anywhere, on any OS: the point of the choice is that no device is ever asked.
        assert_eq!(sink_choice(Some(OsStr::new("none"))), SinkChoice::Nowhere);
        let mixer = Mixer::start(Box::new(|| Box::new(Nowhere::default())));
        let a = mixer.stream();
        a.flush(0);
        a.push(&vec![0i16; 4_800 * 2]); // 100 ms
        a.set_playing(true);
        a.set_ended();
        let t = Instant::now();
        wait_until(|| a.drained());
        assert_eq!(a.position_us(), Some(100_000), "the whole ring at the speaker");
        assert!(
            t.elapsed() >= Duration::from_millis(90),
            "real time, not as fast as it can write: {} ms for 100 ms",
            t.elapsed().as_millis()
        );
    }

    #[test]
    fn the_wav_tee_over_the_stand_in_holds_the_frames_the_page_had() {
        let path = std::env::temp_dir().join(format!("lucinanet-mixer-tee-{}.wav", std::process::id()));
        let for_factory = path.clone();
        let mixer = Mixer::start(Box::new(move || {
            let wav = Wav::create(for_factory.as_os_str()).expect("a temp WAV");
            Box::new(Tee { sink: Box::new(Nowhere::default()), wav })
        }));
        let a = mixer.stream();
        a.flush(0);
        a.push(&vec![500i16; 4_800 * 2]);
        a.set_playing(true);
        a.set_ended();
        // The page's clock moves although nothing is heard: 20 ms of its 100 ms, at real time.
        wait_until(|| a.position_us().is_some_and(|us| us >= 20_000));
        let early = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(early > 44, "audio was in the file while the page played: {early} bytes");
        wait_until(|| a.drained());
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(bytes >= 44 + 4_800 * 4, "all 100 ms of it: {bytes} bytes");
        // The sizes the header states, so a reader takes the file as WAV: what `sink-check.sh` reads with `wc -c`.
        let h = std::fs::read(&path).unwrap();
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()) as u64, bytes - 44, "data size");
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()) as u64, bytes - 8, "RIFF size");
        let _ = std::fs::remove_file(&path);
    }
}
