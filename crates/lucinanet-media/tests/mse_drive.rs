/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Media Source Extensions in the backend, driven as Servo's SourceBuffer drives it: attach, add the buffers, append a segment at
//! a time (YouTube's pattern: one buffer for pictures, one for sound; fragmented MP4 by its boxes, WebM by its clusters), end the
//! stream, play. Every picture shown is compared with FFmpeg's for the same frames (the fragmented files are `-c copy` of
//! tests/media/h264-aac.mp4, so its framemd5 is the reference; the WebM ones are re-encodes, so they carry their own).
#![cfg(feature = "mse")]

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucinanet_media::player::{LucinaPlayer, Options};
use md5::{Digest, Md5};
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

// WebM is EBML: the bytes before the first Cluster element are the initialization segment (EBML header, Segment with its Info
// and Tracks), and each Cluster element (id + size + body) is one media segment, as YouTube appends a WebM stream.
const EBML_SEGMENT: u32 = 0x1853_8067;
const EBML_TRACKS: u32 = 0x1654_AE6B;
const EBML_CLUSTER: u32 = 0x1F43_B675;

/// An EBML element id at `at`: (id, its length in bytes). Ids are 1-4 bytes; the leading bit marks the last byte of the id.
fn ebml_id(b: &[u8], at: usize) -> Option<(u32, usize)> {
    let first = *b.get(at)?;
    if first == 0 {
        return None;
    }
    let (mut len, mut mask) = (1, 0x80u8);
    while first & mask == 0 {
        len += 1;
        mask >>= 1;
        if len > 4 {
            return None;
        }
    }
    if at + len > b.len() {
        return None;
    }
    let mut id = 0u32;
    for i in 0..len {
        id = (id << 8) | u32::from(b[at + i]);
    }
    Some((id, len))
}

/// An EBML element size at `at`: (size, its length in bytes). Sizes are vints of 1-8 bytes; all ones is the unknown size.
fn ebml_size(b: &[u8], at: usize) -> Option<(Option<usize>, usize)> {
    let first = *b.get(at)?;
    if first == 0 {
        return None;
    }
    let (mut len, mut mask) = (1, 0x80u8);
    while first & mask == 0 {
        len += 1;
        mask >>= 1;
        if len > 8 {
            return None;
        }
    }
    if at + len > b.len() {
        return None;
    }
    let mut v = u64::from(first & (mask - 1));
    for i in 1..len {
        v = (v << 8) | u64::from(b[at + i]);
    }
    let unknown = v == (1u64 << (7 * len)) - 1;
    Some((if unknown { None } else { Some(v as usize) }, len))
}

/// The elements of one EBML level in `[at, end)`: (id, start, header length, body end). An element of unknown size runs to `end`.
fn ebml_level(b: &[u8], mut at: usize, end: usize) -> Vec<(u32, usize, usize, usize)> {
    let mut out = Vec::new();
    while at < end {
        let Some((id, il)) = ebml_id(b, at) else { break };
        let Some((size, sl)) = ebml_size(b, at + il) else { break };
        let head = il + sl;
        let body_end = match size {
            Some(s) => (at + head + s).min(end),
            None => end,
        };
        out.push((id, at, head, body_end));
        match size {
            Some(s) => at += head + s,
            None => break,
        }
    }
    out
}

/// `[EBML header + Segment + Info + Tracks]`, then each Cluster element, as a page splits a WebM stream for its SourceBuffers.
fn webm_segments(b: &[u8]) -> Vec<Vec<u8>> {
    let mut first_cluster = None;
    let mut clusters: Vec<(usize, usize)> = Vec::new();
    for (id, at, head, body_end) in ebml_level(b, 0, b.len()) {
        if id == EBML_CLUSTER {
            first_cluster.get_or_insert(at);
            clusters.push((at, body_end));
        } else if id == EBML_SEGMENT {
            // The Segment's own size may be unknown, so its children run to the end of the file.
            for (cid, cat, _chead, cbody_end) in ebml_level(b, at + head, body_end) {
                if cid == EBML_CLUSTER {
                    first_cluster.get_or_insert(cat);
                    clusters.push((cat, cbody_end));
                }
            }
        }
    }
    let Some(at) = first_cluster else { return vec![b.to_vec()] };
    let mut out = vec![b[..at].to_vec()];
    out.extend(clusters.iter().map(|&(s, e)| b[s..e].to_vec()));
    out
}

#[test]
fn webm_segments_split_the_init_segment_from_the_clusters() {
    let b = std::fs::read(clip("frag-v-vp9.webm")).unwrap();
    let segs = webm_segments(&b);
    assert!(segs.len() >= 5, "the initialization segment and at least 4 clusters, got {}", segs.len());
    assert!(segs[0].windows(4).any(|w| w == EBML_TRACKS.to_be_bytes().as_slice()), "the init segment holds the Tracks element");
    for s in &segs[1..] {
        assert_eq!(&s[..4], &EBML_CLUSTER.to_be_bytes()[..], "every media segment starts with a Cluster");
    }
}

struct Count(Arc<Mutex<u32>>);

impl VideoFrameRenderer for Count {
    fn render(&mut self, _frame: VideoFrame) {
        *self.0.lock().unwrap() += 1;
    }
}

fn play(buffers: &[(&str, &str)], seek_to: Option<f64>) -> (Vec<String>, u32, Duration, PathBuf) {
    let (events, shown, took, dump, _) = play_full(buffers, seek_to);
    (events, shown, took, dump)
}

/// `play`, also returning each SourceBuffer's buffered ranges after the appends.
fn play_full(buffers: &[(&str, &str)], seek_to: Option<f64>) -> (Vec<String>, u32, Duration, PathBuf, Vec<Vec<std::ops::Range<f64>>>) {
    let (tx, rx) = mpsc::channel::<PlayerEvent>();
    let tx = Mutex::new(tx);
    let callback = GenericCallback::new(move |e: Result<PlayerEvent, _>| {
        let _ = tx.lock().unwrap().send(e.unwrap());
    })
    .unwrap();
    let shown = Arc::new(Mutex::new(0));
    let renderer: Arc<Mutex<dyn VideoFrameRenderer>> = Arc::new(Mutex::new(Count(shown.clone())));
    // One file per run: tests run in parallel, and two of them have two buffers each.
    static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let run = RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dump = std::env::temp_dir().join(format!("lucinanet-mse-{}-{run}.yuv", std::process::id()));
    let player = LucinaPlayer::new(1, StreamType::Seekable, callback, Some(renderer), Options { dump_yuv: Some(dump.clone()) });
    player.mse_attach().unwrap();
    player.mse_set_duration(f64::NAN);
    let ids: Vec<usize> = buffers.iter().map(|(_, mime)| player.mse_add_source_buffer(mime).unwrap()).collect();
    let mut buffered = Vec::new();
    for ((file, _), id) in buffers.iter().zip(&ids) {
        let bytes = std::fs::read(clip(file)).unwrap();
        // A page splits by what the file is: a fragmented MP4 by its boxes, a WebM by its clusters.
        let segs = if file.ends_with(".webm") { webm_segments(&bytes) } else { segments(&bytes) };
        for (i, seg) in segs.into_iter().enumerate() {
            let r = player.mse_append(*id, seg, 0.0).unwrap();
            if i == 0 {
                assert!(r.init_segment.is_some(), "{file}: the first append is the initialization segment");
            }
        }
        let b = player.mse_buffered(*id);
        assert_eq!(b.len(), 1, "{file}: one buffered range, {b:?}");
        // FFmpeg's fragmenting muxer drops the edit list, so the video's first frame is at 2/24 s (its B-frame delay).
        assert!(b[0].start < 0.1 && b[0].end > 3.9, "{file}: {b:?}");
        buffered.push(b);
    }
    player.mse_set_duration(4.0);
    player.mse_end_of_stream();
    let mut events = Vec::new();
    let t0 = Instant::now();
    let mut played = false;
    let mut seeked = false;
    let mut end = None;
    while t0.elapsed() < Duration::from_secs(20) {
        let Ok(e) = rx.recv_timeout(Duration::from_millis(100)) else { continue };
        match &e {
            PlayerEvent::StateChanged(PlaybackState::Paused) if !played => {
                played = true;
                player.play().unwrap();
            },
            PlayerEvent::PositionChanged(p) if *p > 1.0 && !seeked => {
                if let Some(t) = seek_to {
                    seeked = true;
                    player.seek(t).unwrap();
                }
            },
            PlayerEvent::EndOfStream => {
                end = Some(t0.elapsed());
                events.push("EndOfStream".to_owned());
                break;
            },
            PlayerEvent::Error(err) => panic!("player error: {err}"),
            _ => {},
        }
        let name = format!("{e:?}");
        events.push(name.split('(').next().unwrap().to_owned());
    }
    drop(player);
    std::thread::sleep(Duration::from_millis(100));
    let n = *shown.lock().unwrap();
    (events, n, end.expect("EndOfStream"), dump, buffered)
}

fn exact_frames(dump: &Path) -> (usize, usize) {
    exact_frames_as(dump, "h264-aac.framemd5", 640, 360, 2)
}

/// Every picture shown against FFmpeg's `reference` framemd5: our frame at presentation time `pts` (24 fps clips) must equal the
/// reference frame at `round(pts * 24 / 1e3) - shift`, `pts` the milliseconds of the `.pts` sidecar's second column. A fragmented
/// MP4 loses the edit list, so its first frame is `shift` frames later than in the original; WebM keeps its timestamps, so there
/// the shift is 0.
fn exact_frames_as(dump: &Path, reference: &str, width: usize, height: usize, shift: i64) -> (usize, usize) {
    let ours = std::fs::read(dump).unwrap();
    let pts: Vec<f64> = std::fs::read_to_string(dump.with_extension("pts"))
        .unwrap()
        .lines()
        .map(|l| l.split_whitespace().nth(1).unwrap().parse().unwrap())
        .collect();
    let theirs: Vec<String> = std::fs::read_to_string(clip(reference))
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap().trim().to_owned())
        .collect();
    let frame = width * height * 3 / 2;
    let mut same = 0;
    for (i, f) in ours.chunks(frame).enumerate() {
        let at = ((pts[i] * 24.0 / 1000.0).round() as i64 - shift).max(0) as usize;
        if theirs.get(at) == Some(&format!("{:x}", Md5::digest(f))) {
            same += 1;
        }
    }
    let _ = std::fs::remove_file(dump);
    let _ = std::fs::remove_file(dump.with_extension("pts"));
    (same, ours.len() / frame)
}

#[test]
fn two_source_buffers_as_youtube_play_frame_exact() {
    let (events, shown, took, dump) = play(
        &[("frag-v.mp4", "video/mp4; codecs=\"avc1.64001e\""), ("frag-a.mp4", "audio/mp4; codecs=\"mp4a.40.2\"")],
        None,
    );
    assert!(events.iter().any(|e| e == "MetadataUpdated"), "{events:?}");
    assert!((3.5..6.0).contains(&took.as_secs_f64()), "{took:?}");
    assert!(shown >= 90, "{shown} shown");
    let (same, all) = exact_frames(&dump);
    assert_eq!(same, all, "every picture shown equal to FFmpeg's");
}

#[test]
fn one_muxed_source_buffer_plays_and_seeks() {
    let (events, shown, _took, dump) = play(&[("frag-av.mp4", "video/mp4; codecs=\"avc1.64001e, mp4a.40.2\"")], Some(3.0));
    assert!(events.iter().any(|e| e == "SeekDone"), "{events:?}");
    assert!(shown >= 40, "{shown} shown");
    let (same, all) = exact_frames(&dump);
    assert_eq!(same, all);
}

#[test]
fn a_type_this_cannot_play_is_refused_and_garbage_is_a_decode_error() {
    let callback = GenericCallback::new(|_: Result<PlayerEvent, _>| {}).unwrap();
    let player = LucinaPlayer::new(2, StreamType::Seekable, callback, None, Options::default());
    player.mse_attach().unwrap();
    assert!(player.mse_add_source_buffer("video/webm; codecs=\"vp8\"").is_err());
    assert!(player.mse_add_source_buffer("video/x-flv").is_err());
    let id = player.mse_add_source_buffer("video/mp4; codecs=\"avc1.64001e\"").unwrap();
    // A media segment before any initialization segment.
    let segs = segments(&std::fs::read(clip("frag-v.mp4")).unwrap());
    assert!(player.mse_append(id, segs[1].clone(), 0.0).is_err());
}

/// YouTube's WebM: VP9 pictures in one SourceBuffer, Opus sound (itags 249-251) in another, an initialization segment and then
/// one Cluster at a time. Every picture shown must equal FFmpeg's for frag-v-vp9.webm.
#[test]
fn vp9_and_opus_webm_as_youtube_play_frame_exact() {
    let (events, shown, took, dump, _buffered) = play_full(
        &[("frag-v-vp9.webm", "video/webm; codecs=\"vp9\""), ("frag-a-opus.webm", "audio/webm; codecs=\"opus\"")],
        None,
    );
    assert!(events.iter().any(|e| e == "MetadataUpdated"), "{events:?}");
    assert!((3.5..6.0).contains(&took.as_secs_f64()), "{took:?}");
    assert!(shown >= 90, "{shown} shown");
    // WebM keeps its timestamps, so no shift as with the fragmented MP4.
    let (same, all) = exact_frames_as(&dump, "frag-v-vp9.framemd5", 640, 360, 0);
    assert_eq!(same, all, "every picture shown equal to FFmpeg's");
}

/// Opus alone in a SourceBuffer: no pictures, the sound clock drives the stream to its end.
#[test]
fn opus_only_webm_plays_to_the_end() {
    let (_events, _shown, took, dump, buffered) = play_full(&[("frag-a-opus.webm", "audio/webm; codecs=\"opus\"")], None);
    assert!(buffered[0].iter().any(|r| r.start <= 0.1 && r.end >= 3.9), "buffered {buffered:?}");
    assert!((3.5..6.0).contains(&took.as_secs_f64()), "{took:?}");
    let _ = std::fs::remove_file(&dump);
    let _ = std::fs::remove_file(dump.with_extension("pts"));
}

/// a seek reaches the player as `seek(t)`, which lands some time later, and a `pause()` that arrives while it is in
/// flight — the plumbing's, not the page's, which sees no `pause` event and so never calls `play()` to answer it. A pause that
/// survives its own seek leaves the mixer stream not-playing, which is never mixed: the 1 s ring fills, the producer parks, the
/// sound stops reporting a position and the clock freezes with the whole clip still buffered. Two buffers, as the page: the seek
/// lands when the pictures flush, so the pause genuinely arrives with it in flight (a sound-only stream seeks synchronously, and
/// there the following pause is an ordinary one). Run with `LUCINANET_MEDIA_RECOVERY=0`, so that it passes on the fix and not on
/// the recovery re-open.
#[test]
fn a_pause_arriving_with_a_seek_does_not_stop_the_sound() {
    let (tx, rx) = mpsc::channel::<PlayerEvent>();
    let tx = Mutex::new(tx);
    let callback = GenericCallback::new(move |e: Result<PlayerEvent, _>| {
        let _ = tx.lock().unwrap().send(e.unwrap());
    })
    .unwrap();
    let shown = Arc::new(Mutex::new(0));
    let renderer: Arc<Mutex<dyn VideoFrameRenderer>> = Arc::new(Mutex::new(Count(shown.clone())));
    let player = LucinaPlayer::new(1, StreamType::Seekable, callback, Some(renderer), Options::default());
    player.mse_attach().unwrap();
    player.mse_set_duration(f64::NAN);
    let v = player.mse_add_source_buffer("video/webm; codecs=\"vp9\"").unwrap();
    let a = player.mse_add_source_buffer("audio/webm; codecs=\"opus\"").unwrap();
    for seg in webm_segments(&std::fs::read(clip("frag-v-vp9.webm")).unwrap()) {
        player.mse_append(v, seg, 0.0).unwrap();
    }
    for seg in webm_segments(&std::fs::read(clip("frag-a-opus.webm")).unwrap()) {
        player.mse_append(a, seg, 0.0).unwrap();
    }
    player.mse_set_duration(4.0);
    player.mse_end_of_stream();
    let mut played = false;
    let mut seeked = false;
    let mut ended = false;
    let mut last = 0.0f64;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(15) && !ended {
        let Ok(e) = rx.recv_timeout(Duration::from_millis(100)) else { continue };
        match &e {
            PlayerEvent::StateChanged(PlaybackState::Paused) if !played => {
                played = true;
                player.play().unwrap();
            },
            PlayerEvent::PositionChanged(p) => {
                last = *p;
                if *p > 1.0 && !seeked {
                    seeked = true;
                    player.seek(2.0).unwrap();
                    player.pause().unwrap(); // the pause of the seek plumbing, which nothing answers
                }
            },
            PlayerEvent::EndOfStream => ended = true,
            PlayerEvent::Error(err) => panic!("player error: {err}"),
            _ => {},
        }
    }
    let frames = *shown.lock().unwrap();
    drop(player);
    assert!(ended, "the sound stopped at {last:.2} s of a 4 s clip and never finished: the seek's pause outlived the seek ({frames} pictures shown)");
}
