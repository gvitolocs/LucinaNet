/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The player driven as Servo's `htmlmediaelement.rs` drives it, without Servo: a "script" thread answers the player's events as
//! script does (unlock on `NeedData`, cancel a seekable fetch on `EnoughData`, a range fetch and `SeekLock::unlock` on `SeekData`,
//! `play()` once paused with metadata), and a "network" thread pushes the file's bytes from the fetch's offset.
//!
//! Every picture shown is compared with FFmpeg's, frame by frame (`tests/media/*.framemd5`, from
//! `ffmpeg -i <clip> -an -f framemd5 -pix_fmt yuv420p`): the decoders are exact, so the frames must be too, in display order, none
//! missing and none repeated.
//!
//! Clips (`tests/media/`): synthetic, made on the build host with FFmpeg's `testsrc2` and `sine` (no one's footage):
//!   h264-aac.mp4             640x360 24 fps 4 s, x264 high, AAC 48 kHz, moov first
//!   h264-aac-moov-at-end.mp4 the same, moov last (the player must fetch the end first and come back)
//!   vp9-vorbis.webm          640x360 24 fps 4 s, libvpx-vp9, Vorbis 44.1 kHz
//!   tone.mp3                 3 s, 550 Hz, 44.1 kHz

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lucinanet_media::player::{LucinaPlayer, Options};
use md5::{Digest, Md5};
use servo_base::generic_channel::GenericCallback;
use servo_media_player::video::{VideoFrame, VideoFrameRenderer};
use servo_media_player::{PlaybackState, Player, PlayerError, PlayerEvent, StreamType};

fn clip(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media").join(name)
}

#[derive(Default)]
struct Shown(Mutex<Vec<(Instant, i32, i32)>>);

struct Renderer(Arc<Shown>);

impl VideoFrameRenderer for Renderer {
    fn render(&mut self, frame: VideoFrame) {
        self.0.0.lock().unwrap().push((Instant::now(), frame.get_width(), frame.get_height()));
    }
}

/// The fetch as script runs it.
struct Fetch {
    offset: u64,
    /// The data source stays locked until `NeedData` (htmlmediaelement.rs `DataSource::locked`).
    locked: bool,
    /// Cancelled at `EnoughData` ("Backoff").
    cancelled: bool,
    ended: bool,
    closed: bool,
}

#[derive(Debug, Clone)]
enum Seen {
    Metadata(servo_media_player::metadata::Metadata),
    State(String),
    Position(f64),
    SeekData(u64),
    SeekDone(f64),
    Need,
    Enough,
    Frame,
    End,
    Error(String),
}

struct Run {
    player: Arc<Mutex<LucinaPlayer>>,
    seen: Arc<Mutex<Vec<(Instant, Seen)>>>,
    shown: Arc<Shown>,
    fetch: Arc<(Mutex<Fetch>, Condvar)>,
    start: Instant,
    /// Backoff then NeedData: Servo would answer with a seek to the playback position, which this backend must never need.
    needless_seek: Arc<Mutex<bool>>,
}

impl Run {
    fn new(file: &Path, autoplay: bool, dump: Option<PathBuf>) -> Run {
        let data = Arc::new(std::fs::read(file).unwrap());
        let (tx, rx) = mpsc::channel::<PlayerEvent>();
        let tx = Mutex::new(tx);
        let callback = GenericCallback::new(move |e: Result<PlayerEvent, _>| {
            let _ = tx.lock().unwrap().send(e.unwrap());
        })
        .unwrap();
        let shown = Arc::new(Shown::default());
        let renderer: Arc<Mutex<dyn VideoFrameRenderer>> = Arc::new(Mutex::new(Renderer(shown.clone())));
        let player = Arc::new(Mutex::new(LucinaPlayer::new(
            0,
            StreamType::Seekable,
            callback,
            Some(renderer),
            Options { dump_yuv: dump },
        )));
        // Response headers: Servo sets these before the first chunk.
        player.lock().unwrap().set_seekable(true).unwrap();
        player.lock().unwrap().set_input_size(data.len() as u64).unwrap();
        let fetch = Arc::new((Mutex::new(Fetch { offset: 0, locked: true, cancelled: false, ended: false, closed: false }), Condvar::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let needless_seek = Arc::new(Mutex::new(false));
        let run = Run { player, seen, shown, fetch, start: Instant::now(), needless_seek };
        run.network(data);
        run.script(rx, autoplay);
        run
    }

    fn network(&self, data: Arc<Vec<u8>>) {
        let (fetch, player) = (self.fetch.clone(), self.player.clone());
        std::thread::spawn(move || {
            loop {
                let (m, cv) = &*fetch;
                let mut f = m.lock().unwrap();
                while !f.closed && (f.locked || f.cancelled || f.ended) {
                    f = cv.wait_timeout(f, Duration::from_millis(50)).unwrap().0;
                }
                if f.closed {
                    return;
                }
                let at = f.offset as usize;
                if at >= data.len() {
                    f.ended = true;
                    drop(f);
                    player.lock().unwrap().end_of_stream().unwrap();
                    continue;
                }
                let end = (at + 32 * 1024).min(data.len());
                let chunk = data[at..end].to_vec();
                drop(f);
                let r = player.lock().unwrap().push_data(chunk);
                let mut f = m.lock().unwrap();
                // A fetch restarted meanwhile: this chunk belonged to the old one.
                if f.offset as usize != at {
                    continue;
                }
                match r {
                    Ok(()) => f.offset = end as u64,
                    Err(PlayerError::EnoughData) => f.cancelled = true,
                    Err(e) => panic!("push_data: {e:?}"),
                }
                drop(f);
                // About 80 Mbit/s.
                std::thread::sleep(Duration::from_micros(400));
            }
        });
    }

    fn script(&self, rx: Receiver<PlayerEvent>, autoplay: bool) {
        let (fetch, player, seen, needless) = (self.fetch.clone(), self.player.clone(), self.seen.clone(), self.needless_seek.clone());
        std::thread::spawn(move || {
            let mut played = false;
            let mut have_metadata = false;
            while let Ok(e) = rx.recv() {
                let now = Instant::now();
                let (m, cv) = &*fetch;
                let s = match e {
                    PlayerEvent::NeedData => {
                        let mut f = m.lock().unwrap();
                        if f.cancelled {
                            *needless.lock().unwrap() = true;
                        }
                        f.locked = false;
                        cv.notify_all();
                        Seen::Need
                    },
                    PlayerEvent::EnoughData => {
                        m.lock().unwrap().cancelled = true;
                        Seen::Enough
                    },
                    PlayerEvent::SeekData(offset, lock) => {
                        let mut f = m.lock().unwrap();
                        f.offset = offset;
                        f.locked = true;
                        f.cancelled = false;
                        f.ended = false;
                        drop(f);
                        lock.unlock(true);
                        Seen::SeekData(offset)
                    },
                    PlayerEvent::MetadataUpdated(md) => {
                        have_metadata = true;
                        Seen::Metadata(md)
                    },
                    PlayerEvent::StateChanged(st) => {
                        if matches!(st, PlaybackState::Paused) && have_metadata && autoplay && !played {
                            played = true;
                            player.lock().unwrap().play().unwrap();
                        }
                        Seen::State(format!("{st:?}"))
                    },
                    PlayerEvent::PositionChanged(p) => Seen::Position(p),
                    PlayerEvent::SeekDone(t) => Seen::SeekDone(t),
                    PlayerEvent::VideoFrameUpdated => Seen::Frame,
                    PlayerEvent::EndOfStream => Seen::End,
                    PlayerEvent::Error(e) => Seen::Error(e),
                    PlayerEvent::DurationChanged(_) => continue,
                };
                seen.lock().unwrap().push((now, s));
            }
        });
    }

    fn wait_for(&self, limit: Duration, what: impl Fn(&Seen) -> bool) -> Option<Instant> {
        let until = Instant::now() + limit;
        loop {
            if let Some((t, _)) = self.seen.lock().unwrap().iter().find(|(_, s)| what(s)) {
                return Some(*t);
            }
            if Instant::now() > until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().iter().map(|(_, s)| s.clone()).collect()
    }

    fn errors(&self) -> Vec<String> {
        self.seen().into_iter().filter_map(|s| if let Seen::Error(e) = s { Some(e) } else { None }).collect()
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.fetch.0.lock().unwrap().closed = true;
        self.fetch.1.notify_all();
        let _ = self.player.lock().unwrap().stop();
    }
}

/// The dump and its `.pts` sidecar, read with the player still running and never dropped first: `--render` leaves through
/// `lucinanet_ui::exit_now`, which calls `libc::_exit` and runs no destructors, so what the writers still hold is lost (the
/// owner's Mac: 96 frames minus the last 320 bytes, the sidecar empty). By `EndOfStream` the file must be a whole number of
/// frames and the sidecar must have exactly one `<index> <pts_ms>` line for each, the index counting up.
fn read_dump(dump: &Path, frame: usize) -> (Vec<u8>, Vec<f64>) {
    use std::io::BufRead;
    let ours = std::fs::read(dump).unwrap();
    let shown = ours.len() / frame;
    assert_eq!(ours.len(), shown * frame, "the dump is {} bytes, not whole {frame}-byte pictures", ours.len());
    let mut sidecar = std::io::BufReader::new(std::fs::File::open(dump.with_extension("pts")).unwrap());
    // As many lines as there are pictures and no more: the writer is still open, and a line past the last picture would be
    // one being written as this reads.
    let pts: Vec<f64> = (0..shown)
        .map(|i| {
            let mut line = String::new();
            assert_ne!(sidecar.read_line(&mut line).unwrap(), 0, "picture {i} of {shown} has no `.pts` line");
            let mut cols = line.split_whitespace();
            let index: usize = cols.next().unwrap().parse().unwrap();
            let pts_ms: f64 = cols.next().unwrap().parse().unwrap();
            assert_eq!(index, i, "the `.pts` index column is not the picture's place in the dump");
            pts_ms
        })
        .collect();
    let mut extra = String::new();
    assert_eq!(sidecar.read_line(&mut extra).unwrap(), 0, "the `.pts` has more lines than the {shown} pictures in the dump");
    (ours, pts)
}

/// Our frames' MD5s against FFmpeg's, each against the reference frame at its time (`.pts` sidecar, 24 fps clips): what was
/// shown must be exact; what was dropped is counted by the caller.
fn compare_frames(dump: &Path, reference: &Path, w: usize, h: usize) -> (usize, usize, usize) {
    let frame = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
    let (ours, pts) = read_dump(dump, frame);
    let theirs: Vec<String> = std::fs::read_to_string(reference)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap().trim().to_owned())
        .collect();
    let (mut same, mut shown) = (0, 0);
    for (i, f) in ours.chunks(frame).enumerate() {
        shown += 1;
        let at = (pts[i] * 24.0 / 1000.0).round() as usize;
        let sum = format!("{:x}", Md5::digest(f));
        if theirs.get(at) == Some(&sum) {
            same += 1;
        } else {
            eprintln!("frame {i} (pts {} ms, reference {at}): ours {sum}, FFmpeg's {:?}", pts[i], theirs.get(at));
        }
    }
    (same, shown, theirs.len())
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lucinanet-media-{}-{name}", std::process::id()))
}

fn plays_to_the_end_like_ffmpeg(name: &str, framemd5: &str) {
    let dump = tmp(&format!("{name}.yuv"));
    let run = Run::new(&clip(name), true, Some(dump.clone()));
    let end = run.wait_for(Duration::from_secs(30), |s| matches!(s, Seen::End));
    let seen = run.seen();
    assert!(run.errors().is_empty(), "{:?}", run.errors());
    let end = end.unwrap_or_else(|| panic!("no EndOfStream; saw {:?}", &seen[seen.len().saturating_sub(10)..]));
    let Some(Seen::Metadata(md)) = seen.iter().find(|s| matches!(s, Seen::Metadata(_))).cloned() else { panic!("no metadata") };
    assert_eq!((md.width, md.height), (640, 360));
    let d = md.duration.expect("a duration").as_secs_f64();
    assert!((3.9..4.2).contains(&d), "duration {d}");
    assert_eq!(md.audio_tracks.len(), 1, "{md:?}");
    assert!(md.is_seekable);
    // The order GStreamer gives and Servo expects: metadata, a picture, paused, then playing.
    let i_md = seen.iter().position(|s| matches!(s, Seen::Metadata(_))).unwrap();
    let i_frame = seen.iter().position(|s| matches!(s, Seen::Frame)).unwrap();
    let i_paused = seen.iter().position(|s| matches!(s, Seen::State(x) if x == "Paused")).unwrap();
    let i_playing = seen.iter().position(|s| matches!(s, Seen::State(x) if x == "Playing")).unwrap();
    assert!(i_md < i_frame && i_frame < i_paused && i_paused < i_playing, "{:?}", &seen[..i_playing + 1]);
    // Played at its own speed: 4 s of film from the first picture.
    let first = run.shown.0.lock().unwrap()[0].0;
    let took = end.duration_since(first).as_secs_f64();
    assert!((3.6..5.5).contains(&took), "took {took} s");
    let positions: Vec<f64> = seen.iter().filter_map(|s| if let Seen::Position(p) = s { Some(*p) } else { None }).collect();
    assert!(positions.len() >= 20, "{} position updates", positions.len());
    assert!(positions.windows(2).all(|w| w[1] >= w[0] - 0.001), "positions went back: {positions:?}");
    assert!(!*run.needless_seek.lock().unwrap(), "NeedData after Backoff");
    // Read it with the player still running and no wait for its thread: the last picture must already be on disk, because the
    // browser leaves by `exit_now`, which never drops the player.
    let (same, shown, total) = compare_frames(&dump, &clip(framemd5), 640, 360);
    assert_eq!(shown, run.shown.0.lock().unwrap().len(), "one picture in the dump per picture the renderer was given");
    let _ = std::fs::remove_file(&dump);
    let _ = std::fs::remove_file(dump.with_extension("pts"));
    assert_eq!(total, 96);
    assert_eq!(same, shown, "every picture shown equal to FFmpeg's at its time");
    // Dropped only when the CPU fell behind: on the build host that is none or a few, under load.
    assert!(shown >= 88, "only {shown} of 96 shown");
}

#[test]
fn h264_aac_mp4_plays_to_the_end_frame_exact() {
    plays_to_the_end_like_ffmpeg("h264-aac.mp4", "h264-aac.framemd5");
}

#[test]
fn an_mp4_with_its_index_at_the_end_fetches_it_and_comes_back() {
    plays_to_the_end_like_ffmpeg("h264-aac-moov-at-end.mp4", "h264-aac-moov-at-end.framemd5");
}

#[test]
fn vp9_vorbis_webm_plays_to_the_end_frame_exact() {
    plays_to_the_end_like_ffmpeg("vp9-vorbis.webm", "vp9-vorbis.framemd5");
}

#[test]
fn an_mp3_plays_by_itself() {
    let run = Run::new(&clip("tone.mp3"), true, None);
    let playing = run.wait_for(Duration::from_secs(10), |s| matches!(s, Seen::State(x) if x == "Playing")).expect("playing");
    let end = run.wait_for(Duration::from_secs(15), |s| matches!(s, Seen::End)).expect("EndOfStream");
    assert!(run.errors().is_empty(), "{:?}", run.errors());
    let took = end.duration_since(playing).as_secs_f64();
    assert!((2.8..4.5).contains(&took), "3 s of sound took {took} s");
    assert!(run.shown.0.lock().unwrap().is_empty());
    let Some(Seen::Metadata(md)) = run.seen().into_iter().find(|s| matches!(s, Seen::Metadata(_))) else { panic!() };
    assert!(md.video_tracks.is_empty() && md.audio_tracks.len() == 1, "{md:?}");
}

#[test]
fn ogg_vorbis_gets_its_length_from_the_last_page() {
    // symphonia gives no length for Ogg; the element needs one (WPT audio_loop_seek_to_eos seeks to `duration`).
    let run = Run::new(&clip("tone.ogg"), false, None);
    run.wait_for(Duration::from_secs(10), |s| matches!(s, Seen::Metadata(_))).expect("metadata");
    let Some(Seen::Metadata(md)) = run.seen().into_iter().find(|s| matches!(s, Seen::Metadata(_))) else { panic!() };
    let d = md.duration.expect("a length").as_secs_f64();
    assert!((d - 3.0).abs() < 0.05, "{d}");
}

#[test]
fn a_seek_lands_where_asked_and_plays_on() {
    let run = Run::new(&clip("h264-aac.mp4"), true, None);
    run.wait_for(Duration::from_secs(10), |s| matches!(s, Seen::Position(p) if *p > 0.5)).expect("playing");
    let asked = Instant::now();
    run.player.lock().unwrap().seek(3.0).unwrap();
    let done = run.wait_for(Duration::from_secs(10), |s| matches!(s, Seen::SeekDone(_))).expect("SeekDone");
    let Some(Seen::SeekDone(t)) = run.seen().into_iter().find(|s| matches!(s, Seen::SeekDone(_))) else { panic!() };
    assert!((t - 3.0).abs() < 0.01, "landed at {t}");
    assert!(done.duration_since(asked) < Duration::from_secs(2), "seek took {:?}", done.duration_since(asked));
    let end = run.wait_for(Duration::from_secs(10), |s| matches!(s, Seen::End)).expect("EndOfStream");
    let took = end.duration_since(done).as_secs_f64();
    assert!((0.8..2.0).contains(&took), "1 s left took {took} s");
    assert!(run.errors().is_empty(), "{:?}", run.errors());
    // A seek past the end is refused, as GStreamer's.
    assert_eq!(run.player.lock().unwrap().seek(60.0), Err(PlayerError::SeekOutOfRange));
}

#[test]
fn paused_shows_the_first_picture_and_does_not_run() {
    let run = Run::new(&clip("h264-aac.mp4"), false, None);
    run.wait_for(Duration::from_secs(10), |s| matches!(s, Seen::State(x) if x == "Paused")).expect("paused");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(run.shown.0.lock().unwrap().len(), 1, "one picture while paused");
    assert!(!run.seen().iter().any(|s| matches!(s, Seen::Position(_))));
    run.player.lock().unwrap().play().unwrap();
    run.wait_for(Duration::from_secs(5), |s| matches!(s, Seen::Position(p) if *p > 0.3)).expect("runs once played");
    run.player.lock().unwrap().pause().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let n = run.shown.0.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(run.shown.0.lock().unwrap().len(), n, "no pictures while paused");
}

#[test]
fn what_is_not_media_is_an_error() {
    let bogus = tmp("bogus.mp4");
    std::fs::write(&bogus, vec![0x42u8; 100_000]).unwrap();
    let run = Run::new(&bogus, true, None);
    run.wait_for(Duration::from_secs(10), |s| matches!(s, Seen::Error(_))).expect("an error");
    let _ = std::fs::remove_file(&bogus);
}

/// The decoding thread on the WebM read from a file: the same pictures must come as through the player's byte cache.
#[test]
fn vp9_decodes_from_a_file_through_the_worker() {
    use lucina_video_play::worker::Worker;
    let file = std::io::BufReader::new(std::fs::File::open(clip("vp9-vorbis.webm")).unwrap());
    let demux = lucina_video::demux::open(file).unwrap();
    let codec = demux.track().codec;
    let fds = lucinanet_media::sys::nonblocking_cloexec_pipe().unwrap_or([0, 0]);
    let w = Worker::start(demux, codec, 0, fds[1]).unwrap();
    let t = Instant::now();
    let mut got = 0;
    while t.elapsed() < Duration::from_secs(20) {
        let mut s = w.lock();
        while s.queue.pop().is_some() {
            got += 1;
        }
        let (eof, err, decoded) = (s.eof, s.error.clone(), s.counts.decoded);
        drop(s);
        w.wake();
        if eof || err.is_some() {
            eprintln!("eof {eof} err {err:?} decoded {decoded} got {got}");
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(got, 96);
}
