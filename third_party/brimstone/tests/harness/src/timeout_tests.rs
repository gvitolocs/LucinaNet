//! Tests of the harness itself: the per-test wall clock that stops a looping test, and the
//! test262 test that the frozen mocked clock used to hang the whole suite on.
//!
//! The fixture suite lives in `test_fixtures/js`: one test that loops forever and one that
//! finishes, indexed through the same machinery the harness uses for real suites.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant, SystemTime},
};

use brimstone_core::runtime::{
    ContextBuilder,
    embed::{self, Host},
};

use crate::{
    ignored::IgnoredIndex,
    index::TestIndex,
    manifest::{Suite, SuiteFilter, TestManifest},
    runner::{RunnerConfig, TestResultCompletion, TestResults, TestRunner},
    timeout::{TimeoutHost, Watchdog},
};

/// The unix time the mocked clock of these tests starts at, in nanoseconds.
const MOCK_BASE_NANOS: u128 = 1_000_000_000_000_000_000;

/// More counting than any timeout used here allows (of the order of ten minutes), written as a
/// bound rather than `while (true)` so that a regression fails an assertion instead of hanging.
const COUNT_PAST_THE_BUDGET: &str = "let i = 0; while (i < 400e9) { i += 1; }";

/// A test that loops forever must be stopped at its deadline, reported as a timeout, and must not
/// take the rest of the run with it.
#[test]
fn looping_test_is_a_timeout_and_the_run_continues() {
    let timeout = Duration::from_secs(2);

    let started = Instant::now();
    let results = run_fixtures(timeout);
    let run_time = started.elapsed();

    assert_eq!(
        outcome(&results, "loop_forever.js"),
        Some(TestResultCompletion::Timeout),
        "the looping fixture was not reported as a timeout"
    );
    assert_eq!(
        outcome(&results, "runs_fine.js"),
        Some(TestResultCompletion::Success),
        "the test after the looping fixture did not run and pass"
    );
    assert!(!results.is_successful(), "a timed out test must count against the run");

    let looped = time_of(&results, "loop_forever.js");

    // The watchdog polls every 25 ms and the interrupt lands on a loop back edge, so the reported
    // time sits just past the deadline. Well past it means the watchdog never reached this test;
    // under it means the test was cut off before it had its budget.
    assert!(looped >= timeout, "interrupted after only {looped:?}");
    assert!(looped < timeout + timeout / 2, "interrupted only after {looped:?}");
    assert!(run_time < timeout * 2, "the run did not go on after the timeout: {run_time:?}");
}

/// With the timeout disabled every fixture just runs, which is how the looping one behaved before
/// the watchdog existed: the run cannot finish, so this test only covers the test that does.
#[test]
fn a_run_without_a_timeout_reports_normally() {
    // Only the test that terminates, since without a deadline the looping one would never return.
    let (manifest, index, ignored, suite_filter) = fixture_runner_inputs();

    let mut runner = TestRunner::new(
        manifest,
        index,
        ignored,
        RunnerConfig {
            suite_filter,
            num_threads: 1,
            filter: Some("runs_fine.js".to_owned()),
            feature: None,
            test_timeout: Duration::ZERO,
        },
    );
    let results = runner.run(false);

    assert_eq!(outcome(&results, "runs_fine.js"), Some(TestResultCompletion::Success));
    assert!(results.is_successful());
}

/// The budget belongs to a test, not to the run: a test that starts once another has spent its
/// whole budget still gets all of its own. Read off the watchdog directly, since the only way to
/// see it through the runner is to burn two budgets of wall time.
#[test]
fn every_test_gets_its_own_budget() {
    let budget = Duration::from_millis(300);
    let watchdog = Watchdog::new(budget);

    // A test that starts with its budget already gone, which is what a run-wide deadline would do
    // to every test after the first.
    let spent_start = SystemTime::now();
    std::thread::sleep(budget * 2);

    let spent = ContextBuilder::new()
        .mock_unix_time_nanos(MOCK_BASE_NANOS)
        .build()
        .unwrap();
    let spent_guard = watchdog.arm(spent, spent_start).unwrap();
    std::thread::sleep(budget / 3);

    assert!(
        spent_guard.is_timeout(),
        "a test that started over budget was never interrupted"
    );
    assert!(
        !TimeoutHost::new(spent_guard.fired_flag()).interrupt(spent),
        "the host of a timed out test told the VM to carry on"
    );

    let fresh = ContextBuilder::new()
        .mock_unix_time_nanos(MOCK_BASE_NANOS)
        .build()
        .unwrap();
    let fresh_guard = watchdog.arm(fresh, SystemTime::now()).unwrap();

    assert!(!fresh_guard.is_timeout(), "a test that has not used its budget was interrupted");
    assert!(
        TimeoutHost::new(fresh_guard.fired_flag()).interrupt(fresh),
        "the host of a test within its budget let it carry on"
    );

    watchdog.stop();
    drop(spent_guard);
    drop(fresh_guard);
    spent.drop();
    fresh.drop();
}

/// A mark from the watchdog is not by itself a TIMEOUT: the poll that sets it can run while a test
/// that finished just inside its deadline is still on its way out of the watched list, and that
/// test did have its budget. Only a spent budget turns the mark into a timeout.
///
/// The elapsed times are handed in rather than read from the clock, since the boundary the runner
/// has to get right is a few milliseconds wide and a real one would depend on how the box happens
/// to be loaded. The fire is raised by hand too, against a deadline an hour out, so that nothing
/// but this test puts the flag up.
#[test]
fn a_fire_inside_the_budget_is_not_a_timeout() {
    let budget = Duration::from_secs(3600);
    let watchdog = Watchdog::new(budget);

    let cx = ContextBuilder::new()
        .mock_unix_time_nanos(MOCK_BASE_NANOS)
        .build()
        .unwrap();
    let guard = watchdog.arm(cx, SystemTime::now()).unwrap();

    assert!(
        !guard.timed_out(budget * 2, watchdog.budget()),
        "a test the watchdog never marked was reported as a timeout"
    );

    guard.fired_flag().store(true, Ordering::Relaxed);
    assert!(guard.is_timeout(), "the hand-raised mark did not reach the guard");

    assert!(
        !guard.timed_out(budget / 2, watchdog.budget()),
        "a test that had used half its budget was reported as a timeout"
    );
    assert!(
        guard.timed_out(budget, watchdog.budget()),
        "a test that spent exactly its budget was not reported as a timeout"
    );
    assert!(
        guard.timed_out(budget * 2, watchdog.budget()),
        "a test well past its budget was not reported as a timeout"
    );

    watchdog.stop();
    drop(guard);
    cx.drop();
}

/// Each run of a test gets the whole budget, not what the previous run left. A Script-mode test
/// executes its body twice (`run_full_test`), and against one shared budget the second pass of the
/// slow regexp tests started out of time: `S7.8.5_A1.4_T2.js` spent all 300 s on the default and
/// was reported a TIMEOUT for a test that passes on the fork point.
#[test]
fn each_run_of_a_two_run_test_gets_the_whole_budget() {
    let budget = Duration::from_secs(1);
    let watchdog = Watchdog::new(budget);
    let test_start = SystemTime::now();

    // The first run, taking half its budget and finishing inside it.
    let first_cx = ContextBuilder::new()
        .mock_unix_time_nanos(MOCK_BASE_NANOS)
        .build()
        .unwrap();
    let first_start = SystemTime::now();
    let first = watchdog.arm(first_cx, first_start).unwrap();
    std::thread::sleep(budget / 2);

    assert!(
        !first.timed_out(first_start.elapsed().unwrap(), watchdog.budget()),
        "the first run was cut off although it used only half its budget"
    );
    drop(first);
    first_cx.drop();

    // The way the runner used to arm: against the start of the whole test. With half the budget
    // already spent by the first run, the second is past its deadline before it does anything.
    let starved_cx = ContextBuilder::new()
        .mock_unix_time_nanos(MOCK_BASE_NANOS)
        .build()
        .unwrap();
    let starved = watchdog.arm(starved_cx, test_start).unwrap();
    std::thread::sleep(budget * 3 / 4);

    assert!(
        starved.is_timeout(),
        "a run armed against the test's start was left some budget after all"
    );
    drop(starved);
    starved_cx.drop();

    // The way it arms now: against the start of this run, so what the first run spent is not the
    // second one's problem. The test as a whole is already past one budget by this point.
    assert!(
        test_start.elapsed().unwrap() >= budget,
        "the test had not even spent one budget yet, so nothing here was proven"
    );

    let second_cx = ContextBuilder::new()
        .mock_unix_time_nanos(MOCK_BASE_NANOS)
        .build()
        .unwrap();
    let second_start = SystemTime::now();
    let second = watchdog.arm(second_cx, second_start).unwrap();
    std::thread::sleep(budget / 4);

    assert!(
        !second.timed_out(second_start.elapsed().unwrap(), watchdog.budget()),
        "the second run was cut off although it used only a quarter of its own budget"
    );

    watchdog.stop();
    drop(second);
    second_cx.drop();
}

/// The path a `--embed-host` run takes, where the installed host answers interrupts itself: the
/// harness installs a `TimeoutHost` that answers with the guard's fired flag, where the default
/// answer of "carry on" would ride the watchdog out. The host is installed here directly, since
/// what is under test is the answer the harness's host gives and not the flag that selects it.
#[test]
fn embed_host_timeout_fires_on_the_embed_path() {
    let timeout = Duration::from_secs(2);

    let cx = ContextBuilder::new()
        .mock_unix_time_nanos(MOCK_BASE_NANOS)
        .build()
        .unwrap();

    let watchdog = Watchdog::new(timeout);
    let guard = watchdog.arm(cx, SystemTime::now()).unwrap();
    embed::install_host(cx, Box::new(TimeoutHost::new(guard.fired_flag()))).unwrap();

    let started = Instant::now();
    let outcome =
        embed::evaluate_script(cx, cx.initial_realm(), COUNT_PAST_THE_BUDGET, "timeout_test.js", 1);
    let ran = started.elapsed();

    assert!(outcome.is_err(), "the loop ran to its end, so nothing interrupted it");
    assert!(guard.is_timeout(), "the loop stopped without the watchdog having fired");
    assert!(ran >= timeout, "interrupted after only {ran:?} of a {timeout:?} budget");
    assert!(
        ran < timeout * 10,
        "took {ran:?} to stop: the host let the loop run on past its deadline"
    );

    watchdog.stop();
    drop(guard);
    cx.drop();
}

/// `language/expressions/dynamic-import/await-import-evaluation.js` busy-waits in its fixture for
/// 100 ms of `Date.now()`, so on the frozen mocked clock it never ended and blocked every
/// `-s test262 --all language` run. It must now complete, and pass.
#[test]
fn test262_await_import_evaluation_completes() {
    const PATH: &str = "language/expressions/dynamic-import/await-import-evaluation.js";

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let test262_harness = root.join("tests/test262/test262/harness");

    if !test262_harness.join("assert.js").exists() {
        // An absent checkout means nothing ran, so the line that says so has to stand out from a
        // passing test's output, and a gate that promises the checkout gets a failure instead of a
        // quiet pass when it is not there.
        let message = format!(
            "SKIPPED: test262 checkout missing at {} - nothing of this test ran. Install it with \
             tests/test262/install_test262.sh, or set BS_REQUIRE_TEST262=1 to fail instead.",
            test262_harness.display()
        );

        if std::env::var("BS_REQUIRE_TEST262").is_ok_and(|value| value == "1") {
            panic!("{message}");
        }

        println!("{message}");
        return;
    }

    let manifest = TestManifest::load_from_file(&root.join("tests/test_manifest.jsonc")).unwrap();

    // Reuse the index that `cargo btest --reindex` leaves behind when it is there, since building
    // one walks all of test262.
    let index_path = manifest.index_path();
    let index = if index_path.exists() {
        TestIndex::load_from_file(&index_path).unwrap()
    } else {
        TestIndex::new(&manifest).unwrap()
    };

    // `--all`, so that the `slow` set that this test is in is run rather than skipped.
    let mut ignored = HashMap::new();
    for suite in &manifest.suites {
        let path = manifest.manifest_dir.as_path().join(&suite.ignored_file);
        ignored
            .insert(suite.suite, IgnoredIndex::load_from_file(&path, true, false, false).unwrap());
    }

    let mut runner = TestRunner::new(
        manifest,
        index,
        ignored,
        RunnerConfig {
            suite_filter: SuiteFilter::new(Some(vec![Suite::Test262])),
            num_threads: 1,
            filter: Some(PATH.to_owned()),
            feature: None,
            test_timeout: Duration::from_secs(120),
        },
    );
    let results = runner.run(false);

    assert_eq!(
        outcome(&results, PATH),
        Some(TestResultCompletion::Success),
        "{PATH} did not pass"
    );
}

/// The fixture suite, ready for a run.
fn fixture_runner_inputs() -> (TestManifest, TestIndex, HashMap<Suite, IgnoredIndex>, SuiteFilter) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_fixtures");
    let manifest = TestManifest::load_from_file(&dir.join("manifest.json")).unwrap();

    let index = TestIndex::new(&manifest).unwrap();
    assert_eq!(index.tests.len(), 2, "expected exactly the two fixture tests");

    let ignored = HashMap::from([(
        Suite::Integration,
        IgnoredIndex::load_from_file(&dir.join("ignored.json"), true, false, false).unwrap(),
    )]);

    (manifest, index, ignored, SuiteFilter::new(Some(vec![Suite::Integration])))
}

fn run_fixtures(timeout: Duration) -> TestResults {
    let (manifest, index, ignored, suite_filter) = fixture_runner_inputs();

    let mut runner = TestRunner::new(
        manifest,
        index,
        ignored,
        RunnerConfig {
            suite_filter,
            num_threads: 1,
            filter: None,
            feature: None,
            test_timeout: timeout,
        },
    );

    runner.run(false)
}

fn outcome(results: &TestResults, path: &str) -> Option<TestResultCompletion> {
    results
        .outcomes()
        .find(|result| result.path == path)
        .map(|result| result.result.clone())
}

fn time_of(results: &TestResults, path: &str) -> Duration {
    results
        .outcomes()
        .find(|result| result.path == path)
        .unwrap_or_else(|| panic!("{path} did not report a result"))
        .time
}
