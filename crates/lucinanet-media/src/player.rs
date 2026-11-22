/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! One `<video>` or `<audio>`: servo-media's `Player` on Juno's decoders.
//!
//! The behavioural original is servo-media's GStreamer player (`backends/gstreamer/player.rs`), as Servo's `htmlmediaelement.rs`
//! relies on it:
//! - it starts paused; once the resource is known it sends `MetadataUpdated`, shows the first picture (`VideoFrameUpdated`) and sends
//!   `StateChanged(Paused)`, which takes the element to `HAVE_ENOUGH_DATA`;
//! - `play`/`pause` are answered with `StateChanged`; `PositionChanged` comes about every 100 ms while playing (GstPlay's default);
//! - `seek` is refused for a stream that is not seekable and past the duration, and answered with `SeekDone` at the time asked once a
//!   picture from there is up;
//! - `EndOfStream` when everything has been shown and heard; `Error` when the resource cannot be played.
//!
//! The playing itself is the Video app's (`lucina-app-video`): the decoding thread `lucina_video_play::worker`, the pacing
//! `lucina_video_play::pace` and the clock `lucina_video::Clock`, which follows the sound when there is sound. Its `played` line is
//! printed here too, with the counts the laptop tests read.

use std::collections::VecDeque;
use std::io::BufReader;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lucina_audio_decode::Decoder as SoundDecoder;
use lucina_video::{AudioOutput, Clock, Codec};
use lucina_video_play::decode::H264_REORDER;
use lucina_video_play::pace::{self, Step};
use lucina_video_play::worker::{Command, Worker};
use servo_base::generic_channel::{self, GenericCallback};
use servo_media::MediaInstanceError;
use servo_media_player::metadata::Metadata;
use servo_media_player::video::VideoFrameRenderer;
use servo_media_player::{PlaybackState, Player, PlayerError, PlayerEvent, SeekLock, StreamType};
use servo_media_streams::registry::MediaStreamId;
use servo_media_traits::MediaInstance;

#[cfg(feature = "mse")]
use servo_media_player::mse::{AppendMode, AppendResult, MseError, MseTrack, SourceBufferId};

use crate::frames::Frames;
use crate::hold;
use crate::sound::Sound;
use crate::source::{Ask, Source};

/// GstPlay's position update interval.
const POSITION_EVERY: Duration = Duration::from_millis(100);

enum Cmd {
    Play,
    Pause,
    Seek(f64),
    Gain,
}

struct Control {
    commands: VecDeque<Cmd>,
    paused: bool,
    can_resume: bool,
    muted: bool,
    volume: f64,
    rate: f64,
    metadata: Option<Metadata>,
    closed: bool,
    /// Fed by a MediaSource (`mse_attach`), not by `push_data`.
    #[cfg_attr(not(feature = "mse"), allow(dead_code))]
    mse_attached: bool,
}

struct Shared {
    control: Mutex<Control>,
    wake: Wake,
    /// Video frames shown and dropped, for `getVideoPlaybackQuality()` (Servo patch 0057).
    shown: std::sync::atomic::AtomicU64,
    dropped: std::sync::atomic::AtomicU64,
    #[cfg(feature = "mse")]
    mse: Arc<crate::mse_play::MseShared>,
    /// This player's MSE call recorder, when `LUCINANET_MEDIA_MSE_DUMP` names a directory.
    #[cfg(feature = "mse")]
    mse_dump: Option<crate::mse_dump::Recorder>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Control> {
        self.control.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn command(&self, c: Cmd) {
        self.lock().commands.push_back(c);
        self.wake.tell();
    }
}

/// What a player is made with besides what servo-media gives.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Every picture shown, as raw I420 in display order, for comparing with a reference decoder (`LUCINANET_MEDIA_DUMP_YUV`).
    pub dump_yuv: Option<std::path::PathBuf>,
}

pub struct LucinaPlayer {
    id: usize,
    shared: Arc<Shared>,
    source: Source,
    stream_type: StreamType,
    #[cfg_attr(not(feature = "mse"), allow(dead_code))]
    observer: GenericCallback<PlayerEvent>,
}

impl LucinaPlayer {
    pub fn new(
        id: usize,
        stream_type: StreamType,
        observer: GenericCallback<PlayerEvent>,
        renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        options: Options,
    ) -> LucinaPlayer {
        let shared = Arc::new(Shared {
            shown: Default::default(),
            dropped: Default::default(),
            control: Mutex::new(Control {
                commands: VecDeque::new(),
                paused: true,
                can_resume: false,
                muted: false,
                volume: 1.0,
                rate: 1.0,
                metadata: None,
                closed: false,
                mse_attached: false,
            }),
            wake: Wake::new(),
            #[cfg(feature = "mse")]
            mse: crate::mse_play::MseShared::new(),
            #[cfg(feature = "mse")]
            mse_dump: crate::mse_dump::recorder(),
        });
        let source = Source::new(asker(observer.clone()));
        let (s, src, events) = (shared.clone(), source.clone(), observer.clone());
        if let Err(e) = std::thread::Builder::new()
            .name("lucinanet-player".into())
            .spawn(move || Presenter::run(id, s, src, observer, renderer, options))
        {
            log::error!("lucinanet-media: no player thread: {e}");
        }
        LucinaPlayer { id, shared, source, stream_type, observer: events }
    }

    #[cfg(feature = "mse")]
    fn mse_add_source_buffer_inner(&self, mime: &str) -> Result<SourceBufferId, MseError> {
        let essence = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let Some(format) = lucina_video::mse::format_of(&essence) else { return Err(MseError::NotSupported) };
        if crate::mime::can_play_type(mime, crate::mime::venus_present()) == servo_media::SupportsMediaType::No {
            return Err(MseError::NotSupported);
        }
        let core = crate::mse::SourceBufferCore::new(format, essence.starts_with("video/"));
        let mut m = self.shared.mse.lock();
        m.buffers.push(Some(core));
        Ok(m.buffers.len() - 1)
    }

    #[cfg(feature = "mse")]
    fn mse_append_inner(&self, id: SourceBufferId, data: &[u8], current_time: f64) -> Result<AppendResult, MseError> {
        let mut m = self.shared.mse.lock();
        let ended = m.ended;
        let Some(Some(b)) = m.buffers.get_mut(id) else { return Err(MseError::Decode("no such SourceBuffer".into())) };
        b.make_room(data.len(), secs_to_us(current_time)).map_err(|_| MseError::QuotaExceeded)?;
        let result = b.append(data);
        let out = match result {
            Ok(done) => Ok(AppendResult {
                init_segment: done.init.as_ref().map(|i| {
                    i.tracks
                        .iter()
                        .map(|t| MseTrack {
                            id: t.id,
                            video: matches!(t.kind, lucina_video::mse::TrackKind::Video { .. }),
                            codec: t.codec_name.clone(),
                        })
                        .collect()
                }),
                init_duration: done.init.as_ref().and_then(|i| i.duration_us).map(us_to_secs),
                added_frames: done.frames > 0,
                buffered: to_secs(&b.buffered(ended)),
                timestamp_offset: us_to_secs(b.timestamp_offset_us),
            }),
            Err(crate::mse::AppendError::Quota) => Err(MseError::QuotaExceeded),
            Err(e) => Err(MseError::Decode(format!("{e:?}"))),
        };
        drop(m);
        self.shared.mse.changed();
        out
    }
}

/// What the byte cache asks becomes events; a new fetch waits for script's unlock on a thread of its own, as GStreamer's `seek-data`
/// waits on its `SeekChannel`.
fn asker(observer: GenericCallback<PlayerEvent>) -> crate::source::Asker {
    Box::new(move |ask, source| match ask {
        Ask::NeedData => {
            let _ = observer.send(PlayerEvent::NeedData);
        },
        Ask::EnoughData => {
            let _ = observer.send(PlayerEvent::EnoughData);
        },
        Ask::FetchFrom(offset) => {
            let Some((lock_tx, lock_rx)) = generic_channel::channel() else {
                log::error!("lucinanet-media: no channel for a seek lock");
                return;
            };
            let _ = observer.send(PlayerEvent::SeekData(offset, SeekLock { lock_channel: lock_tx }));
            let source = source.clone();
            let spawned = std::thread::Builder::new().name("lucinanet-fetch".into()).spawn(move || match lock_rx.recv() {
                Ok((ok, ack)) => {
                    source.fetch_started(offset, ok);
                    let _ = ack.send(());
                },
                Err(_) => source.fetch_started(offset, false),
            });
            if let Err(e) = spawned {
                log::error!("lucinanet-media: no thread to wait for a fetch: {e}");
            }
        },
    })
}

impl Drop for LucinaPlayer {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        #[cfg(feature = "mse")]
        {
            self.shared.mse.lock().closed = true;
            self.shared.mse.changed();
        }
        self.shared.wake.tell();
        self.source.close();
        hold::set(self.id, false);
    }
}

/// which command reached the player, and from where. A pause is what stops the sound and with it the clock, so a frozen
/// position has to say who asked for it: the thread, the player's own mirror at that moment, and the frames above this one.
/// Only with `LUCINANET_MEDIA_TRACE`; a handful of lines per run.
fn command_trace(what: &str, paused: bool, can_resume: bool) {
    if !crate::tracing() {
        return;
    }
    let bt = std::backtrace::Backtrace::force_capture().to_string();
    let frames: Vec<&str> = bt.lines().skip(1).take(7).map(str::trim).collect();
    trace!(
        "player {what} on {} mirror(paused={paused},can_resume={can_resume})",
        std::thread::current().name().unwrap_or("?")
    );
    for f in frames {
        if crate::tracing() {
            eprintln!("lucinanet-media:   {f}");
        }
    }
}

impl MediaInstance for LucinaPlayer {
    fn get_id(&self) -> usize {
        self.id
    }
    fn mute(&self, val: bool) -> Result<(), MediaInstanceError> {
        trace!("player mute {val} on {}", std::thread::current().name().unwrap_or("?"));
        self.set_mute(val).map_err(|_| MediaInstanceError)
    }
    fn suspend(&self) -> Result<(), MediaInstanceError> {
        trace!("player suspend on {}", std::thread::current().name().unwrap_or("?"));
        self.pause().map_err(|_| MediaInstanceError)
    }
    fn resume(&self) -> Result<(), MediaInstanceError> {
        trace!("player resume on {}", std::thread::current().name().unwrap_or("?"));
        if self.shared.lock().can_resume { self.play().map_err(|_| MediaInstanceError) } else { Ok(()) }
    }
}

impl Player for LucinaPlayer {
    fn play(&self) -> Result<(), PlayerError> {
        let mut c = self.shared.lock();
        command_trace("play", c.paused, c.can_resume);
        // the command reaches the presenter even when the mirror says we are already playing. The mirror is not the
        // sound: a seek stops the feed and a `Clock::stop` leaves the clock's own mirror unpaused, so a play that returned here
        // as a no-op left the mixer not-playing with its position latched, the clock answering the pause-time frame for ever.
        c.paused = false;
        c.can_resume = false;
        drop(c);
        self.shared.command(Cmd::Play);
        Ok(())
    }

    fn pause(&self) -> Result<(), PlayerError> {
        let mut c = self.shared.lock();
        command_trace("pause", c.paused, c.can_resume);
        c.paused = true;
        c.can_resume = true;
        drop(c);
        self.shared.command(Cmd::Pause);
        Ok(())
    }

    fn paused(&self) -> bool {
        self.shared.lock().paused
    }

    fn can_resume(&self) -> bool {
        self.shared.lock().can_resume
    }

    fn stop(&self) -> Result<(), PlayerError> {
        let mut c = self.shared.lock();
        c.paused = true;
        c.can_resume = false;
        c.closed = true;
        drop(c);
        self.shared.wake.tell();
        self.source.close();
        hold::set(self.id, false);
        Ok(())
    }

    fn seek(&self, time: f64) -> Result<(), PlayerError> {
        trace!("player seek {time:.3} on {}", std::thread::current().name().unwrap_or("?"));
        if self.stream_type != StreamType::Seekable {
            return Err(PlayerError::NonSeekableStream);
        }
        if let Some(d) = self.shared.lock().metadata.as_ref().and_then(|m| m.duration)
            && d < Duration::from_secs_f64(time.max(0.0))
        {
            return Err(PlayerError::SeekOutOfRange);
        }
        self.shared.command(Cmd::Seek(time.max(0.0)));
        Ok(())
    }

    fn seekable(&self) -> Vec<Range<f64>> {
        let c = self.shared.lock();
        if let Some(m) = &c.metadata
            && m.is_seekable
            && let Some(d) = m.duration
        {
            return vec![0.0..d.as_secs_f64()];
        }
        drop(c);
        self.buffered()
    }

    fn set_mute(&self, muted: bool) -> Result<(), PlayerError> {
        let mut c = self.shared.lock();
        if c.muted == muted {
            return Ok(());
        }
        c.muted = muted;
        drop(c);
        self.shared.command(Cmd::Gain);
        Ok(())
    }

    fn muted(&self) -> bool {
        self.shared.lock().muted
    }

    fn set_volume(&self, volume: f64) -> Result<(), PlayerError> {
        let mut c = self.shared.lock();
        if c.volume == volume {
            return Ok(());
        }
        c.volume = volume.clamp(0.0, 1.0);
        drop(c);
        self.shared.command(Cmd::Gain);
        Ok(())
    }

    fn volume(&self) -> f64 {
        self.shared.lock().volume
    }

    fn set_input_size(&self, size: u64) -> Result<(), PlayerError> {
        self.source.set_size(size);
        Ok(())
    }

    fn set_seekable(&self, seekable: bool) -> Result<(), PlayerError> {
        self.source.set_seekable(seekable);
        Ok(())
    }

    fn set_download_buffering_enabled(&self, _enabled: bool) -> Result<(), PlayerError> {
        Ok(())
    }

    fn set_playback_rate(&self, playback_rate: f64) -> Result<(), PlayerError> {
        if self.stream_type != StreamType::Seekable {
            return Err(PlayerError::NonSeekableStream);
        }
        // Kept, not applied: playing at another rate needs the sound stretched (docs/media.md, open items).
        self.shared.lock().rate = playback_rate;
        Ok(())
    }

    fn playback_rate(&self) -> f64 {
        self.shared.lock().rate
    }

    fn push_data(&self, data: Vec<u8>) -> Result<(), PlayerError> {
        self.source.push(&data).map_err(|()| PlayerError::EnoughData)
    }

    fn end_of_stream(&self) -> Result<(), PlayerError> {
        self.source.end_of_stream();
        Ok(())
    }

    fn buffered(&self) -> Vec<Range<f64>> {
        #[cfg(feature = "mse")]
        if self.shared.lock().mse_attached {
            // The intersection of every SourceBuffer's ranges (MSE, the HTMLMediaElement buffered extension).
            let m = self.shared.mse.lock();
            let mut acc: Option<Vec<(i64, i64)>> = None;
            for b in m.buffers.iter().flatten() {
                let r = b.buffered(m.ended);
                acc = Some(match acc {
                    None => r,
                    Some(a) => crate::mse::intersect(&a, &r),
                });
            }
            return to_secs(&acc.unwrap_or_default());
        }
        // As GStreamer's percent buffering query: the bytes held as a share of the resource, times the duration.
        let Some(d) = self.shared.lock().metadata.as_ref().and_then(|m| m.duration) else {
            return Vec::new();
        };
        let Some(size) = self.source.size().filter(|s| *s > 0) else {
            return Vec::new();
        };
        let d = d.as_secs_f64();
        self.source
            .ranges()
            .into_iter()
            .map(|(a, b)| (a as f64 / size as f64 * d)..(b.min(size) as f64 / size as f64 * d))
            .collect()
    }

    fn set_stream(&self, _stream: &MediaStreamId, _only_stream: bool) -> Result<(), PlayerError> {
        // MediaStream playback (getUserMedia into <video>) is not supported on Juno yet.
        Err(PlayerError::SetStreamFailed)
    }

    fn render_use_gl(&self) -> bool {
        false
    }

    fn set_audio_track(&self, _stream_index: i32, _enabled: bool) -> Result<(), PlayerError> {
        Ok(())
    }

    fn set_video_track(&self, _stream_index: i32, _enabled: bool) -> Result<(), PlayerError> {
        Ok(())
    }

    fn playback_quality(&self) -> Option<(u64, u64)> {
        use std::sync::atomic::Ordering::Relaxed;
        let dropped = self.shared.dropped.load(Relaxed);
        Some((self.shared.shown.load(Relaxed) + dropped, dropped))
    }

    // Media Source Extensions (servo-media `Player::mse_*`, LucinaNet patch). Times in seconds there, microseconds here.
    #[cfg(feature = "mse")]
    fn mse_attach(&self) -> Result<(), MseError> {
        if let Some(r) = &self.shared.mse_dump { r.attach(); }
        self.shared.lock().mse_attached = true;
        // `open` is waiting on the byte source: closing it ends that, and the player thread goes on with the MediaSource.
        self.source.close();
        Ok(())
    }

    #[cfg(feature = "mse")]
    fn mse_add_source_buffer(&self, mime: &str) -> Result<SourceBufferId, MseError> {
        let out = self.mse_add_source_buffer_inner(mime);
        if let Some(r) = &self.shared.mse_dump { r.add_source_buffer(mime, &out); }
        out
    }

    #[cfg(feature = "mse")]
    fn mse_remove_source_buffer(&self, id: SourceBufferId) {
        if let Some(r) = &self.shared.mse_dump { r.remove_source_buffer(id); }
        if let Some(b) = self.shared.mse.lock().buffers.get_mut(id) {
            *b = None;
        }
        self.shared.mse.changed();
    }

    #[cfg(feature = "mse")]
    fn mse_append(&self, id: SourceBufferId, data: Vec<u8>, current_time: f64) -> Result<AppendResult, MseError> {
        let out = self.mse_append_inner(id, &data, current_time);
        if let Some(r) = &self.shared.mse_dump { r.append(id, current_time, &data, &out); }
        out
    }

    #[cfg(feature = "mse")]
    fn mse_remove(&self, id: SourceBufferId, start: f64, end: f64) -> Vec<Range<f64>> {
        if let Some(r) = &self.shared.mse_dump { r.remove(id, start, end); }
        let mut m = self.shared.mse.lock();
        let ended = m.ended;
        let out = match m.buffers.get_mut(id) {
            Some(Some(b)) => {
                b.remove(secs_to_us(start), if end.is_finite() { secs_to_us(end) } else { i64::MAX });
                to_secs(&b.buffered(ended))
            },
            _ => Vec::new(),
        };
        drop(m);
        self.shared.mse.changed();
        out
    }

    #[cfg(feature = "mse")]
    fn mse_reset_parser(&self, id: SourceBufferId) {
        if let Some(r) = &self.shared.mse_dump { r.reset_parser(id); }
        if let Some(Some(b)) = self.shared.mse.lock().buffers.get_mut(id) {
            b.reset_parser();
        }
    }

    #[cfg(feature = "mse")]
    fn mse_set_mode(&self, id: SourceBufferId, mode: AppendMode) {
        if let Some(r) = &self.shared.mse_dump { r.set_mode(id, mode); }
        if let Some(Some(b)) = self.shared.mse.lock().buffers.get_mut(id) {
            b.set_mode(match mode {
                AppendMode::Segments => crate::mse::Mode::Segments,
                AppendMode::Sequence => crate::mse::Mode::Sequence,
            });
        }
    }

    #[cfg(feature = "mse")]
    fn mse_set_timestamp_offset(&self, id: SourceBufferId, offset: f64) {
        if let Some(r) = &self.shared.mse_dump { r.set_timestamp_offset(id, offset); }
        if let Some(Some(b)) = self.shared.mse.lock().buffers.get_mut(id) {
            b.timestamp_offset_us = secs_to_us(offset);
        }
    }

    #[cfg(feature = "mse")]
    fn mse_set_append_window(&self, id: SourceBufferId, start: f64, end: f64) {
        if let Some(r) = &self.shared.mse_dump { r.set_append_window(id, start, end); }
        if let Some(Some(b)) = self.shared.mse.lock().buffers.get_mut(id) {
            b.append_window_start_us = secs_to_us(start);
            b.append_window_end_us = end.is_finite().then(|| secs_to_us(end));
        }
    }

    #[cfg(feature = "mse")]
    fn mse_change_type(&self, id: SourceBufferId, mime: &str) -> Result<(), MseError> {
        if let Some(r) = &self.shared.mse_dump { r.change_type(id, mime); }
        let essence = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let Some(format) = lucina_video::mse::format_of(&essence) else { return Err(MseError::NotSupported) };
        if crate::mime::can_play_type(mime, crate::mime::venus_present()) == servo_media::SupportsMediaType::No {
            return Err(MseError::NotSupported);
        }
        match self.shared.mse.lock().buffers.get_mut(id) {
            Some(Some(b)) => {
                b.change_type(format);
                Ok(())
            },
            _ => Err(MseError::NotSupported),
        }
    }

    #[cfg(feature = "mse")]
    fn mse_buffered(&self, id: SourceBufferId) -> Vec<Range<f64>> {
        let m = self.shared.mse.lock();
        match m.buffers.get(id) {
            Some(Some(b)) => to_secs(&b.buffered(m.ended)),
            _ => Vec::new(),
        }
    }

    #[cfg(feature = "mse")]
    fn mse_set_duration(&self, duration: f64) {
        if let Some(r) = &self.shared.mse_dump { r.set_duration(duration); }
        let us = if duration.is_nan() {
            None
        } else if duration.is_infinite() {
            Some(i64::MAX)
        } else {
            Some(secs_to_us(duration))
        };
        self.shared.mse.lock().duration_us = us;
        let mut c = self.shared.lock();
        if let Some(md) = c.metadata.as_mut() {
            let d = us.filter(|d| *d != i64::MAX).map(|d| Duration::from_micros(d.max(0) as u64));
            md.duration = d;
            drop(c);
            let _ = self.observer.send(PlayerEvent::DurationChanged(d));
        }
    }

    #[cfg(feature = "mse")]
    fn mse_end_of_stream(&self) {
        if let Some(r) = &self.shared.mse_dump { r.end_of_stream(); }
        self.shared.mse.lock().ended = true;
        self.shared.mse.changed();
    }

    #[cfg(feature = "mse")]
    fn mse_reopen(&self) {
        if let Some(r) = &self.shared.mse_dump { r.reopen(); }
        self.shared.mse.lock().ended = false;
        self.shared.mse.changed();
    }
}

/// A pipe the decoding thread and the player's methods write a byte to, which the presenter sleeps on.
struct Wake {
    read: i32,
    write: i32,
}

impl Wake {
    fn new() -> Wake {
        match crate::sys::nonblocking_cloexec_pipe() {
            Ok([read, write]) => Wake { read, write },
            // Neither end exists, as when the pipe could not be made: a tell is lost and a wait runs to its limit.
            Err(e) => {
                log::error!("lucinanet-media: no pipe: {e}");
                Wake { read: -1, write: -1 }
            },
        }
    }

    fn tell(&self) {
        let b = 1u8;
        // SAFETY: one byte from a live local; a full non-blocking pipe means the other side is awake already.
        unsafe { libc::write(self.write, (&b as *const u8).cast(), 1) };
    }

    /// Sleeps until told or `limit` has passed, then empties the pipe.
    fn wait(&self, limit: Duration) {
        let mut p = libc::pollfd { fd: self.read, events: libc::POLLIN, revents: 0 };
        let ms = limit.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: poll on one live pollfd.
        unsafe { libc::poll(&mut p, 1, ms) };
        let mut buf = [0u8; 64];
        // SAFETY: reads into a live buffer of its length from our own non-blocking descriptor.
        while unsafe { libc::read(self.read, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: closes the two descriptors this owns, once.
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}

/// The sound, or none: what the clock follows.
enum Audio {
    Sound(Sound),
    #[cfg(feature = "mse")]
    Mse(crate::mse_play::MseSound),
    Silent,
}

impl Audio {
    fn set_gain(&self, gain: f32) {
        match self {
            Audio::Sound(s) => s.set_gain(gain),
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.set_gain(gain),
            Audio::Silent => {},
        }
    }

    /// All of it heard; true with no sound.
    fn drained(&self) -> bool {
        match self {
            Audio::Sound(s) => s.drained(),
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.drained(),
            Audio::Silent => true,
        }
    }

    /// Samples waiting in the mixer for the sound, 0 with none.
    fn queued(&self) -> usize {
        match self {
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.queued(),
            _ => 0,
        }
    }

    /// The mixer's playing flag for the sound, false with none.
    fn playing(&self) -> bool {
        match self {
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.playing(),
            _ => false,
        }
    }

    /// Which of the sound producer's waits it is parked in, for the watchdog.
    fn producer_wait(&self) -> &'static str {
        match self {
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.producer_wait(),
            _ => "n/a",
        }
    }

    /// The mixer's held frame count for the sound: `Some` while it plays is the F4 latch that freezes the clock.
    fn held_frames(&self) -> Option<u64> {
        match self {
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.held_frames(),
            _ => None,
        }
    }

    /// Recovery: close the sound feed and open another over the same MediaSource, then play from `pts_us`. While there
    /// is no feed the sound reports no position and the clock runs on the system clock, which is Firefox's rule for a missing
    /// sink; the new feed re-resolves the audio track, so a reader whose (buffer, track id) snapshot went stale is put back on
    /// the data. False when no feed could be opened, which leaves the sound silent and the clock on the system clock.
    fn re_open(&mut self, pts_us: i64, paused: bool) -> bool {
        let mut done = false;
        #[cfg(feature = "mse")]
        {
            let shared = match self {
                Audio::Mse(s) => s.shared_ref().clone(),
                _ => return false,
            };
            // Dropping the old sound stops its producer and releases the mixer stream it owned.
            *self = Audio::Silent;
            done = match crate::mse_play::MseSound::open(shared) {
                Some(s) => {
                    *self = Audio::Mse(s);
                    // The state the player is in, said to the new feed before the start: a fresh producer starts paused, and a
                    // start with the wrong copy of that is the stall again.
                    self.set_paused(paused);
                    self.start(pts_us);
                    true
                },
                None => false,
            };
        }
        let _ = (pts_us, paused);
        done
    }

    /// The wall-clock fallback (`LUCINANET_MEDIA_WALLCLOCK_FALLBACK`): the sound reports no position at all, so the clock runs on
    /// the system clock. Off by default; neither Firefox nor Chromium does this once an audio sink exists.
    fn set_wall_only(&self, on: bool) {
        match self {
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.set_wall_only(on),
            _ => {},
        }
    }

    /// The end of all buffered audio, `None` with no MSE sound.
    fn buffered_end_us(&self) -> Option<i64> {
        match self {
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.buffered_end_us(),
            _ => None,
        }
    }
}

impl AudioOutput for Audio {
    fn position_us(&self) -> Option<i64> {
        match self {
            // Once the sound has played to its end the clock runs on from the wall: pictures may go on after it (a last
            // frame at 4.04 s after sound that ends at 4.00 s would otherwise wait for ever).
            Audio::Sound(s) => if s.drained() { None } else { s.position_us() },
            #[cfg(feature = "mse")]
            Audio::Mse(s) => if s.drained() { None } else { s.position_us() },
            Audio::Silent => None,
        }
    }
    fn start(&mut self, pts_us: i64) {
        match self {
            Audio::Sound(s) => s.start(pts_us),
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.start(pts_us),
            Audio::Silent => {},
        }
    }
    fn set_paused(&mut self, paused: bool) {
        match self {
            Audio::Sound(s) => s.set_paused(paused),
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.set_paused(paused),
            Audio::Silent => {},
        }
    }
    fn seek(&mut self, pts_us: i64) {
        match self {
            Audio::Sound(s) => s.seek(pts_us),
            #[cfg(feature = "mse")]
            Audio::Mse(s) => s.seek(pts_us),
            Audio::Silent => {},
        }
    }
}

#[derive(Default)]
struct Counts {
    shown: u64,
    dropped: u64,
    jumps: u64,
    sound_frames: u64,
    late_us_sum: i64,
    worst_late_us: i64,
}

struct Presenter {
    id: usize,
    shared: Arc<Shared>,
    observer: GenericCallback<PlayerEvent>,
    renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
    clock: Clock<Audio>,
    worker: Option<Worker>,
    reorder: usize,
    frames: Frames,
    /// Pictures to WebRender as YUV planes, where the painter takes them (`surfaces`); else `frames` converts to BGRA.
    surface: Option<crate::surfaces::Surface>,
    counts: Counts,
    has_video: bool,
    started: Instant,
    title: String,
    /// The I420 dump and its `.pts` sidecar, one line per picture shown: `<index> <pts_ms>`.
    dump: Option<(std::io::BufWriter<std::fs::File>, std::io::BufWriter<std::fs::File>)>,
}

impl Presenter {
    fn send(&self, e: PlayerEvent) {
        let _ = self.observer.send(e);
    }

    fn run(
        id: usize,
        shared: Arc<Shared>,
        source: Source,
        observer: GenericCallback<PlayerEvent>,
        renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        options: Options,
    ) {
        let started = Instant::now();
        // Script's first fetch is waiting for this.
        source.start();
        let opened = open(&source, &shared);
        trace!("opened: {}", if opened.is_some() { "yes" } else { "no" });
        #[cfg(feature = "mse")]
        if shared.lock().mse_attached {
            // `mse_attach` closed the byte source, which ended `open`: the media comes from the MediaSource.
            let Some((demux, sound, metadata)) = open_mse(&shared) else { return };
            return Presenter::start(id, shared, observer, renderer, options, started, demux, sound, metadata);
        }
        let Some((demux, sound, metadata)) = opened else {
            if !shared.lock().closed {
                let _ = observer.send(PlayerEvent::Error("lucinanet-media: not a file this can play".into()));
            }
            return;
        };
        let sound = sound.map(|d| Audio::Sound(Sound::new(d))).unwrap_or(Audio::Silent);
        Presenter::start(id, shared, observer, renderer, options, started, demux, sound, metadata);
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        id: usize,
        shared: Arc<Shared>,
        observer: GenericCallback<PlayerEvent>,
        renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        options: Options,
        started: Instant,
        demux: Option<Box<dyn lucina_video::Demuxer>>,
        audio: Audio,
        metadata: Metadata,
    ) {
        let has_video = demux.is_some();
        let mut reorder = 0;
        let worker = match demux {
            Some(d) => {
                let codec = d.track().codec;
                reorder = if codec == Codec::H264 { H264_REORDER } else { 0 };
                // At nice 0, not the Video app's 10: Servo's threads run at 0, and a decoder at 10 got a tenth of a contended CPU on
                // the laptop (43 ms a 1080p frame on Venus against the Video app's 7, 30-44% dropped). `LUCINANET_MEDIA_DECODE_NICE`
                // sets another, for measuring.
                let nice = std::env::var("LUCINANET_MEDIA_DECODE_NICE").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
                match Worker::start_with_nice(d, codec, reorder, shared.wake.write, nice) {
                    Ok(w) => Some(w),
                    Err(e) => {
                        let _ = observer.send(PlayerEvent::Error(format!("lucinanet-media: {e}")));
                        return;
                    },
                }
            },
            None => None,
        };
        let title = metadata.format.clone();
        shared.lock().metadata = Some(metadata.clone());
        let _ = observer.send(PlayerEvent::MetadataUpdated(metadata));
        let mut p = Presenter {
            id,
            shared,
            observer,
            renderer,
            clock: Clock::new(audio),
            worker,
            reorder,
            frames: Frames::default(),
            surface: if has_video { crate::surfaces::Surface::new() } else { None },
            counts: Counts::default(),
            has_video,
            started,
            title,
            dump: options.dump_yuv.and_then(|p| {
                let pts = std::fs::File::create(p.with_extension("pts")).ok()?;
                Some((std::io::BufWriter::new(std::fs::File::create(p).ok()?), std::io::BufWriter::new(pts)))
            }),
        };
        p.apply_gain();
        p.clock.set_paused(true, Instant::now());
        p.play();
        p.report();
    }

    fn apply_gain(&mut self) {
        let c = self.shared.lock();
        let gain = if c.muted { 0.0 } else { c.volume as f32 };
        drop(c);
        self.clock.audio.set_gain(gain);
    }

    fn closed(&self) -> bool {
        self.shared.lock().closed
    }

    fn set_playing_state(&mut self, playing: bool, finished: bool) {
        // Pictures playing with sound keep the screen on (juno-power's browser rule); a muted or silent video does not.
        let c = self.shared.lock();
        let audible = !c.muted && c.volume > 0.0;
        drop(c);
        let has_sound = !matches!(self.clock.audio, Audio::Silent);
        hold::set(self.id, self.has_video && has_sound && audible && playing && !finished);
    }

    /// The first picture is up (or there is none): `StateChanged(Paused)`, as GStreamer's preroll.
    fn play(&mut self) {
        let mut prerolled = false;
        let mut finished = false;
        let mut seeking: Option<i64> = None;
        // frozen-play, the pause that arrives with a seek. Its sender is `HTMLMediaElement::update_media_state`, which calls
        // `player.pause()` whenever it finds the pipeline playing — including when the page's own `play()` is what it is answering
        // — and on the `currentTime = t` then `play()` pattern every MSE page uses, that lands while the seek is still in flight.
        // The element is potentially playing and will not call `play()` again, because from its side nothing paused. Remember that
        // this seek was asked for while playing, so that the landing can put the playing state back.
        let mut resume_after_seek = false;
        let mut last_position = Instant::now() - POSITION_EVERY;
        let mut last_shown = Instant::now();
        let mut last_pts: Option<i64> = None;
        let mut frame_us: i64 = 40_000;
        let mut paused = true;
        // frozen-play watchdog. The clock, driven by the audio position, can freeze on a `Some` value that never advances when the sound
        // feed stops while audio is still buffered: the mixer stream left not-playing (F-b), the producer parked on stale ids (P1),
        // or a cursor that never reads the frames in front of it (F-a). Nothing else in that chain times out, so watch for the
        // clock standing still here, where both the clock and the audio buffer are visible.
        let mut stuck_at: Option<i64> = None;
        let mut stuck_since = Instant::now();
        let mut watchdog_reported = false;
        let mut waiting_reported = false;
        let mut reopened = false;
        let mut fell_back = false;
        let mut clock_from_sound: Option<bool> = None;
        if !self.has_video {
            // Sound alone: time runs from the start, paused until play.
            self.clock.start_at(0, Instant::now());
            self.send(PlayerEvent::StateChanged(PlaybackState::Paused));
            prerolled = true;
        }
        loop {
            if self.closed() {
                return;
            }
            // Commands.
            let commands: Vec<Cmd> = self.shared.lock().commands.drain(..).collect();
            for c in commands {
                let now = Instant::now();
                match c {
                    Cmd::Play => {
                        if finished {
                            // Script seeks back first when it wants a replay; a play at the end changes nothing.
                        }
                        self.clock.set_paused(false, now);
                        // say it to the sound too, whatever the clock made of it. `Clock::set_paused` returns without
                        // touching the audio when the request matches its own mirror, and a seek stops the sound without
                        // pausing the clock — so a resume that arrives with the clock already unpaused would never reach the
                        // mixer, leaving the stream not-playing with its position latched and the ring full. One source of the
                        // truth: the player's `paused`, applied to both. `set_paused` is idempotent at the mixer.
                        self.clock.audio.set_paused(false);
                        if std::mem::replace(&mut paused, false) {
                            self.send(PlayerEvent::StateChanged(PlaybackState::Playing));
                        }
                    },
                    Cmd::Pause => {
                        // A pause that arrives with a seek in flight, for a seek asked for while playing, is not the page pausing:
                        // it is `update_media_state` (§1c of the run report) answering the page's own `play()`. The page sees no
                        // `pause` event and never calls `play()` again, so letting this pause outlive the seek is what leaves the
                        // sound stopped with the whole clip buffered. Hold the clock for the seek and stay quiet about it; the
                        // landing puts the state back.
                        let plumbing = resume_after_seek && seeking.is_some();
                        self.clock.set_paused(true, now);
                        self.clock.audio.set_paused(true);
                        if !std::mem::replace(&mut paused, true) && !plumbing {
                            self.send(PlayerEvent::StateChanged(PlaybackState::Paused));
                        }
                    },
                    Cmd::Gain => self.apply_gain(),
                    Cmd::Seek(t) => {
                        let target = (t * 1_000_000.0) as i64;
                        finished = false;
                        // Whether this seek has to give the playing state back when it lands.
                        resume_after_seek = !paused;
                        self.clock.stop(target);
                        match &self.worker {
                            Some(w) => {
                                w.command(Command::Seek { pts: target, forward: false, from: 0 });
                                seeking = Some(target);
                            },
                            None => {
                                resume_after_seek = false;
                                self.clock.start_at(target, now);
                                self.send(PlayerEvent::SeekDone(t));
                                self.send(PlayerEvent::PositionChanged(t));
                            },
                        }
                    },
                }
            }
            self.set_playing_state(!paused, finished);
            let now = Instant::now();
            let clock_now = self.clock.now_us(now);
            if !paused && !finished && seeking.is_none() && last_position.elapsed() >= POSITION_EVERY {
                last_position = now;
                if let Some(n) = clock_now {
                    // Never past the duration, as GStreamer's positions: the wall clock runs on a little after the sound ends.
                    let d = self.shared.lock().metadata.as_ref().and_then(|m| m.duration).map(|d| d.as_secs_f64());
                    let p = n as f64 / 1_000_000.0;
                    self.send(PlayerEvent::PositionChanged(d.map_or(p, |d| p.min(d))));
                }
            }
            // The clock-stall watchdog: `now_us` is the audio position while the feed works, so a `Some` that does not change for
            // 3 s with audio still buffered is the stall, not merely a pause or the end.
            let from_sound = self.clock.follows_sound();
            if prerolled && !paused && clock_from_sound != Some(from_sound) {
                if let Some(was) = clock_from_sound {
                    // Firefox logs the master switch; so do we, both ways. A re-open goes through the system clock for the time
                    // its feed takes to start reporting a position again.
                    log::warn!(
                        "lucinanet-media: clock source: {} -> {} at {:.3} s",
                        if was { "audio" } else { "system" },
                        if from_sound { "audio" } else { "system" },
                        clock_now.map_or(0.0, |n| n as f64 / 1_000_000.0)
                    );
                }
                clock_from_sound = Some(from_sound);
            }
            if prerolled && !paused && !finished && seeking.is_none() && from_sound {
                match clock_now {
                    Some(v) => {
                        if stuck_at != Some(v) {
                            stuck_at = Some(v);
                            stuck_since = now;
                            watchdog_reported = false;
                            waiting_reported = false;
                        } else if now.duration_since(stuck_since) >= Duration::from_secs(3)
                            && self.clock.audio.buffered_end_us().is_some_and(|e| v < e)
                        {
                            if !watchdog_reported {
                                watchdog_reported = true;
                                log::warn!(
                                    "lucinanet-media: clock stood still {} s with audio buffered (queued={}, playing={}, held={:?}, producer={})",
                                    stuck_since.elapsed().as_secs(),
                                    self.clock.audio.queued(),
                                    self.clock.audio.playing(),
                                    self.clock.audio.held_frames(),
                                    self.clock.audio.producer_wait()
                                );
                                if crate::recovery() && !reopened {
                                    reopened = true;
                                    stuck_since = now;
                                    log::warn!(
                                        "lucinanet-media: recovery: re-opening the audio feed at {:.3} s",
                                        v as f64 / 1_000_000.0
                                    );
                                    if !self.clock.audio.re_open(v, paused) {
                                        log::warn!("lucinanet-media: recovery: no audio feed could be opened; the picture is left to the system clock");
                                    }
                                }
                            } else if reopened && !waiting_reported {
                                waiting_reported = true;
                                if crate::wallclock_fallback() && !fell_back {
                                    fell_back = true;
                                    self.clock.audio.set_wall_only(true);
                                    log::warn!(
                                        "lucinanet-media: fallback: the audio still stands after the re-open; the clock is handed to the system clock (LUCINANET_MEDIA_WALLCLOCK_FALLBACK=1)"
                                    );
                                } else {
                                    // Chromium's and Firefox's answer to a feed that will not come back: the clock stops and the
                                    // element waits for data. The picture does not run ahead of it.
                                    log::warn!(
                                        "lucinanet-media: the audio still stands {} s after the re-open; waiting for data at {:.3} s (queued={}, playing={}, held={:?}, producer={})",
                                        stuck_since.elapsed().as_secs(),
                                        v as f64 / 1_000_000.0,
                                        self.clock.audio.queued(),
                                        self.clock.audio.playing(),
                                        self.clock.audio.held_frames(),
                                        self.clock.audio.producer_wait()
                                    );
                                }
                            }
                        }
                    },
                    None => stuck_at = None,
                }
            } else {
                stuck_at = None;
                watchdog_reported = false;
                waiting_reported = false;
            }
            let Some(worker) = self.worker.as_ref() else {
                // Sound alone.
                if !finished && self.clock.audio.drained() {
                    finished = true;
                    self.send(PlayerEvent::EndOfStream);
                }
                self.shared.wake.wait(Duration::from_millis(50));
                continue;
            };
            // A seek: wait for it to land, then for the first picture at or after the time asked.
            if let Some(target) = seeking {
                let mut w = worker.lock();
                if let Some(e) = w.error.take() {
                    drop(w);
                    self.send(PlayerEvent::Error(format!("lucinanet-media: {e}")));
                    return;
                }
                if w.landed.is_none() {
                    drop(w);
                    self.shared.wake.wait(Duration::from_millis(20));
                    continue;
                }
                // Pictures before the time asked are decoded only to reach it.
                while w.queue.len() > self.reorder && w.queue.first_pts().is_some_and(|p| p + frame_us / 2 < target) {
                    w.queue.pop();
                }
                let ready = w.queue.len() > self.reorder || (w.eof && w.queue.len() > 0);
                if !ready && !w.eof {
                    w.starving = true;
                    drop(w);
                    worker.wake();
                    self.shared.wake.wait(Duration::from_millis(20));
                    continue;
                }
                let pic = w.queue.pop();
                drop(w);
                worker.wake();
                if let Some(pic) = pic {
                    self.show(&pic);
                }
                // give back the playing state the seek was asked for. The pause that arrived with the seek held us for its
                // duration and owes nothing more, and the element — which never paused, and was told nothing of that pause — will
                // not call `play()`. Nothing is announced here for the same reason. The resume reaches the clock and the sound
                // before `start_at`, so that the `Cmd::Paused(false)` gets to the producer ahead of its `Cmd::Start`: the start
                // re-arms the mixer queue from the `paused` it holds, and a stale `true` there is what left the stream not-playing
                // with a full ring and the clock frozen at the seek.
                if std::mem::replace(&mut resume_after_seek, false) && paused {
                    self.clock.set_paused(false, Instant::now());
                    self.clock.audio.set_paused(false);
                    paused = false;
                    // Not the `Control.paused` mirror, on purpose, and the reason is the fork bug rather than carelessness: while
                    // `update_media_state` pauses a player it finds playing, a mirror that still says *paused* makes that same
                    // call re-`play()` us, which is the one path that revives a feed the element stopped by accident. Once the
                    // fork line lands the mirror should be written here too; until then `Player::paused()` can report
                    // the pause that this seek already undid, and the run report says so.
                }
                self.clock.start_at(target, Instant::now());
                seeking = None;
                let t = target as f64 / 1_000_000.0;
                self.send(PlayerEvent::SeekDone(t));
                self.send(PlayerEvent::PositionChanged(t));
                last_shown = Instant::now();
                continue;
            }
            let (queued, first_pts, eof, error) = {
                let mut w = worker.lock();
                let behind = pace::behind_us(w.queue.first_pts(), clock_now, last_pts, frame_us);
                if behind > pace::SKIP_AFTER_US {
                    w.skip_nonref = true;
                } else if behind < pace::SKIP_UNTIL_US {
                    w.skip_nonref = false;
                }
                (w.queue.len(), w.queue.first_pts(), w.eof, w.error.take())
            };
            if let Some(e) = error {
                self.send(PlayerEvent::Error(format!("lucinanet-media: {e}")));
                return;
            }
            let state = pace::State {
                queued,
                first_pts,
                // Before the first picture is up, it goes up whatever its time (GStreamer's preroll): the clock is held at 0 until
                // then, and a first frame at 7 ms would otherwise wait for a clock that is not running.
                now: if prerolled { clock_now } else { None },
                eof,
                reorder: self.reorder,
                frame_us,
                paused: paused && prerolled,
                since_shown_us: last_shown.elapsed().as_micros() as i64,
            };
            match pace::next(&state) {
                Step::Starving => {
                    trace!("starving queued={queued} eof={eof} now={clock_now:?}");
                    worker.lock().starving = true;
                    worker.wake();
                    self.shared.wake.wait(Duration::from_millis(20));
                },
                Step::SkipToKey => {
                    self.counts.jumps += 1;
                    self.clock.stop(clock_now.unwrap_or(0));
                    worker.command(Command::ToKey);
                },
                Step::Drop => {
                    trace!("drop pts={first_pts:?} now={clock_now:?} sound={}", self.clock.follows_sound());
                    worker.lock().queue.pop();
                    worker.wake();
                    self.counts.dropped += 1;
                    self.shared.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                },
                Step::Show => {
                    let Some(pic) = worker.lock().queue.pop() else { continue };
                    worker.wake();
                    let pts = pic.pts_us();
                    trace!("show pts={pts} now={clock_now:?} sound={:?}", self.clock.audio.position_us());
                    if clock_now.is_some() && self.clock.follows_sound() {
                        self.counts.sound_frames += 1;
                    }
                    if let Some(n) = clock_now {
                        let late = n - pts;
                        self.counts.late_us_sum += late.max(0);
                        self.counts.worst_late_us = self.counts.worst_late_us.max(late);
                    }
                    self.show(&pic);
                    let t = Instant::now();
                    last_shown = t;
                    if let Some(d) = last_pts.map(|l: i64| pts - l).filter(|d| (1_000..1_000_000).contains(d)) {
                        frame_us = d;
                    }
                    last_pts = Some(pts);
                    if !self.clock.is_started() {
                        self.clock.start_at(pts, t);
                    }
                    if !prerolled {
                        prerolled = true;
                        log::info!("lucinanet-media: first frame after {} ms", self.started.elapsed().as_millis());
                        self.send(PlayerEvent::StateChanged(PlaybackState::Paused));
                    }
                },
                Step::Wait(d) => self.shared.wake.wait(d.min(POSITION_EVERY)),
                Step::Finished => {
                    let heard = self.clock.audio.drained() || paused;
                    if !finished && heard {
                        finished = true;
                        if !prerolled {
                            prerolled = true;
                            self.send(PlayerEvent::StateChanged(PlaybackState::Paused));
                        }
                        self.set_playing_state(false, true);
                        // The stream's last picture is up, so the dump is finished: whole by `EndOfStream`, whatever the process
                        // does next.
                        self.flush_dump();
                        self.report();
                        self.send(PlayerEvent::EndOfStream);
                    }
                    self.shared.wake.wait(Duration::from_millis(50));
                },
            }
        }
    }

    fn show(&mut self, pic: &lucina_video_play::decode::Picture) {
        let frame = match self.surface.as_mut() {
            Some(s) => s.publish(pic),
            None => self.frames.convert(pic),
        };
        let Some(frame) = frame else {
            self.send(PlayerEvent::Error("lucinanet-media: video above 8 bits is not playable here".into()));
            return;
        };
        self.counts.shown += 1;
        self.shared.shown.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some((d, pts)) = self.dump.as_mut() {
            use std::io::Write;
            let _ = crate::frames::write_i420(pic, d);
            // The picture's place in the file (`counts.shown` was already bumped for it) and its presentation time in
            // milliseconds, the same clock the frames and `Step::Show` pace by.
            let _ = writeln!(pts, "{} {:.3}", self.counts.shown - 1, pic.pts_us() as f64 / 1000.0);
        }
        // Flushed every picture, not only when the Presenter drops: `--render` leaves through `lucinanet_ui::exit_now`, which
        // calls `libc::_exit`, so no destructor runs and the writer still holds the tail (seen on macOS: the last of 96 frames
        // short by 320 bytes, and the `.pts` empty). It is a diagnostic, so the cost of the write is not the concern.
        self.flush_dump();
        if let Some(r) = &self.renderer {
            r.lock().unwrap_or_else(|e| e.into_inner()).render(frame);
        }
        self.send(PlayerEvent::VideoFrameUpdated);
    }

    /// Both dump writers empty on disk, errors ignored: nothing here changes what plays.
    fn flush_dump(&mut self) {
        use std::io::Write;
        if let Some((d, pts)) = self.dump.as_mut() {
            let _ = d.flush();
            let _ = pts.flush();
        }
    }

    /// The Video app's `played` line, which the laptop tests read.
    fn report(&self) {
        let (decoded, on_venus, skipped, decode_ms, worst) = match &self.worker {
            Some(w) => {
                let w = w.lock();
                let c = &w.counts;
                (c.decoded, c.on_venus, c.skipped, c.decode_ms, c.worst_decode_ms)
            },
            None => (0, 0, 0, 0.0, 0.0),
        };
        let c = &self.counts;
        let mean_late = if c.shown > 0 { c.late_us_sum as f64 / c.shown as f64 / 1000.0 } else { 0.0 };
        eprintln!(
            "lucinanet-media: played {} seconds={:.1} shown={} dropped={} jumps={} decoded={} on_venus={} skipped={} decode_ms_mean={:.1} \
             decode_ms_worst={:.1} sound_frames={} late_ms_mean={:.1} late_ms_worst={:.1}",
            self.title,
            self.started.elapsed().as_secs_f64(),
            c.shown,
            c.dropped,
            c.jumps,
            decoded,
            on_venus,
            skipped,
            if decoded > 0 { decode_ms / decoded as f64 } else { 0.0 },
            worst,
            c.sound_frames,
            mean_late,
            c.worst_late_us as f64 / 1000.0,
        );
    }
}

/// Reads what the resource is. Pictures through `lucina_video`'s demuxers, sound through symphonia's, each its own reader.
#[allow(clippy::type_complexity)]
fn open(source: &Source, shared: &Shared) -> Option<(Option<Box<dyn lucina_video::Demuxer>>, Option<SoundDecoder>, Metadata)> {
    let video = match lucina_video::demux::open(BufReader::with_capacity(64 << 10, source.reader())) {
        Ok(d) => Some(d),
        Err(e) => {
            if shared.lock().closed {
                return None;
            }
            log::info!("lucinanet-media: no pictures: {e}");
            None
        },
    };
    let sound = match SoundDecoder::from_reader(BufReader::with_capacity(64 << 10, source.reader())) {
        Ok(d) => Some(d),
        Err(e) => {
            if shared.lock().closed {
                return None;
            }
            log::info!("lucinanet-media: no sound: {e}");
            None
        },
    };
    if video.is_none() && sound.is_none() {
        return None;
    }
    let (width, height, codec, video_us) = match &video {
        Some(d) => {
            let t = d.track();
            (t.width, t.height, Some(t.codec.name()), t.duration_us)
        },
        None => (0, 0, None, None),
    };
    let sound_us = sound.as_ref().and_then(|s| s.duration_ms()).map(|ms| ms as i64 * 1000).or_else(|| {
        // symphonia gives no length for Ogg; browsers read the last page's granule position, and so does this.
        let rate = sound.as_ref()?.rate();
        ogg_duration_us(&mut source.reader(), rate)
    });
    let duration = video_us.or(sound_us).filter(|d| *d > 0).map(|us| Duration::from_micros(us as u64));
    let audio_name = video.as_ref().and_then(|d| d.track().audio.clone()).unwrap_or_else(|| String::from("audio"));
    let metadata = Metadata {
        duration,
        width,
        height,
        format: codec.map_or_else(|| audio_name.clone(), str::to_owned),
        is_seekable: source.seekable() && source.size().is_some(),
        video_tracks: codec.map(|c| vec![c.to_owned()]).unwrap_or_default(),
        audio_tracks: if sound.is_some() { vec![audio_name] } else { Vec::new() },
        is_live: source.size().is_none(),
        title: None,
    };
    Some((video, sound, metadata))
}

#[cfg(feature = "mse")]
fn secs_to_us(s: f64) -> i64 {
    if s.is_finite() { (s * 1_000_000.0).round().clamp(i64::MIN as f64, i64::MAX as f64) as i64 } else { 0 }
}

#[cfg(feature = "mse")]
fn us_to_secs(us: i64) -> f64 {
    us as f64 / 1_000_000.0
}

#[cfg(feature = "mse")]
fn to_secs(r: &[(i64, i64)]) -> Vec<Range<f64>> {
    r.iter().map(|(a, b)| us_to_secs(*a)..us_to_secs(*b)).collect()
}

/// Waits for every SourceBuffer's first initialization segment, then plays from the track buffers.
#[cfg(feature = "mse")]
#[allow(clippy::type_complexity)]
fn open_mse(shared: &Shared) -> Option<(Option<Box<dyn lucina_video::Demuxer>>, Audio, Metadata)> {
    use lucina_video::Demuxer as _;

    use crate::mse_play::{MseDemux, MseSound};
    let mse = shared.mse.clone();
    loop {
        if shared.lock().closed || mse.lock().closed {
            return None;
        }
        if mse.all_initialized() {
            break;
        }
        let s = mse.lock();
        drop(mse.cv.wait_timeout(s, Duration::from_millis(250)).unwrap_or_else(|e| e.into_inner()));
    }
    let demux = MseDemux::open(mse.clone()).filter(|d| crate::mse_play::video_codec_supported(d.track().codec));
    let sound = MseSound::open(mse.clone());
    let (width, height, codec) = match &demux {
        Some(d) => (d.track().width, d.track().height, Some(d.track().codec.name())),
        None => (0, 0, None),
    };
    let duration_us = mse.lock().duration_us;
    let metadata = Metadata {
        duration: duration_us.filter(|d| *d != i64::MAX && *d >= 0).map(|d| Duration::from_micros(d as u64)),
        width,
        height,
        format: codec.map_or_else(|| String::from("audio"), str::to_owned),
        is_seekable: duration_us != Some(i64::MAX),
        video_tracks: codec.map(|c| vec![c.to_owned()]).unwrap_or_default(),
        audio_tracks: if sound.is_some() { vec![String::from("audio")] } else { Vec::new() },
        is_live: duration_us == Some(i64::MAX),
        title: None,
    };
    let audio = sound.map(Audio::Mse).unwrap_or(Audio::Silent);
    Some((demux.map(|d| Box::new(d) as Box<dyn lucina_video::Demuxer>), audio, metadata))
}

/// An Ogg stream's length from its last page: the granule position at 48 kHz for Opus (less its pre-skip), at the stream's rate for
/// Vorbis and FLAC (RFC 3533, RFC 7845). `None` when it is not Ogg or the end cannot be read.
fn ogg_duration_us(r: &mut crate::source::SourceReader, rate: u32) -> Option<i64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut head = [0u8; 64];
    r.seek(SeekFrom::Start(0)).ok()?;
    r.read_exact(&mut head[..36]).ok()?;
    if &head[..4] != b"OggS" {
        return None;
    }
    // The first packet says which codec: "OpusHead" (pre-skip at 10, rate always 48 kHz) or another.
    let first = &head[28..36];
    let (rate, pre_skip) = if first == b"OpusHead" {
        let mut more = [0u8; 4];
        r.read_exact(&mut more).ok()?;
        (48_000u64, u16::from_le_bytes([more[2], more[3]]) as u64)
    } else {
        (u64::from(rate.max(1)), 0)
    };
    let size = r.source().size()?;
    let from = size.saturating_sub(65_536);
    r.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    r.take(65_536).read_to_end(&mut tail).ok()?;
    let at = tail.windows(4).rposition(|w| w == b"OggS")?;
    let page = tail.get(at..at + 14)?;
    let granule = u64::from_le_bytes(page[6..14].try_into().ok()?);
    if granule == u64::MAX {
        return None;
    }
    Some((granule.saturating_sub(pre_skip) as i128 * 1_000_000 / rate as i128) as i64)
}
