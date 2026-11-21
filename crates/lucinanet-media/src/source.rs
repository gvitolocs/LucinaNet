/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The bytes script pushes, as files the demuxers can read and seek.
//!
//! The original is servo-media's GStreamer source (`servosrc`, an appsrc). Script fetches the resource and calls `push_data`; the
//! player says `EnoughData` and `NeedData`, and `SeekData(offset, lock)` when the demuxer wants bytes from elsewhere, which makes script
//! start a range request there and unlock; pushes that arrive while that is under way are dropped. Each new fetch starts with its data
//! source locked until `NeedData`.
//!
//! What differs (behaviour kept, structure not): GStreamer has one demuxer reading the stream once; here the picture's demuxer
//! (`lucina_video`) and the sound's (symphonia, through `lucina_audio_decode`) read the same bytes as two files, as the Video app reads a
//! file twice. So the bytes are kept as a sparse cache of runs with a memory cap, and each reader has its own position. After
//! `EnoughData` Servo cancels the fetch ("Backoff") and would answer the next `NeedData` with a seek to the playback position; this asks
//! for a fetch from where the old one stopped (`SeekData`) instead, which restarts the download without disturbing playback. When and
//! what to signal is `rules::flow`, which Verus checks.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::rules::flow::{self, Flow, Signal};

/// The most the cache holds before it drops runs behind every reader.
pub const MAX_CACHED: usize = 64 << 20;
/// Kept behind the slowest reader for short seeks back, as long as the cap allows.
pub const KEEP_BEHIND: u64 = 8 << 20;
/// One run is split at this, so eviction frees memory in useful pieces.
const RUN: usize = 1 << 20;

/// What the cache asks of script. The player turns these into `PlayerEvent`s.
#[derive(Debug, PartialEq, Eq)]
pub enum Ask {
    NeedData,
    EnoughData,
    /// A new fetch from this offset. The player answers with [`Source::fetch_started`] once script has unlocked.
    FetchFrom(u64),
}

/// Called with the source that asks, from whichever thread is reading.
pub type Asker = Box<dyn Fn(Ask, &Source) + Send + Sync>;

struct Reader {
    pos: u64,
    /// Waiting for bytes at `pos`.
    waiting: bool,
    /// Dropped readers stay in the list (ids are indices) but no longer count.
    active: bool,
}

struct State {
    runs: BTreeMap<u64, Vec<u8>>,
    cached: usize,
    /// The resource's length when script knows it.
    size: Option<u64>,
    /// Where the next pushed byte belongs.
    fetch_at: u64,
    /// A fetch is bringing bytes (not cancelled, not ended).
    live: bool,
    /// The fetch reached the end of the resource at `fetch_at`.
    ended: bool,
    /// A new fetch was asked for here and script has not unlocked it yet: pushes are dropped meanwhile.
    asked: Option<u64>,
    flow: Flow,
    seekable: bool,
    readers: Vec<Reader>,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    ask: Asker,
}

/// One player's bytes. Cloning shares them.
#[derive(Clone)]
pub struct Source(Arc<Shared>);

impl Source {
    /// Script's first fetch starts at 0 by itself; this waits for it to be unlocked by `NeedData`, which the player sends once it is
    /// ready ([`Source::start`]).
    pub fn new(ask: Asker) -> Source {
        Source(Arc::new(Shared {
            state: Mutex::new(State {
                runs: BTreeMap::new(),
                cached: 0,
                size: None,
                fetch_at: 0,
                live: true,
                ended: false,
                asked: None,
                flow: Flow::Filling,
                seekable: true,
                readers: Vec::new(),
                closed: false,
            }),
            cv: Condvar::new(),
            ask,
        }))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Unlocks script's first fetch.
    pub fn start(&self) {
        (self.0.ask)(Ask::NeedData, self);
    }

    /// A reader of its own, from the start.
    pub fn reader(&self) -> SourceReader {
        let mut s = self.lock();
        s.readers.push(Reader { pos: 0, waiting: false, active: true });
        SourceReader { source: self.clone(), id: s.readers.len() - 1 }
    }

    pub fn set_size(&self, size: u64) {
        let mut s = self.lock();
        s.size = (size > 0).then_some(size);
        drop(s);
        self.0.cv.notify_all();
    }

    pub fn size(&self) -> Option<u64> {
        self.lock().size
    }

    pub fn set_seekable(&self, seekable: bool) {
        self.lock().seekable = seekable;
    }

    pub fn seekable(&self) -> bool {
        self.lock().seekable
    }

    /// Script's `push_data`. `Err` is `PlayerError::EnoughData`.
    pub fn push(&self, data: &[u8]) -> Result<(), ()> {
        let mut s = self.lock();
        if s.closed || s.asked.is_some() {
            // GStreamer's servosrc drops what arrives while a seek is under way, and so does this.
            return Ok(());
        }
        if !flow::takes_push(s.flow) {
            // Servo cancels a seekable fetch on this; the next one is asked for from `fetch_at`.
            s.live = false;
            return Err(());
        }
        s.live = true;
        s.ended = false;
        let mut at = s.fetch_at;
        let mut rest = data;
        while !rest.is_empty() {
            let next = s.runs.range(at..).next().map(|(k, _)| *k);
            // Bytes already held, from an earlier fetch that ran on past here: skip them.
            if let Some((start, run)) = s.runs.range(..=at).next_back()
                && start + run.len() as u64 > at
            {
                let skip = ((start + run.len() as u64 - at) as usize).min(rest.len());
                at += skip as u64;
                rest = &rest[skip..];
                continue;
            }
            let take = flow::new_bytes(at, rest.len() as u64, next.unwrap_or(u64::MAX)) as usize;
            append(&mut s, at, &rest[..take]);
            at += take as u64;
            rest = &rest[take..];
        }
        s.fetch_at = at;
        evict(&mut s);
        let (f, signal) = flow::after_push(s.flow, ahead(&s));
        s.flow = f;
        if signal == Signal::Enough && s.seekable {
            // Servo cancels a seekable fetch on `EnoughData` (htmlmediaelement.rs, playback_enough_data).
            s.live = false;
        }
        drop(s);
        self.0.cv.notify_all();
        if signal == Signal::Enough {
            (self.0.ask)(Ask::EnoughData, self);
        }
        Ok(())
    }

    /// Script has nothing more after `fetch_at` for this fetch: the end of the resource.
    pub fn end_of_stream(&self) {
        let mut s = self.lock();
        if s.asked.is_none() {
            s.ended = true;
            s.live = false;
            if s.size.is_none() {
                s.size = Some(s.fetch_at);
            }
        }
        drop(s);
        self.0.cv.notify_all();
    }

    /// Script started the fetch [`Ask::FetchFrom`] asked for (`ok`), or could not.
    pub fn fetch_started(&self, offset: u64, ok: bool) {
        trace!("fetch from {offset} started: {ok}");
        let mut s = self.lock();
        if s.asked == Some(offset) {
            s.asked = None;
            if ok {
                s.fetch_at = offset;
                s.live = true;
                s.ended = false;
                s.flow = Flow::Filling;
            }
        }
        drop(s);
        self.0.cv.notify_all();
        if ok {
            // A new fetch's data source stays locked until this.
            (self.0.ask)(Ask::NeedData, self);
        }
    }

    /// Wakes every reader with an error: the player is going away.
    pub fn close(&self) {
        self.lock().closed = true;
        self.0.cv.notify_all();
    }

    /// The byte ranges held, for `buffered()`.
    pub fn ranges(&self) -> Vec<(u64, u64)> {
        let s = self.lock();
        let mut out: Vec<(u64, u64)> = Vec::new();
        for (start, run) in &s.runs {
            let end = start + run.len() as u64;
            match out.last_mut() {
                Some(last) if last.1 == *start => last.1 = end,
                _ => out.push((*start, end)),
            }
        }
        out
    }

    /// Asks for a fetch from `offset` unless one is asked for already. Called with the lock held; the ask goes out after it is let go.
    fn want(&self, s: &mut State, offset: u64) -> Option<Ask> {
        trace!("want a fetch from {offset} (asked {:?}, fetch at {}, live {}, ended {})", s.asked, s.fetch_at, s.live, s.ended);
        if s.asked.is_some() || !s.seekable {
            return None;
        }
        s.asked = Some(offset);
        s.live = false;
        Some(Ask::FetchFrom(offset))
    }
}

/// The run that `at` would extend, or a new one.
fn append(s: &mut State, at: u64, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    s.cached += bytes.len();
    if let Some((start, run)) = s.runs.range_mut(..at).next_back()
        && start + run.len() as u64 == at
        && run.len() + bytes.len() <= RUN
    {
        run.extend_from_slice(bytes);
        return;
    }
    // Split large pushes so no run is much over RUN.
    let mut at = at;
    for piece in bytes.chunks(RUN) {
        let mut v = Vec::with_capacity(RUN.max(piece.len()));
        v.extend_from_slice(piece);
        s.runs.insert(at, v);
        at += piece.len() as u64;
    }
}

/// Bytes cached ahead of the slowest reader inside the contiguous stretch that ends where the fetch writes. With no reader there, the
/// stretch's own length: a fetch nobody reads yet still stops at the high mark.
fn ahead(s: &State) -> u64 {
    let mut start = s.fetch_at;
    for (k, run) in s.runs.range(..s.fetch_at).rev() {
        if k + run.len() as u64 == start {
            start = *k;
        } else {
            break;
        }
    }
    s.readers
        .iter()
        .filter(|r| r.active && r.pos >= start && r.pos <= s.fetch_at)
        .map(|r| s.fetch_at - r.pos)
        .min()
        .unwrap_or(s.fetch_at - start)
}

/// Drops runs over the cap, farthest behind every reader first, and never one a reader is inside or less than `KEEP_BEHIND` behind.
fn evict(s: &mut State) {
    while s.cached > MAX_CACHED {
        let slowest = s.readers.iter().filter(|r| r.active).map(|r| r.pos).min().unwrap_or(s.fetch_at);
        let victim = s
            .runs
            .iter()
            .filter(|(k, run)| *k + run.len() as u64 + KEEP_BEHIND <= slowest)
            .map(|(k, _)| *k)
            .next();
        // Then runs past every reader and past the fetch, from the far end: an old fetch's leftovers.
        let fastest = s.readers.iter().filter(|r| r.active).map(|r| r.pos).max().unwrap_or(0).max(s.fetch_at);
        let victim = victim.or_else(|| s.runs.range(fastest + KEEP_BEHIND..).next_back().map(|(k, _)| *k));
        let Some(k) = victim else { break };
        if let Some(run) = s.runs.remove(&k) {
            s.cached -= run.len();
        }
    }
}

/// One reader's view: `Read` and `Seek` over the resource, waiting for bytes that are coming and asking for those that are not.
pub struct SourceReader {
    source: Source,
    id: usize,
}

impl SourceReader {
    pub fn source(&self) -> &Source {
        &self.source
    }
}

impl Drop for SourceReader {
    fn drop(&mut self) {
        self.source.lock().readers[self.id].active = false;
    }
}

impl Read for SourceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let shared = &self.source.0;
        let mut s = self.source.lock();
        loop {
            if s.closed {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the player closed"));
            }
            let pos = s.readers[self.id].pos;
            if let Some((start, run)) = s.runs.range(..=pos).next_back()
                && start + run.len() as u64 > pos
            {
                let from = (pos - start) as usize;
                let n = (run.len() - from).min(buf.len());
                buf[..n].copy_from_slice(&run[from..from + n]);
                let r = &mut s.readers[self.id];
                r.pos += n as u64;
                r.waiting = false;
                let (f, signal) = flow::after_read(s.flow, ahead(&s));
                s.flow = f;
                let ask = match signal {
                    // A seekable fetch was cancelled at `EnoughData`: a new one from where it stopped.
                    Signal::Need if s.seekable && !s.live && !s.ended && s.asked.is_none() => {
                        let at = s.fetch_at;
                        self.source.want(&mut s, at)
                    },
                    // Not seekable: script decides (it restarts the fetch at the playback position).
                    Signal::Need if !s.seekable && !s.ended => Some(Ask::NeedData),
                    _ => None,
                };
                drop(s);
                if let Some(a) = ask {
                    (shared.ask)(a, &self.source);
                }
                return Ok(n);
            }
            if s.size.is_some_and(|size| pos >= size) || (s.ended && pos >= s.fetch_at && s.size.is_none()) {
                return Ok(0);
            }
            let coming = s.asked.is_some_and(|a| flow::served_by_fetch(pos, a))
                || (s.asked.is_none() && s.live && flow::served_by_fetch(pos, s.fetch_at) && s.flow == Flow::Filling);
            let ask = if coming {
                None
            } else if !s.seekable {
                if s.ended {
                    return Ok(0);
                }
                None
            } else {
                // A fetch somewhere else, a cancelled one, or one that is full of bytes nobody here needs: a new one from here.
                let ask = if s.asked.is_some() {
                    None
                } else {
                    s.flow = Flow::Filling;
                    self.source.want(&mut s, pos)
                };
                if s.asked.is_some_and(|a| a != pos) && ask.is_none() {
                    // Another reader's fetch is being started: wait for it, then look again.
                }
                ask
            };
            trace!("reader {} waits at {pos} (asked {:?}, fetch at {}, live {}, ended {}, flow {:?})", self.id, s.asked, s.fetch_at, s.live, s.ended, s.flow);
            s.readers[self.id].waiting = true;
            if let Some(a) = ask {
                drop(s);
                (shared.ask)(a, &self.source);
                s = self.source.lock();
                continue;
            }
            s = shared.cv.wait_timeout(s, Duration::from_millis(500)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

impl Seek for SourceReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let mut s = self.source.lock();
        let pos = s.readers[self.id].pos;
        let new = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => pos.checked_add_signed(d),
            SeekFrom::End(d) => match s.size {
                Some(size) => size.checked_add_signed(d),
                None => return Err(io::Error::new(io::ErrorKind::Unsupported, "the resource's length is not known")),
            },
        };
        let new = new.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a seek before the start"))?;
        s.readers[self.id].pos = new;
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn source() -> (Source, mpsc::Receiver<Ask>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        (Source::new(Box::new(move |a, _| tx.lock().unwrap().send(a).unwrap())), rx)
    }

    #[test]
    fn reads_what_was_pushed_and_waits_at_the_end() {
        let (s, rx) = source();
        let mut r = s.reader();
        s.push(b"hello world").unwrap();
        let mut b = [0u8; 5];
        r.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"hello");
        let s2 = s.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            s2.push(b"!!").unwrap();
            s2.end_of_stream();
        });
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        t.join().unwrap();
        assert_eq!(rest, b" world!!");
        assert!(rx.try_recv().is_err(), "nothing asked of script");
    }

    #[test]
    fn a_read_far_ahead_asks_for_a_fetch_there_and_drops_pushes_until_it_starts() {
        let (s, rx) = source();
        s.set_size(100 << 20);
        let mut r = s.reader();
        s.push(&[1u8; 1000]).unwrap();
        r.seek(SeekFrom::Start(50 << 20)).unwrap();
        let s2 = s.clone();
        let t = std::thread::spawn(move || {
            let mut b = [0u8; 4];
            r.read_exact(&mut b).unwrap();
            b
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Ask::FetchFrom(50 << 20));
        s2.push(&[9u8; 10]).unwrap(); // the old fetch, still arriving: dropped
        s2.fetch_started(50 << 20, true);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Ask::NeedData);
        s2.push(&[7u8; 10]).unwrap();
        assert_eq!(t.join().unwrap(), [7u8; 4]);
        assert_eq!(s2.ranges(), vec![(0, 1000), (50 << 20, (50 << 20) + 10)]);
    }

    #[test]
    fn enough_then_a_fetch_from_where_it_stopped() {
        let (s, rx) = source();
        let mut r = s.reader();
        let chunk = vec![0u8; 1 << 20];
        let mut pushed = 0u64;
        loop {
            match s.push(&chunk) {
                Ok(()) => pushed += chunk.len() as u64,
                Err(()) => break,
            }
        }
        assert_eq!(pushed, flow::HIGH);
        assert_eq!(rx.try_recv().unwrap(), Ask::EnoughData);
        // Read until the bytes ahead fall to the low mark: a fetch from the end of what is held.
        let mut b = vec![0u8; 1 << 20];
        while rx.try_recv().is_err() {
            r.read_exact(&mut b).unwrap();
        }
        // (the loop consumed the ask; check it by state)
        assert_eq!(s.lock().asked, Some(flow::HIGH));
        s.fetch_started(flow::HIGH, true);
        assert_eq!(rx.recv().unwrap(), Ask::NeedData);
        s.push(&chunk).unwrap();
        assert_eq!(s.ranges(), vec![(0, flow::HIGH + chunk.len() as u64)]);
    }

    #[test]
    fn a_second_fetch_skips_bytes_already_held() {
        // Past SLACK: a read nearer than that waits for the running fetch instead.
        const FAR: u64 = 4 << 20;
        let (s, rx) = source();
        s.set_size(10 << 20);
        let mut r = s.reader();
        s.push(&[1u8; 100]).unwrap();
        r.seek(SeekFrom::Start(FAR)).unwrap();
        let s2 = s.clone();
        let t = std::thread::spawn(move || {
            let mut b = [0u8; 10];
            r.read_exact(&mut b).unwrap();
            r
        });
        assert_eq!(rx.recv().unwrap(), Ask::FetchFrom(FAR));
        s2.fetch_started(FAR, true);
        let _ = rx.recv();
        s2.push(&[2u8; 200]).unwrap();
        let mut r = t.join().unwrap();
        // Back to 50: inside the first run, nothing asked; read across its end into a fetch from 100 that overlaps FAR..FAR+200.
        r.seek(SeekFrom::Start(50)).unwrap();
        let mut b = [0u8; 50];
        r.read_exact(&mut b).unwrap();
        assert!(b.iter().all(|x| *x == 1));
        let s3 = s.clone();
        let t = std::thread::spawn(move || {
            let mut b = [0u8; 1];
            r.read_exact(&mut b).unwrap();
            b[0]
        });
        assert_eq!(rx.recv().unwrap(), Ask::FetchFrom(100));
        s3.fetch_started(100, true);
        let _ = rx.recv();
        s3.push(&vec![3u8; FAR as usize]).unwrap(); // 100..FAR+100: FAR..FAR+100 is held already and stays 2
        assert_eq!(t.join().unwrap(), 3);
        assert_eq!(s3.ranges(), vec![(0, FAR + 200)]);
        let mut r = s3.reader();
        r.seek(SeekFrom::Start(FAR + 50)).unwrap();
        let mut b = [0u8; 1];
        r.read_exact(&mut b).unwrap();
        assert_eq!(b[0], 2);
    }

    #[test]
    fn the_end_of_a_resource_of_known_size() {
        let (s, _rx) = source();
        s.set_size(4);
        let mut r = s.reader();
        s.push(b"abcd").unwrap();
        let mut v = Vec::new();
        r.read_to_end(&mut v).unwrap();
        assert_eq!(v, b"abcd");
        assert_eq!(r.seek(SeekFrom::End(-1)).unwrap(), 3);
    }

    #[test]
    fn closing_wakes_a_waiting_reader() {
        let (s, _rx) = source();
        let mut r = s.reader();
        let s2 = s.clone();
        let t = std::thread::spawn(move || r.read(&mut [0u8; 4]).map_err(|e| e.kind()));
        std::thread::sleep(Duration::from_millis(50));
        s2.close();
        assert_eq!(t.join().unwrap(), Err(io::ErrorKind::BrokenPipe));
    }

    #[test]
    fn eviction_keeps_what_readers_need() {
        let (s, rx) = source();
        let mut r = s.reader();
        let chunk = vec![5u8; 1 << 20];
        // Read as fast as it is pushed, far past the cap: memory stays under it.
        for _ in 0..(MAX_CACHED >> 20) * 2 {
            if s.push(&chunk).is_err() {
                panic!("a reader keeping up never fills the cache");
            }
            let mut b = vec![0u8; 1 << 20];
            r.read_exact(&mut b).unwrap();
        }
        assert!(s.lock().cached <= MAX_CACHED);
        assert!(rx.try_recv().is_err());
    }
}
