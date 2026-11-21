use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};

use brimstone_core::runtime::{Context, embed};

/// How long one run of a test may take before the watchdog interrupts it, unless
/// `--test-timeout-secs` or `BS_TEST_TIMEOUT_SECS` says otherwise.
///
/// 300 so that the slow-but-correct tests of the real suites keep their result: the four regexp
/// `S7.8.5_A*_T2.js` cases (217-300 s for their two passes together in a debug build, they `eval` a
/// literal per code unit), `DurationFormat/prototype/format/style-short-en.js` (67 s) and
/// `Temporal/PlainYearMonth/prototype/add/leap-months-hebrew.js` (64 s). Anything that finished on
/// the fork point must not become a TIMEOUT here; a test that really hangs loses its worker for
/// five minutes per pass, and the run still goes on.
pub const DEFAULT_TEST_TIMEOUT_SECS: u64 = 300;

/// How often the watchdog looks at the deadlines it is watching. Timeouts are seconds long, so
/// this only bounds how late an interrupt is requested.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One test that is currently running against a deadline.
struct Watched {
    /// Identifies this entry, so that a finished test removes exactly its own entry.
    id: u64,
    deadline: Instant,
    interrupt: embed::InterruptHandle,
    /// Set by the watchdog before it requests the interrupt, read by the worker once the test has
    /// stopped. Shared with the harness's host so that `--embed-host` also stops on a timeout.
    fired: Arc<AtomicBool>,
}

struct Shared {
    entries: Mutex<Vec<Watched>>,
    running: AtomicBool,
    next_id: AtomicU64,
}

/// Watches the wall time of every test of one run and stops the ones that run over.
///
/// A looping JS test cannot be cancelled from another Rust thread safely, so the watchdog never
/// touches the worker thread. It only sets the engine's interrupt flag
/// (`embed::InterruptHandle`), which the VM checks at every loop back edge and JS function entry.
/// The test's own thread then unwinds its frames, returns a TIMEOUT result and goes on to the next
/// test, so one looping test cannot hold up the suite or the verification step that runs it.
///
/// The interrupt is only polled while the VM is running a script. A test stuck inside a native that
/// never returns (a regexp match that does not re-enter JS, say) is not reached by it and still
/// holds its worker for the rest of the run.
pub struct Watchdog {
    timeout: Duration,
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Watchdog {
    /// Start watching tests, each allowed `timeout` of wall time. A zero timeout disables watching
    /// and spawns no thread.
    pub fn new(timeout: Duration) -> Watchdog {
        let enabled = timeout != Duration::ZERO;

        let shared = Arc::new(Shared {
            entries: Mutex::new(vec![]),
            running: AtomicBool::new(enabled),
            next_id: AtomicU64::new(0),
        });

        let thread = enabled.then(|| {
            let shared = shared.clone();
            std::thread::spawn(move || {
                while shared.running.load(Ordering::Relaxed) {
                    std::thread::sleep(POLL_INTERVAL);

                    let now = Instant::now();
                    // A dead watchdog is invisible from the outside: every test would simply run to
                    // its own end, which for a looping one means hanging the suite the way it hung
                    // before there was a timeout. So say out loud that the polling stopped.
                    let Ok(entries) = shared.entries.lock() else {
                        eprintln!(
                            "harness watchdog: entry lock poisoned; no test will be interrupted"
                        );
                        shared.running.store(false, Ordering::Relaxed);
                        return;
                    };
                    for entry in entries.iter() {
                        // Mark the test before interrupting it, so that a worker which comes back
                        // can never see the interrupt without the flag that explains it.
                        if now >= entry.deadline && !entry.fired.swap(true, Ordering::Relaxed) {
                            entry.interrupt.request();
                        }
                    }
                }
            })
        });

        Watchdog { timeout, shared, thread: Mutex::new(thread) }
    }

    /// The wall time each test of this run is allowed, zero when nothing is watched.
    pub fn budget(&self) -> Duration {
        self.timeout
    }

    /// Watch `cx` until whatever started at `started` has used its whole budget, returning None
    /// when nothing is watched. The guard must be dropped before its context is.
    pub fn arm(&self, cx: Context, started: SystemTime) -> Option<Guard> {
        if self.timeout == Duration::ZERO {
            return None;
        }

        // The budget counts from `started` rather than from now, so a run that starts with the
        // budget already gone gets none of it. Each run of a test arms with its own start: a
        // Script-mode test executes its body twice, and the second pass must not be cut short by
        // what the first one spent.
        let elapsed = started.elapsed().unwrap_or_default();
        let remaining = self.timeout.checked_sub(elapsed).unwrap_or_default();

        let fired = Arc::new(AtomicBool::new(false));
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);

        self.shared.entries.lock().unwrap().push(Watched {
            id,
            deadline: Instant::now() + remaining,
            interrupt: embed::interrupt_handle(cx),
            fired: fired.clone(),
        });

        Some(Guard { shared: self.shared.clone(), id, fired })
    }

    /// Stop the watchdog thread. Idempotent, and called by `Drop`, so a run that ends early still
    /// takes its poller with it.
    pub fn stop(&self) {
        self.shared.running.store(false, Ordering::Relaxed);

        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A single test under a deadline. See `Watchdog::arm`.
pub struct Guard {
    shared: Arc<Shared>,
    id: u64,
    fired: Arc<AtomicBool>,
}

impl Guard {
    /// Whether the deadline passed and the test's script was interrupted.
    pub fn is_timeout(&self) -> bool {
        self.fired.load(Ordering::Relaxed)
    }

    /// The verdict for a test that has used `elapsed` of its `budget`: timed out only when the
    /// watchdog fired *and* the budget is gone.
    ///
    /// The flag alone is not enough. It is set the first time a poll finds the deadline passed, and
    /// the poller sleeps 25 ms between looks, so a test that finished a hair inside its deadline is
    /// marked before its worker gets to this guard. Such a test had its budget, and its own answer
    /// stands; only a test that really spent the whole budget is a TIMEOUT.
    pub fn timed_out(&self, elapsed: Duration, budget: Duration) -> bool {
        self.is_timeout() && elapsed >= budget
    }

    /// The flag that says whether this test ran out of time, for the host installed by
    /// `--embed-host`: a host answers interrupts itself, and must answer a timeout by stopping.
    pub fn fired_flag(&self) -> Arc<AtomicBool> {
        self.fired.clone()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.shared
            .entries
            .lock()
            .unwrap()
            .retain(|entry| entry.id != self.id);
    }
}

/// A host that does nothing but refuses the interrupts the watchdog raises for a timed-out test.
pub struct TimeoutHost(Arc<AtomicBool>);

impl TimeoutHost {
    /// Takes the flag of the test's `Guard`, set once that test runs out of time.
    pub fn new(fired: Arc<AtomicBool>) -> TimeoutHost {
        TimeoutHost(fired)
    }
}

impl embed::Host for TimeoutHost {
    fn interrupt(&self, _: Context) -> bool {
        !self.0.load(Ordering::Relaxed)
    }
}
