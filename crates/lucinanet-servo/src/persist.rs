//! Crash-safe, coalesced saving of a profile file, shared by the stores the engine keeps (`visits.rs` for
//! `history.json`, `marks.rs` for `bookmarks.json`). The store tells [`Saver`] that it changed and hands it its
//! state when the clock ticks; the saver decides when the file is due, clones the state and gives the snapshot to a
//! short-lived worker thread that serialises it and does the fsyncs, so the engine thread never waits on the disk.
//! Nothing here knows what is stored; the writer closure does (usually `zoom::write_atomic` of the store's JSON).

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a worker does with a snapshot of the state: put it in the file at `path`.
pub type Writer<T> = Arc<dyn Fn(&Path, &T) -> std::io::Result<()> + Send + Sync>;

/// The history file is written at most this often (microseconds), and at exit. The bookmarks use a shorter interval.
pub const SAVE_EVERY_US: i64 = 10_000_000;

/// Whether the file is due: something changed and a full interval has passed since the last write (or, with no write
/// yet, since the first change). This is the whole coalescing rule.
pub fn save_due(dirty_since_us: Option<i64>, saved_us: Option<i64>, now_us: i64, every_us: i64) -> bool {
    let Some(dirty) = dirty_since_us else { return false };
    now_us.saturating_sub(saved_us.unwrap_or(dirty)) >= every_us
}

/// One file and the rules for writing it.
pub struct Saver<T> {
    /// `<profile>/<name>`; None with `LUCINANET_PROFILE_DIR=none`, and then nothing is ever written.
    file: Option<PathBuf>,
    /// Names the store in the messages on stderr.
    label: &'static str,
    every_us: i64,
    dirty_since_us: Option<i64>,
    saved_us: Option<i64>,
    writer: Writer<T>,
    /// The write on its worker thread, at most one.
    in_flight: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

impl<T: Clone + Send + 'static> Saver<T> {
    pub fn new(file: Option<PathBuf>, label: &'static str, every_us: i64, writer: Writer<T>) -> Saver<T> {
        Saver { file, label, every_us, dirty_since_us: None, saved_us: None, writer, in_flight: None }
    }

    /// The state changed at `now_us`.
    pub fn changed(&mut self, now_us: i64) {
        self.dirty_since_us.get_or_insert(now_us);
    }

    /// Whether a change is waiting to be written.
    pub fn is_dirty(&self) -> bool {
        self.dirty_since_us.is_some()
    }

    /// The loop's clock. It only decides: when the file is due and no write is in flight, a snapshot of `state` goes to a
    /// short-lived worker thread. While a write is in flight the state stays dirty and is due again when it ends.
    pub fn tick(&mut self, now_us: i64, state: &T) {
        self.reap(now_us, false);
        if self.in_flight.is_none() && save_due(self.dirty_since_us, self.saved_us, now_us, self.every_us) {
            self.spawn(now_us, state);
        }
    }

    /// Write what is left and wait for it (exit, and the command channel closing). A write in flight is joined first; then,
    /// if anything changed since its snapshot, the latest state is written here, so the file is complete before `_exit`.
    pub fn flush(&mut self, now_us: i64, state: &T) {
        self.reap(now_us, true);
        if self.dirty_since_us.is_some() {
            self.dirty_since_us = None;
            self.saved_us = Some(now_us);
            if let Some(path) = &self.file
                && let Err(error) = (self.writer)(path, state)
            {
                eprintln!("lucinanet: {} not saved: {error}", self.label);
            }
        }
    }

    /// Take the finished write's result (waiting for it when `wait`); a failed one leaves the state dirty to try again.
    fn reap(&mut self, now_us: i64, wait: bool) {
        if !self.in_flight.as_ref().is_some_and(|h| wait || h.is_finished()) {
            return;
        }
        let Some(handle) = self.in_flight.take() else { return };
        match handle.join() {
            Ok(Ok(())) => {},
            Ok(Err(error)) => {
                eprintln!("lucinanet: {} not saved: {error}", self.label);
                self.dirty_since_us.get_or_insert(now_us);
            },
            Err(_) => {
                eprintln!("lucinanet: {} writer panicked", self.label);
                self.dirty_since_us.get_or_insert(now_us);
            },
        }
    }

    fn spawn(&mut self, now_us: i64, state: &T) {
        self.dirty_since_us = None;
        self.saved_us = Some(now_us);
        let Some(path) = self.file.clone() else { return };
        let (snapshot, writer) = (state.clone(), self.writer.clone());
        let name = format!("{}-writer", self.label);
        match std::thread::Builder::new().name(name).spawn(move || writer(&path, &snapshot)) {
            Ok(handle) => self.in_flight = Some(handle),
            Err(error) => {
                eprintln!("lucinanet: {} writer not started: {error}", self.label);
                self.dirty_since_us = Some(now_us);
            },
        }
    }

    /// Point the saver at `file`, for tests with a fake writer.
    #[cfg(test)]
    pub fn with_file(mut self, file: PathBuf) -> Saver<T> {
        self.file = Some(file);
        self
    }

    /// Wait for the write in flight, for tests.
    #[cfg(test)]
    pub fn settle(&mut self) {
        self.reap(0, true);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, mpsc};

    use super::*;

    #[test]
    fn the_file_is_due_once_per_interval() {
        let e = 10;
        assert!(!save_due(None, Some(0), 1_000, e), "nothing changed");
        assert!(!save_due(Some(100), None, 105, e), "first change, interval not over");
        assert!(save_due(Some(100), None, 110, e));
        assert!(!save_due(Some(200), Some(195), 204, e), "written 9 ago");
        assert!(save_due(Some(200), Some(195), 205, e));
        assert!(!save_due(Some(200), Some(1_000), 900, e), "a clock that went back never writes in a burst");
    }

    /// A writer that waits for a token per write and logs what each snapshot held.
    fn gated() -> (Writer<Vec<u32>>, Arc<Mutex<Vec<Vec<u32>>>>, mpsc::Sender<()>) {
        let (release, gate) = mpsc::channel::<()>();
        let (gate, log) = (Mutex::new(gate), Arc::new(Mutex::new(Vec::new())));
        let seen = log.clone();
        let writer: Writer<Vec<u32>> = Arc::new(move |_, state| {
            gate.lock().unwrap().recv().ok();
            seen.lock().unwrap().push(state.clone());
            Ok(())
        });
        (writer, log, release)
    }

    #[test]
    fn a_write_in_flight_does_not_block_and_takes_the_state_it_began_with() {
        let (writer, log, release) = gated();
        let mut s = Saver::new(Some(PathBuf::from("x.json")), "test", 10, writer);
        let mut state = vec![1];
        s.changed(0);
        s.tick(5, &state);
        assert!(s.is_dirty(), "not due inside the interval");
        s.tick(10, &state);
        // The worker is parked on the gate; the caller went on and the state changed.
        state.push(2);
        s.changed(11);
        s.tick(40, &state);
        assert!(log.lock().unwrap().is_empty(), "nothing written yet, no second write started");
        release.send(()).unwrap();
        s.settle();
        assert_eq!(*log.lock().unwrap(), [vec![1]]);
        assert!(s.is_dirty(), "what changed meanwhile is still due");
        s.tick(60, &state);
        release.send(()).unwrap();
        s.settle();
        assert_eq!(*log.lock().unwrap(), [vec![1], vec![1, 2]]);
    }

    #[test]
    fn flush_lands_the_write_in_flight_then_the_latest_state_and_then_nothing() {
        let (writer, log, release) = gated();
        let mut s = Saver::new(Some(PathBuf::from("x.json")), "test", 10, writer);
        release.send(()).unwrap();
        release.send(()).unwrap();
        s.changed(0);
        s.tick(10, &vec![1]);
        s.changed(11);
        s.flush(20, &vec![1, 2]);
        assert_eq!(*log.lock().unwrap(), [vec![1], vec![1, 2]]);
        s.flush(i64::MAX / 2, &vec![1, 2, 3]);
        assert_eq!(log.lock().unwrap().len(), 2, "nothing left to write");
    }

    #[test]
    fn no_file_means_no_write_and_no_thread() {
        let (writer, log, release) = gated();
        let mut s = Saver::new(None, "test", 10, writer);
        release.send(()).unwrap();
        s.changed(0);
        s.tick(100, &vec![1]);
        s.flush(200, &vec![1]);
        assert!(log.lock().unwrap().is_empty());
        assert!(!s.is_dirty());
    }
}
