/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The rasterizing threads of a context (raster/DESIGN.md, "Parallel rasterizing"): `RasterWorker#1..` live as long
//! as the pool, and [`Pool::run`] hands each a share of one draw and waits for all of them. New; swgl has one thread.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// The most threads a context draws with: the default is the machine's cores, capped here.
pub const DEFAULT_MAX_THREADS: usize = 8;

/// How long [`Pool::run`] waits for the next worker's answer before it prints one warning line (DESIGN.md,
/// "Parallel rasterizing"). Only a diagnostic: the wait itself stays unbounded.
const SLOW_WAIT: Duration = Duration::from_secs(1);

/// `LUCINANET_RASTER_THREADS`, read once: the threads a new context draws with (1 is the one-thread path).
pub fn default_threads() -> usize {
    static THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *THREADS.get_or_init(|| {
        std::env::var("LUCINANET_RASTER_THREADS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n >= 1)
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()).min(DEFAULT_MAX_THREADS))
            .min(lucinanet_raster_rules::MAX_THREADS as usize)
    })
}

/// A worker's answer: its thread index and how its `f(t)` ended.
type Answer = (usize, std::thread::Result<()>);

/// A job: thread index `t` runs `f(t)`. The `'static` is a lie told to the channel; [`Pool::run`] waits for every
/// worker to finish with `f` before it returns, so `f` outlives each call.
struct Job {
    f: &'static (dyn Fn(usize) + Sync),
    t: usize,
    done: Sender<Answer>,
}

struct Worker {
    jobs: Option<Sender<Job>>,
    handle: Option<JoinHandle<()>>,
    /// The worker's kernel thread id, published once by the worker before its first job (0 = unknown). Read only
    /// by the slow-wait warning.
    tid: Arc<AtomicU32>,
}

/// `threads - 1` workers; the calling thread is thread 0 of every [`Pool::run`].
pub struct Pool {
    workers: Vec<Worker>,
}

/// Thread index `t` as a bit of the masks `run` keeps; workers from index 128 up are not tracked (no bit) and the
/// warning then lists them as pending.
fn bit(t: usize) -> u128 {
    if t < 128 {
        1 << t
    } else {
        0
    }
}

impl Pool {
    pub fn new(threads: usize) -> Pool {
        let workers = (1..threads.max(1))
            .map(|n| {
                let (tx, rx): (Sender<Job>, Receiver<Job>) = channel();
                let tid = Arc::new(AtomicU32::new(0));
                let tid_w = Arc::clone(&tid);
                let handle = std::thread::Builder::new()
                    .name(format!("RasterWorker#{n}"))
                    .spawn(move || {
                        tid_w.store(current_tid(), Ordering::Relaxed);
                        drop(tid_w);
                        for job in rx {
                            // `f` goes out of scope with the block, before the answer: once `run` has it, the
                            // caller's closure may be gone.
                            let (t, r, done) = {
                                let Job { f, t, done } = job;
                                (t, std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(t))), done)
                            };
                            let _ = done.send((t, r));
                        }
                    })
                    .expect("spawning a raster worker");
                Worker { jobs: Some(tx), handle: Some(handle), tid }
            })
            .collect();
        Pool { workers }
    }

    /// The threads a draw is split across, the caller's included.
    pub fn threads(&self) -> usize {
        self.workers.len() + 1
    }

    /// Runs `f(0)` here and `f(t)` on worker `t` for `t` in `1..threads()`, and returns when all have returned. A
    /// panic on any thread is resumed here, after all have finished. If no answer comes for [`SLOW_WAIT`], one line
    /// naming the pending workers goes to stderr (once per call); the wait goes on, unbounded.
    pub fn run(&self, f: &(dyn Fn(usize) + Sync)) {
        // SAFETY: only the lifetime is extended. Every job's `f` is dropped before its `done` message is sent
        // (also after a panic, which `catch_unwind` stops), and this function returns or unwinds only after
        // receiving all `sent` messages, so no call to `f` outlives this function. A timeout is not a message: it
        // only prints the warning (which cannot unwind, see `warn_slow_wait`) and the loop waits again.
        let fs: &'static (dyn Fn(usize) + Sync) = unsafe { core::mem::transmute(f) };
        let (done, results) = channel::<Answer>();
        let mut sent = 0;
        let mut sent_mask: u128 = 0;
        for (i, w) in self.workers.iter().enumerate() {
            let job = Job { f: fs, t: i + 1, done: done.clone() };
            if w.jobs.as_ref().is_some_and(|j| j.send(job).is_ok()) {
                sent += 1;
                sent_mask |= bit(i + 1);
            }
        }
        drop(done);
        let here = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(0)));
        // Collect every payload without dropping any: a payload's Drop may itself panic, and dropping one here
        // would unwind out of `run` while workers still call `f`.
        let mut payloads: Vec<Box<dyn std::any::Any + Send>> = Vec::new();
        if let Err(p) = here {
            payloads.push(p);
        }
        let mut answered: u128 = 0;
        let mut warned = false;
        let mut got = 0;
        while got < sent {
            let answer = if warned {
                results.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                results.recv_timeout(SLOW_WAIT)
            };
            match answer {
                Ok((t, r)) => {
                    got += 1;
                    answered |= bit(t);
                    if let Err(p) = r {
                        payloads.push(p);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    got += 1;
                    payloads.push(Box::new("raster worker gone"));
                }
                // Not an answer: warn once and wait again.
                Err(RecvTimeoutError::Timeout) => {
                    warned = true;
                    warn_slow_wait(&self.workers, sent_mask & !answered);
                }
            }
        }
        assert!(sent == self.workers.len(), "a raster worker is gone");
        if !payloads.is_empty() {
            let first = payloads.swap_remove(0);
            std::mem::forget(payloads);
            std::panic::resume_unwind(first);
        }
    }
}

/// Prints the one slow-wait line for the workers whose bit is in `pending` (and every untracked one, index 128 up).
/// It never unwinds: everything runs under `catch_unwind`, and a caught payload is forgotten, not dropped (its Drop
/// could panic), so `run` keeps waiting for its answers whatever happens here.
#[cold]
#[inline(never)]
fn warn_slow_wait(workers: &[Worker], pending: u128) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        use std::fmt::Write;
        let mut line = format!("raster: pool draw has waited over {} ms for worker(s)", SLOW_WAIT.as_millis());
        let pending_at = |i: usize| bit(i + 1) == 0 || pending & bit(i + 1) != 0;
        let mut first = true;
        for i in (0..workers.len()).filter(|&i| pending_at(i)) {
            let _ = write!(line, "{}{}", if first { " " } else { ", " }, i + 1);
            first = false;
        }
        let _ = write!(line, "; caller on CPU {}", caller_cpu());
        for (i, w) in workers.iter().enumerate().filter(|&(i, _)| pending_at(i)) {
            let tid = w.tid.load(Ordering::Relaxed);
            let (state, cpu) = task_state(tid).unwrap_or_else(|| ("?".into(), "?".into()));
            let _ = write!(line, "; RasterWorker#{} tid {tid} state {state} cpu {cpu}", i + 1);
        }
        emit(&line);
    }));
    if let Err(p) = r {
        std::mem::forget(p);
    }
}

/// Where the warning goes: stderr (errors ignored), and in tests also the calling thread's capture list.
fn emit(line: &str) {
    #[cfg(test)]
    tests::EMITTED.with(|e| e.borrow_mut().push(line.to_owned()));
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// The calling thread's kernel thread id, from `/proc/thread-self/stat` (0 = unknown). Read once per worker.
fn current_tid() -> u32 {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/thread-self/stat")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse().ok())
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// The CPU the calling thread runs on, or `?`.
fn caller_cpu() -> String {
    #[cfg(target_os = "linux")]
    {
        extern "C" {
            fn sched_getcpu() -> i32;
        }
        // SAFETY: `sched_getcpu` takes no arguments and only returns a number (-1 on error).
        let cpu = unsafe { sched_getcpu() };
        if cpu >= 0 {
            return cpu.to_string();
        }
    }
    "?".into()
}

/// State (field 3) and last CPU (field 39) of thread `tid` of this process, from `/proc/self/task/<tid>/stat`.
fn task_state(tid: u32) -> Option<(String, String)> {
    if tid == 0 || !cfg!(target_os = "linux") {
        return None;
    }
    parse_stat(&std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).ok()?)
}

/// Fields 3 and 39 of a `/proc/.../stat` line. The name (field 2) may hold spaces and `)`, so fields are counted
/// after the last `)`.
fn parse_stat(stat: &str) -> Option<(String, String)> {
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.to_owned();
    let cpu = fields.nth(35)?.to_owned();
    Some((state, cpu))
}

impl Drop for Pool {
    fn drop(&mut self) {
        for w in &mut self.workers {
            w.jobs = None;
        }
        for w in &mut self.workers {
            if let Some(h) = w.handle.take() {
                let _ = h.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::Instant;

    thread_local! {
        /// The warning lines `emit` printed on this thread.
        pub(super) static EMITTED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn take_emitted() -> Vec<String> {
        EMITTED.with(|e| std::mem::take(&mut *e.borrow_mut()))
    }

    #[test]
    fn run_calls_f_for_every_thread() {
        let pool = Pool::new(4);
        let seen = std::sync::Mutex::new(std::collections::BTreeSet::new());
        pool.run(&|t| {
            seen.lock().unwrap().insert(t);
        });
        assert_eq!(*seen.lock().unwrap(), (0..pool.threads()).collect());
    }

    #[test]
    fn run_resumes_panic_only_after_every_worker_finished() {
        struct D;
        impl Drop for D {
            fn drop(&mut self) {
                panic!("a panic payload must not be dropped inside run");
            }
        }
        let pool = Pool::new(3);
        let counter = AtomicUsize::new(0);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pool.run(&|t| {
                if t > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    counter.fetch_add(1, Ordering::SeqCst);
                    std::panic::panic_any(D);
                }
            });
        }));
        assert!(r.is_err());
        std::mem::forget(r); // the payload's Drop panics; it must not run here either
        assert_eq!(counter.load(Ordering::SeqCst), pool.threads() - 1);
    }

    #[test]
    fn run_warns_once_about_the_slow_worker_and_still_waits_for_it() {
        let pool = Pool::new(3);
        take_emitted();
        let fast_done = AtomicBool::new(false);
        let finished = AtomicUsize::new(0);
        pool.run(&|t| {
            match t {
                1 => std::thread::sleep(Duration::from_millis(1300)),
                2 => fast_done.store(true, Ordering::SeqCst),
                // The caller lets worker 2 finish first, so only worker 1 can be pending at the timeout.
                _ => {
                    let until = Instant::now() + Duration::from_millis(500);
                    while !fast_done.load(Ordering::SeqCst) && Instant::now() < until {
                        std::thread::yield_now();
                    }
                }
            }
            finished.fetch_add(1, Ordering::SeqCst);
        });
        // `run` returned only after the slow worker answered.
        assert_eq!(finished.load(Ordering::SeqCst), 3);
        let lines = take_emitted();
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        assert!(line.contains("for worker(s) 1;"), "{line}");
        assert!(line.contains("RasterWorker#1 tid "), "{line}");
        assert!(!line.contains("RasterWorker#2"), "{line}");
        if cfg!(target_os = "linux") {
            assert!(!line.contains("tid 0 ") && line.contains("state S cpu "), "{line}");
            assert!(!line.contains("caller on CPU ?"), "{line}");
        }
    }

    #[test]
    fn run_without_a_slow_worker_warns_nothing() {
        let pool = Pool::new(3);
        take_emitted();
        for _ in 0..20 {
            pool.run(&|t| {
                if t > 0 {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
        }
        assert!(take_emitted().is_empty());
    }

    #[test]
    fn parse_stat_counts_fields_after_the_last_paren() {
        let mut stat = String::from("4242 (Raster) (Worker#1) S");
        for n in 4..=52 {
            stat.push_str(&format!(" {n}"));
        }
        assert_eq!(parse_stat(&stat), Some(("S".into(), "39".into())));
        assert_eq!(parse_stat("4242 (x) R 4 5"), None);
        assert_eq!(parse_stat("no paren"), None);
    }
}
