/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Replaying a recorded MSE session. `round_trip` records one player's `mse_*` calls (`LUCINANET_MEDIA_MSE_DUMP`) while it
//! is driven like tests/mse_drive.rs drives one, then replays the recording into a second player and plays that to the end.
//! `real_session` replays a directory recorded from a real page (`LUCINANET_MEDIA_MSE_REPLAY`) and plays it; it is ignored,
//! so plain `cargo test` skips it.
#![cfg(feature = "mse")]

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucinanet_media::mse_dump::replay;
use lucinanet_media::player::{LucinaPlayer, Options};
use servo_base::generic_channel::GenericCallback;
use servo_media_player::video::{VideoFrame, VideoFrameRenderer};
use servo_media_player::{PlaybackState, Player, PlayerEvent, StreamType};

fn clip(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/media").join(name)
}

/// `[ftyp+moov]`, then each `moof+mdat` pair, as a page splits a fragmented MP4.
fn segments(b: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let (mut at, mut start, mut init) = (0usize, 0usize, true);
    while at + 8 <= b.len() {
        let size = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        let kind = &b[at + 4..at + 8];
        if kind == b"moof" && init {
            out.push(b[start..at].to_vec());
            init = false;
            start = at;
        } else if kind == b"moof" {
            start = at;
        }
        at += size;
        if kind == b"mdat" && !init {
            out.push(b[start..at].to_vec());
            start = at;
        }
    }
    out
}

struct Count(Arc<Mutex<u32>>);

impl VideoFrameRenderer for Count {
    fn render(&mut self, _frame: VideoFrame) {
        *self.0.lock().unwrap() += 1;
    }
}

fn channel() -> (GenericCallback<PlayerEvent>, mpsc::Receiver<PlayerEvent>) {
    let (tx, rx) = mpsc::channel::<PlayerEvent>();
    let tx = Mutex::new(tx);
    let callback = GenericCallback::new(move |e: Result<PlayerEvent, _>| {
        let _ = tx.lock().unwrap().send(e.unwrap());
    })
    .unwrap();
    (callback, rx)
}

/// The event loop of tests/mse_drive.rs: play on the first `Paused`, until `EndOfStream` or `limit`.
fn play_to_end(player: &LucinaPlayer, rx: &mpsc::Receiver<PlayerEvent>, limit: Duration) -> (bool, Vec<String>) {
    let t0 = Instant::now();
    let mut played = false;
    let mut errors = Vec::new();
    while t0.elapsed() < limit {
        let Ok(e) = rx.recv_timeout(Duration::from_millis(100)) else { continue };
        match &e {
            PlayerEvent::StateChanged(PlaybackState::Paused) if !played => {
                played = true;
                player.play().unwrap();
            },
            PlayerEvent::EndOfStream => return (true, errors),
            PlayerEvent::Error(err) => errors.push(err.clone()),
            _ => {},
        }
    }
    (false, errors)
}

fn same_ranges(a: &[Range<f64>], b: &[Range<f64>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x.start - y.start).abs() <= 1e-6 && (x.end - y.end).abs() <= 1e-6)
}

#[test]
fn round_trip() {
    // The dump directory is read once per process, so it must be set before the first player is made (this binary's only
    // other test is ignored, so nothing else makes a player first).
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dump = std::env::temp_dir().join(format!("lucinanet-mse-dump-{}-{stamp}", std::process::id()));
    // SAFETY: before any player thread is started, and no other test in this binary runs beside this one.
    unsafe { std::env::set_var("LUCINANET_MEDIA_MSE_DUMP", &dump) };

    // Drive a player as tests/mse_drive.rs does: YouTube's pattern, one buffer for pictures, one for sound.
    let buffers = [("frag-v.mp4", "video/mp4; codecs=\"avc1.64001e\""), ("frag-a.mp4", "audio/mp4; codecs=\"mp4a.40.2\"")];
    let (cb, _rx) = channel();
    let player = LucinaPlayer::new(1, StreamType::Seekable, cb, None, Options::default());
    player.mse_attach().unwrap();
    player.mse_set_duration(f64::NAN);
    let ids: Vec<usize> = buffers.iter().map(|(_, mime)| player.mse_add_source_buffer(mime).unwrap()).collect();
    let mut appended = 0;
    for ((file, _), id) in buffers.iter().zip(&ids) {
        for seg in segments(&std::fs::read(clip(file)).unwrap()) {
            player.mse_append(*id, seg, 0.0).unwrap();
            appended += 1;
        }
    }
    player.mse_set_duration(4.0);
    player.mse_end_of_stream();
    let first: Vec<(usize, Vec<Range<f64>>)> = ids.iter().map(|id| (*id, player.mse_buffered(*id))).collect();

    // The recording: one subdirectory for this player, its calls.tsv with a line per call.
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&dump).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    assert_eq!(dirs.len(), 1, "one recorded player in {dump:?}");
    let recorded = dirs.remove(0);
    let lines = std::fs::read_to_string(recorded.join("calls.tsv")).unwrap().lines().count();
    assert_eq!(lines, appended + 6, "attach, two durations, two buffers, the appends, end of stream");
    drop(player);

    // A second player (the variable was read once, so it records too, into its own directory; irrelevant here), replayed.
    let (cb, rx) = channel();
    let shown = Arc::new(Mutex::new(0u32));
    let renderer: Arc<Mutex<dyn VideoFrameRenderer>> = Arc::new(Mutex::new(Count(shown.clone())));
    let player = LucinaPlayer::new(2, StreamType::Seekable, cb, Some(renderer), Options::default());
    let report = replay(&recorded, &player).unwrap();
    assert_eq!(report.appends, appended, "every append replayed");
    assert!(report.append_errors.is_empty(), "{:?}", report.append_errors);
    assert_eq!(report.buffered.len(), first.len());
    for ((sb, ranges), (sb1, ranges1)) in report.buffered.iter().zip(&first) {
        assert_eq!(sb, sb1);
        assert!(same_ranges(ranges, ranges1), "sb {sb}: {ranges:?} against the first player's {ranges1:?}");
    }

    let (ended, errors) = play_to_end(&player, &rx, Duration::from_secs(20));
    assert!(ended, "no EndOfStream; errors: {errors:?}");
    assert!(errors.is_empty(), "{errors:?}");
    let n = *shown.lock().unwrap();
    assert!(n >= 90, "{n} frames shown");
    drop(player);
    let _ = std::fs::remove_dir_all(&dump);
}

#[test]
#[ignore]
fn real_session() {
    let Some(dir) = std::env::var_os("LUCINANET_MEDIA_MSE_REPLAY") else {
        eprintln!("LUCINANET_MEDIA_MSE_REPLAY is not set: nothing to replay");
        return;
    };
    let dir = PathBuf::from(dir);
    let (cb, rx) = channel();
    let shown = Arc::new(Mutex::new(0u32));
    let renderer: Arc<Mutex<dyn VideoFrameRenderer>> = Arc::new(Mutex::new(Count(shown.clone())));
    let player = LucinaPlayer::new(1, StreamType::Seekable, cb, Some(renderer), Options::default());
    let report = replay(&dir, &player).unwrap();
    eprintln!("replayed {}: {} appends, errors {:?}", dir.display(), report.appends, report.append_errors);
    for (sb, ranges) in &report.buffered {
        eprintln!("buffered sb {sb}: {ranges:?}");
    }
    let (ended, errors) = play_to_end(&player, &rx, Duration::from_secs(60));
    let n = *shown.lock().unwrap();
    eprintln!("ended={ended} frames shown={n} player errors={errors:?}");
    assert!(report.append_errors.is_empty(), "{:?}", report.append_errors);
    assert!(n > 0, "no frames shown");
}
