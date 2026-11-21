/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Media Source Extensions, the backend's half: what a `SourceBuffer` holds and how appended bytes become coded frames in it.
//!
//! The original is the W3C "Media Source Extensions" specification (2024 Recommendation track), sections 3.5 (algorithms of a
//! SourceBuffer) and 3.5.8 (coded frame processing): timestamp offset, the "segments" and "sequence" modes, the append window,
//! discontinuities, the random access point a track buffer needs after one, the removal of frames a new one overlaps and of
//! whatever depends on them up to the next random access point, buffered ranges as the intersection of the track buffers', and
//! coded frame removal and eviction. Where the specification leaves room this does what Chromium does (behaviour only):
//! buffered ranges join across gaps under a frame's duration, and the quota is 150 MB for a SourceBuffer with video and 12 MB
//! for one with sound only.
//!
//! The DOM half (MediaSource, SourceBuffer, events, readyState) is Servo's script (patch B); the byte stream parsers are
//! `lucina_video::mse`. Times here are microseconds.

use std::collections::BTreeMap;

use lucina_video::mse::{Event, Format, Frame, Init, ParseError, Parser, TrackKind};

use crate::rules::buffered;
pub use crate::rules::buffered::intersect;

/// Chromium's default quotas.
pub const VIDEO_QUOTA: usize = 150 << 20;
pub const AUDIO_QUOTA: usize = 12 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Segments,
    Sequence,
}

/// What an append did that the DOM must act on.
#[derive(Debug, Default, PartialEq)]
pub struct Appended {
    /// An initialization segment was received: the tracks, for the first-init checks and the track lists.
    pub init: Option<Init>,
    /// Coded frames were added.
    pub frames: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppendError {
    /// The append error algorithm: decode error.
    Parse(ParseError),
    /// An init segment that does not match the first one (track count or kinds), which the spec makes a decode error.
    InitMismatch,
    /// A media segment before any initialization segment.
    NoInit,
    /// The buffer is full and eviction could not make room: QuotaExceededError.
    Quota,
}

/// One track's coded frames, in decode order.
#[derive(Default)]
pub struct TrackBuffer {
    pub kind: Option<TrackKind>,
    /// Keyed by decode time; frames with equal decode times keep their order through `seq`.
    frames: BTreeMap<(i64, u64), Frame>,
    seq: u64,
    bytes: usize,
    last_dts: Option<i64>,
    last_duration: Option<i64>,
    highest_end: Option<i64>,
    need_rap: bool,
}

impl TrackBuffer {
    pub(crate) fn new(kind: TrackKind) -> TrackBuffer {
        TrackBuffer { kind: Some(kind), need_rap: true, ..TrackBuffer::default() }
    }

    pub fn is_video(&self) -> bool {
        matches!(self.kind, Some(TrackKind::Video { .. }))
    }

    /// Frames in decode order from the last random access point at or before `pts` (presentation time), for a player that seeks.
    pub fn from_rap(&self, pts: i64) -> Option<(i64, u64)> {
        self.frames.iter().filter(|(_, f)| f.key && f.pts_us <= pts).map(|(k, _)| *k).next_back().or_else(|| {
            // Nothing at or before: the first random access point after.
            self.frames.iter().find(|(_, f)| f.key).map(|(k, _)| *k)
        })
    }

    /// The frame after `key` in decode order.
    pub fn next_after(&self, key: Option<(i64, u64)>) -> Option<((i64, u64), &Frame)> {
        match key {
            None => self.frames.iter().next().map(|(k, f)| (*k, f)),
            Some(k) => self.frames.range((std::ops::Bound::Excluded(k), std::ops::Bound::Unbounded)).next().map(|(k, f)| (*k, f)),
        }
    }

    /// The frame before `key` in decode order; `None` when `key` is the first.
    pub fn prev_key(&self, key: (i64, u64)) -> Option<(i64, u64)> {
        self.frames.range(..key).next_back().map(|(k, _)| *k)
    }

    pub fn get(&self, key: (i64, u64)) -> Option<&Frame> {
        self.frames.get(&key)
    }

    /// Presentation ranges, joined across gaps shorter than the gap's neighbouring frame (Chromium's fudge; `rules::buffered`).
    pub fn ranges(&self) -> Vec<(i64, i64)> {
        let mut spans: Vec<(i64, i64)> = self.frames.values().map(|f| buffered::span(f.pts_us, f.duration_us)).collect();
        spans.sort_unstable();
        buffered::join(&spans)
    }

    /// The frame with the greatest presentation time.
    pub fn last(&self) -> Option<&Frame> {
        self.frames.values().max_by_key(|f| f.pts_us)
    }

    /// The highest presentation time a frame reaches with its duration: the end of what is buffered.
    pub fn highest_pts(&self) -> Option<i64> {
        self.last().map(|f| f.pts_us + f.duration_us)
    }

    /// How many frames are buffered.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub(crate) fn insert(&mut self, f: Frame) {
        self.bytes += f.data.len();
        self.seq += 1;
        self.frames.insert((f.dts_us, self.seq), f);
    }

    /// Removes frames whose presentation time is in `[start, end)`, and then every frame after each in decode order up to the next
    /// random access point (spec 3.5.8 step 14 and coded frame removal step 3.4).
    fn remove_presented(&mut self, start: i64, end: i64) -> usize {
        let hit: Vec<(i64, u64)> = self.frames.iter().filter(|(_, f)| f.pts_us >= start && f.pts_us < end).map(|(k, _)| *k).collect();
        if hit.is_empty() {
            return 0;
        }
        let mut gone = 0;
        let mut doomed: Vec<(i64, u64)> = Vec::new();
        for k in &hit {
            doomed.push(*k);
            // Frames that depend on it: until the next random access point after it, in decode order.
            for (k2, f2) in self.frames.range((std::ops::Bound::Excluded(*k), std::ops::Bound::Unbounded)) {
                if f2.key {
                    break;
                }
                doomed.push(*k2);
            }
        }
        doomed.sort_unstable();
        doomed.dedup();
        for k in doomed {
            if let Some(f) = self.frames.remove(&k) {
                self.bytes -= f.data.len();
                gone += 1;
            }
        }
        gone
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// One SourceBuffer's state.
pub struct SourceBufferCore {
    parser: Parser,
    pub mode: Mode,
    pub timestamp_offset_us: i64,
    pub append_window_start_us: i64,
    /// `None` is +Infinity.
    pub append_window_end_us: Option<i64>,
    group_start: Option<i64>,
    group_end: i64,
    first_init: Option<Init>,
    pub tracks: BTreeMap<u64, TrackBuffer>,
    quota: usize,
    /// Bumped by every initialization segment: the second one remaps the track ids (`init_received`), so a reader that snapshotted
    /// `(buffer, track_id)` can tell its ids went stale.
    pub generation: u64,
}

impl SourceBufferCore {
    pub fn new(format: Format, has_video_codec: bool) -> SourceBufferCore {
        SourceBufferCore {
            parser: Parser::new(format),
            mode: Mode::Segments,
            timestamp_offset_us: 0,
            append_window_start_us: 0,
            append_window_end_us: None,
            group_start: None,
            group_end: 0,
            first_init: None,
            tracks: BTreeMap::new(),
            quota: if has_video_codec { VIDEO_QUOTA } else { AUDIO_QUOTA },
            generation: 0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.tracks.values().map(|t| t.bytes).sum::<usize>() + self.parser.pending()
    }

    pub fn init(&self) -> Option<&Init> {
        self.first_init.as_ref()
    }

    /// `mode = "sequence"` sets the group start to the group end (spec, mode setter step 6).
    pub fn set_mode(&mut self, mode: Mode) {
        if mode == Mode::Sequence {
            self.group_start = Some(self.group_end);
        }
        self.mode = mode;
    }

    /// `changeType()`: a parser for the new format; the track buffers stay (the next initialization segment maps onto them).
    pub fn change_type(&mut self, format: Format) {
        self.parser = Parser::new(format);
    }

    /// `abort()` and the reset parser state algorithm: a partial media segment is dropped.
    pub fn reset_parser(&mut self) {
        self.parser.reset();
        if self.mode == Mode::Sequence {
            self.group_start = Some(self.group_end);
        }
    }

    /// The prepare-append step: room for `incoming` bytes, evicting what is before `current_us` if need be (coded frame eviction).
    pub fn make_room(&mut self, incoming: usize, current_us: i64) -> Result<(), AppendError> {
        if buffered::fits(self.bytes(), incoming, self.quota) {
            return Ok(());
        }
        // As Chromium: what lies more than 10 s before the playback position, from the start, up to a random access point.
        let until = buffered::evict_before(current_us);
        for t in self.tracks.values_mut() {
            let keys: Vec<i64> = t.frames.values().filter(|f| f.key).map(|f| f.pts_us).collect();
            if let Some(cut) = buffered::evict_cut(&keys, until) {
                t.remove_presented(i64::MIN, cut);
            }
        }
        if buffered::fits(self.bytes(), incoming, self.quota) { Ok(()) } else { Err(AppendError::Quota) }
    }

    /// The buffer append algorithm on `bytes`.
    pub fn append(&mut self, bytes: &[u8]) -> Result<Appended, AppendError> {
        let mut events = Vec::new();
        let parsed = self.parser.append(bytes, &mut events);
        let mut out = Appended::default();
        for e in events {
            match e {
                Event::Init(init) => {
                    self.init_received(&init)?;
                    out.init = Some(init);
                },
                Event::Frames(frames) => {
                    if self.first_init.is_none() {
                        return Err(AppendError::NoInit);
                    }
                    for f in frames {
                        if self.process(f) {
                            out.frames += 1;
                        }
                    }
                },
            }
        }
        parsed.map_err(AppendError::Parse)?;
        Ok(out)
    }

    /// The initialization segment received algorithm, the parts that concern the track buffers.
    fn init_received(&mut self, init: &Init) -> Result<(), AppendError> {
        self.generation += 1;
        match &self.first_init {
            None => {
                for t in &init.tracks {
                    self.tracks.insert(t.id, TrackBuffer::new(t.kind.clone()));
                }
                self.first_init = Some(init.clone());
            },
            Some(first) => {
                // Same number of tracks of each kind (spec step 3.1); codecs may change (changeType), configs follow the new one.
                let kinds = |i: &Init| {
                    let v = i.tracks.iter().filter(|t| matches!(t.kind, TrackKind::Video { .. })).count();
                    (v, i.tracks.len() - v)
                };
                if kinds(first) != kinds(init) {
                    return Err(AppendError::InitMismatch);
                }
                // Track ids may differ between init segments; map by kind order.
                let mut old: Vec<u64> = self.tracks.keys().copied().collect();
                old.sort_unstable();
                let mut moved = BTreeMap::new();
                for t in &init.tracks {
                    let is_video = matches!(t.kind, TrackKind::Video { .. });
                    let pos = old.iter().position(|id| self.tracks[id].is_video() == is_video);
                    if let Some(p) = pos {
                        let id = old.remove(p);
                        let mut buf = self.tracks.remove(&id).expect("listed");
                        buf.kind = Some(t.kind.clone());
                        buf.need_rap = true;
                        moved.insert(t.id, buf);
                    }
                }
                self.tracks = moved;
            },
        }
        Ok(())
    }

    /// Coded frame processing for one frame (spec 3.5.8). `true` if it was added.
    fn process(&mut self, mut f: Frame) -> bool {
        // Restarted from step 1 at most once per frame, after a discontinuity.
        for _ in 0..2 {
            if self.mode == Mode::Sequence
                && let Some(start) = self.group_start.take()
            {
                self.timestamp_offset_us = start - f.pts_us;
                self.group_end = start;
                for t in self.tracks.values_mut() {
                    t.need_rap = true;
                }
            }
            let (pts, dts) = (f.pts_us + self.timestamp_offset_us, f.dts_us + self.timestamp_offset_us);
            let Some(t) = self.tracks.get_mut(&f.track) else { return false };
            let discontinuous = t.last_dts.is_some_and(|last| {
                let d = t.last_duration.unwrap_or(0);
                dts < last || dts - last > 2 * d
            });
            if discontinuous {
                match self.mode {
                    Mode::Segments => self.group_end = pts,
                    Mode::Sequence => self.group_start = Some(self.group_end),
                }
                for t in self.tracks.values_mut() {
                    t.last_dts = None;
                    t.last_duration = None;
                    t.highest_end = None;
                    t.need_rap = true;
                }
                continue;
            }
            let end = pts + f.duration_us;
            if pts < self.append_window_start_us || self.append_window_end_us.is_some_and(|w| end > w) {
                t.need_rap = true;
                return false;
            }
            if t.need_rap {
                if !f.key {
                    return false;
                }
                t.need_rap = false;
            }
            // Frames the new one overlaps go, with what depends on them. With no last decode time this is the first frame after
            // a discontinuity: overlap from its presentation time; otherwise from the highest end time so far (step 14).
            let from = match (t.last_dts, t.highest_end) {
                (Some(_), Some(h)) if h <= pts => pts,
                (Some(_), Some(h)) => h,
                _ => pts,
            };
            if from < end {
                t.remove_presented(from, end);
            }
            f.pts_us = pts;
            f.dts_us = dts;
            t.last_dts = Some(dts);
            t.last_duration = Some(f.duration_us);
            t.highest_end = Some(t.highest_end.map_or(end, |h| h.max(end)));
            t.insert(f);
            if end > self.group_end {
                self.group_end = end;
            }
            if self.mode == Mode::Sequence {
                // Generate timestamps is false; the offset already moves the group along.
            }
            return true;
        }
        false
    }

    /// Coded frame removal for `[start, end)`.
    pub fn remove(&mut self, start: i64, end: i64) {
        for t in self.tracks.values_mut() {
            t.remove_presented(start, end);
            t.need_rap = true;
        }
    }

    /// `buffered`: the intersection of every track buffer's ranges; once ended, the highest end time extends the last range.
    pub fn buffered(&self, ended: bool) -> Vec<(i64, i64)> {
        let per: Vec<Vec<(i64, i64)>> = self.tracks.values().map(TrackBuffer::ranges).collect();
        if per.is_empty() {
            return Vec::new();
        }
        let highest = per.iter().filter_map(|r| r.last().map(|x| x.1)).max().unwrap_or(0);
        let mut acc: Vec<(i64, i64)> = vec![(i64::MIN, highest)];
        for mut r in per {
            if ended && let Some(last) = r.last_mut() {
                last.1 = highest;
            }
            acc = intersect(&acc, &r);
        }
        acc
    }

    /// The largest end time of any track buffer, for the duration change at `endOfStream()`.
    pub fn highest_end(&self) -> Option<i64> {
        self.tracks.values().filter_map(|t| t.ranges().last().map(|r| r.1)).max()
    }
}

#[cfg(test)]
mod tests {
    use lucina_video::Codec;
    use lucina_video::mse::{AudioCodec, TrackInfo};

    use super::*;

    fn core() -> SourceBufferCore {
        let mut c = SourceBufferCore::new(Format::IsoBmff, true);
        let init = Init {
            tracks: vec![
                TrackInfo { id: 1, kind: TrackKind::Video { codec: Codec::H264, width: 64, height: 64, config: vec![] }, codec_name: "avc1".into() },
                TrackInfo { id: 2, kind: TrackKind::Audio { codec: AudioCodec::Aac, config: vec![], rate: 48000, channels: 2 }, codec_name: "mp4a.40.2".into() },
            ],
            duration_us: None,
        };
        c.init_received(&init).unwrap();
        c
    }

    fn frame(track: u64, pts: i64, dur: i64, key: bool) -> Frame {
        Frame { track, pts_us: pts, dts_us: pts, duration_us: dur, key, data: vec![0; 10] }
    }

    /// Ten video frames of 40 ms (a key every five) and 20 audio frames of 20 ms from `t`.
    fn segment(c: &mut SourceBufferCore, t: i64) {
        for i in 0..10 {
            assert!(c.process(frame(1, t + i * 40_000, 40_000, i % 5 == 0)));
        }
        for i in 0..20 {
            assert!(c.process(frame(2, t + i * 20_000, 20_000, true)));
        }
    }

    /// case F2, the fact a reader has to survive: the next initialization segment maps the tracks onto the ids it
    /// declares, by kind order. The frames ride along with the track buffer, under the new id, and the only thing that says the
    /// ids moved is `generation` — which every initialization segment bumps, and which the readers cached once and never read.
    #[test]
    fn a_second_initialization_segment_maps_the_tracks_to_its_own_ids() {
        let mut c = core();
        segment(&mut c, 0);
        assert_eq!(c.tracks.keys().copied().collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(c.generation, 1);
        let again = Init {
            tracks: vec![
                TrackInfo { id: 9, kind: TrackKind::Video { codec: Codec::H264, width: 64, height: 64, config: vec![] }, codec_name: "avc1".into() },
                TrackInfo { id: 8, kind: TrackKind::Audio { codec: AudioCodec::Aac, config: vec![], rate: 48000, channels: 2 }, codec_name: "mp4a.40.2".into() },
            ],
            duration_us: None,
        };
        c.init_received(&again).unwrap();
        assert_eq!(c.generation, 2, "the readers' sign that their snapshot of the ids is old");
        assert_eq!(c.tracks.keys().copied().collect::<Vec<_>>(), vec![8, 9], "the ids the new segment declares");
        assert_eq!(c.tracks[&9].frame_count(), 10, "the video frames came with their track");
        assert_eq!(c.tracks[&8].frame_count(), 20, "and the audio frames theirs");
        assert!(c.tracks.get(&1).is_none(), "what a reader still holding id 1 finds: nothing");
        assert!(c.process(frame(9, 400_000, 40_000, true)), "the new frames land on the new id");
        assert_eq!(c.tracks[&9].frame_count(), 11);
    }

    #[test]
    fn buffered_is_the_intersection_of_the_tracks() {
        let mut c = core();
        segment(&mut c, 0);
        segment(&mut c, 400_000);
        assert_eq!(c.buffered(false), vec![(0, 800_000)]);
        // Audio only further on: not buffered until video is there too.
        for i in 0..10 {
            c.process(frame(2, 2_000_000 + i * 20_000, 20_000, true));
        }
        assert_eq!(c.buffered(false), vec![(0, 800_000)]);
        segment(&mut c, 2_000_000);
        // The segment at 2 s brings both tracks to 2.4 s.
        assert_eq!(c.buffered(false), vec![(0, 800_000), (2_000_000, 2_400_000)]);
    }

    #[test]
    fn a_frame_before_a_random_access_point_is_dropped_after_a_discontinuity() {
        let mut c = core();
        // First frame not a key: dropped, the track needs one.
        assert!(!c.process(frame(1, 0, 40_000, false)));
        assert!(c.process(frame(1, 40_000, 40_000, true)));
        assert!(c.process(frame(1, 80_000, 40_000, false)));
        // A jump of a second: a discontinuity; a non-key frame there is dropped.
        assert!(!c.process(frame(1, 1_080_000, 40_000, false)));
        assert!(c.process(frame(1, 1_120_000, 40_000, true)));
    }

    #[test]
    fn appending_over_buffered_frames_replaces_them_and_their_dependents() {
        let mut c = core();
        for i in 0..10 {
            c.process(frame(1, i * 40_000, 40_000, i % 5 == 0));
        }
        assert_eq!(c.tracks[&1].len(), 10);
        // A new GOP from 0 (a new append after a seek back): resets with a discontinuity check, then overlaps.
        c.reset_parser();
        for t in c.tracks.values_mut() {
            t.last_dts = None;
            t.highest_end = None;
            t.need_rap = true;
        }
        c.process(frame(1, 0, 40_000, true));
        // Frame 0 replaced; frames 1..4 depended on it: gone; 5.. kept.
        let t = &c.tracks[&1];
        let pts: Vec<i64> = t.frames.values().map(|f| f.pts_us).collect();
        assert_eq!(pts, vec![0, 200_000, 240_000, 280_000, 320_000, 360_000]);
    }

    #[test]
    fn the_append_window_and_the_timestamp_offset() {
        let mut c = core();
        c.timestamp_offset_us = 10_000_000;
        c.append_window_start_us = 10_100_000;
        assert!(!c.process(frame(1, 0, 40_000, true)), "before the window");
        assert!(!c.process(frame(1, 40_000, 40_000, false)), "needs a key after the window cut");
        assert!(c.process(frame(1, 200_000, 40_000, true)));
        assert_eq!(c.tracks[&1].frames.values().next().unwrap().pts_us, 10_200_000);
    }

    #[test]
    fn sequence_mode_places_segments_one_after_another() {
        let mut c = core();
        c.set_mode(Mode::Sequence);
        segment(&mut c, 5_000_000);
        c.set_mode(Mode::Sequence);
        segment(&mut c, 9_000_000);
        assert_eq!(c.buffered(false), vec![(0, 800_000)]);
    }

    #[test]
    fn remove_takes_frames_and_what_depends_on_them() {
        let mut c = core();
        segment(&mut c, 0);
        c.remove(100_000, 150_000);
        // Video frame at 120 ms removed, and 160 (not key) after it; 200 is a key: kept. Audio 100..150 removed.
        let v: Vec<i64> = c.tracks[&1].frames.values().map(|f| f.pts_us).collect();
        assert_eq!(v, vec![0, 40_000, 80_000, 200_000, 240_000, 280_000, 320_000, 360_000]);
        assert_eq!(c.buffered(false), vec![(0, 100_000), (200_000, 400_000)]);
    }

    #[test]
    fn eviction_frees_what_is_long_played() {
        let mut c = core();
        c.quota = 2_000;
        for s in 0..6 {
            segment(&mut c, s * 400_000 * 10);
        }
        assert!(c.bytes() > 1_000);
        // At 30 s, room for 500 more bytes (1800 held, a quota of 2000): everything up to the last key before 20 s goes.
        c.make_room(500, 30_000_000).unwrap();
        assert!(c.tracks[&1].frames.values().all(|f| f.pts_us >= 16_000_000), "{:?}", c.tracks[&1].ranges());
        assert_eq!(c.make_room(10_000, 0), Err(AppendError::Quota));
    }
}
