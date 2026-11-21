use std::{
    collections::HashMap,
    fs,
    panic::{self, AssertUnwindSafe},
    path::Path,
    rc::Rc,
    sync::{
        Arc, LazyLock, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::channel,
    },
    time::{Duration, SystemTime},
};

use brimstone_core::{
    common::{
        constants::MEGABYTE_BYTES,
        error::FormatOptions,
        options::{Options, OptionsBuilder},
        wtf_8::Wtf8String,
    },
    eval_err,
    parser::{self, ParseContext, ParsedProgram, source::Source},
    runtime::{
        Context, ContextBuilder, EvalResult, Handle, Value,
        bytecode::generator::BytecodeProgramGenerator, embed, eval_result::EvalError, get,
        intrinsics::error_object::ErrorObject, test_262_object::Test262Object, to_console_string,
        to_string,
    },
};
use serde_json::{self, json};
use threadpool::ThreadPool;

/// Whether each test context gets a no-op embedder host (`--embed-host`).
pub static EMBED_HOST: AtomicBool = AtomicBool::new(false);

/// Whether to generate the bytecode of every function up front instead of on first call.
pub static NO_LAZY: AtomicBool = AtomicBool::new(false);

/// Run every test with the structural function cache on, shared by all tests of the run.
pub static FUNCTION_CACHE: AtomicBool = AtomicBool::new(false);

/// Also verify every function cache hit against normal generation.
pub static FUNCTION_CACHE_VERIFY: AtomicBool = AtomicBool::new(false);

/// Keep each script's AST for its lazy functions (no re-parse after the script has run).
pub static NO_REPARSE: AtomicBool = AtomicBool::new(false);

/// Keep the AST, and generate every re-parsed function from it too, comparing the two.
pub static REPARSE_VERIFY: AtomicBool = AtomicBool::new(false);

/// Re-parse statistics summed over the contexts of the run.
static REPARSED: AtomicU64 = AtomicU64::new(0);
static REPARSE_FAILED: AtomicU64 = AtomicU64::new(0);
static REPARSE_MISMATCHES: AtomicU64 = AtomicU64::new(0);
static REPARSE_PROGRAMS_DROPPED: AtomicU64 = AtomicU64::new(0);
static REPARSE_GENERATED_BEFORE_DROP: AtomicU64 = AtomicU64::new(0);

/// Print the re-parse statistics of the run.
pub fn print_reparse_totals() {
    println!(
        "lazy re-parse: programs dropped {} | functions re-parsed {} | failed {} | generated before drop {} | verify mismatches {}",
        REPARSE_PROGRAMS_DROPPED.load(Ordering::Relaxed),
        REPARSED.load(Ordering::Relaxed),
        REPARSE_FAILED.load(Ordering::Relaxed),
        REPARSE_GENERATED_BEFORE_DROP.load(Ordering::Relaxed),
        REPARSE_MISMATCHES.load(Ordering::Relaxed),
    );
}

/// Function cache statistics summed over the contexts of the run.
static FNCACHE_LOOKUPS: AtomicU64 = AtomicU64::new(0);
static FNCACHE_HITS: AtomicU64 = AtomicU64::new(0);
static FNCACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static FNCACHE_UNCACHEABLE: AtomicU64 = AtomicU64::new(0);
static FNCACHE_MISMATCHES: AtomicU64 = AtomicU64::new(0);

/// Print the function cache statistics of the run, if the cache was on.
pub fn print_function_cache_totals() {
    if !FUNCTION_CACHE.load(Ordering::Relaxed) {
        return;
    }
    println!(
        "function cache: lookups {} | hits {} | misses {} | uncacheable {} | verify mismatches {}",
        FNCACHE_LOOKUPS.load(Ordering::Relaxed),
        FNCACHE_HITS.load(Ordering::Relaxed),
        FNCACHE_MISSES.load(Ordering::Relaxed),
        FNCACHE_UNCACHEABLE.load(Ordering::Relaxed),
        FNCACHE_MISMATCHES.load(Ordering::Relaxed),
    );
}

use crate::{
    ignored::IgnoredIndex,
    index::{ExpectedResult, Test, TestIndex, TestMode, TestPhase},
    manifest::{Suite, SuiteFilter, TestManifest},
    table::{CellAlignment, TableCell, TableFormatter, TableRow},
    timeout::{Guard, TimeoutHost, Watchdog},
    utils::GenericResult,
};

/// How a [`TestRunner`] selects and bounds the tests it runs.
pub struct RunnerConfig {
    pub suite_filter: SuiteFilter,
    pub num_threads: u8,
    pub filter: Option<String>,
    pub feature: Option<String>,
    /// Wall time each test is allowed before the watchdog interrupts it. Zero disables the
    /// watchdog entirely.
    pub test_timeout: Duration,
}

pub struct TestRunner {
    manifest: TestManifest,
    index: TestIndex,
    ignored: HashMap<Suite, IgnoredIndex>,
    suite_filter: SuiteFilter,
    thread_pool: ThreadPool,
    filter: Option<String>,
    feature: Option<String>,
    /// Wall time each test is allowed before the watchdog interrupts it. Zero disables the
    /// watchdog entirely.
    test_timeout: Duration,
}

// Runner threads have an 8MB stack
const RUNNER_THREAD_STACK_SIZE: usize = 1 << 23;

/// Size of the heap for each test. Use a small value since most tests do not require a large heap.
const HEAP_SIZE: usize = MEGABYTE_BYTES;

/// Unix time at which the mocked clock of every test starts. Each test's clock advances from here
/// by a fixed tick per read (see `context::MOCKED_UNIX_TIME_STEP_NANOS`), so tests are reproducible
/// without the clock ever standing still.
const MOCKED_UNIX_TIME_START_NANOS: u128 = 1_000_000_000_000_000_000;

impl TestRunner {
    pub fn new(
        manifest: TestManifest,
        index: TestIndex,
        ignored: HashMap<Suite, IgnoredIndex>,
        config: RunnerConfig,
    ) -> TestRunner {
        let RunnerConfig { suite_filter, num_threads, filter, feature, test_timeout } = config;
        let thread_pool = threadpool::Builder::new()
            .num_threads(num_threads.into())
            .thread_stack_size(RUNNER_THREAD_STACK_SIZE)
            .build();
        TestRunner {
            manifest,
            index,
            ignored,
            suite_filter,
            thread_pool,
            filter,
            feature,
            test_timeout,
        }
    }

    pub fn run(&mut self, verbose: bool) -> TestResults {
        let (sender, receiver) = channel::<TestResult>();
        let mut num_jobs = 0;
        let mut num_skipped = self.suite_filter.map_for_suites(|_| 0);
        let mut ignored_failures = vec![];

        // Interrupts any test that goes over its wall-clock budget, so that one looping test
        // cannot hold up the run.
        let watchdog = Arc::new(Watchdog::new(self.test_timeout));

        let all_tests_start_timestamp = SystemTime::now();

        // Precompute the roots for each test suite
        let mut suite_roots = HashMap::new();
        for suite in &self.manifest.suites {
            let suite_root = self.manifest.manifest_dir.as_path().join(&suite.path);
            suite_roots.insert(suite.suite, suite_root.to_str().unwrap().to_owned());
        }

        // Precompute the root for the test262 repo as a string
        let test262_root = self
            .manifest
            .test262_repo_path()
            .to_str()
            .unwrap()
            .to_owned();

        for (i, test) in self.index.tests.values().enumerate() {
            // Skip tests that are in suites that should not be run, but do not count as skipped
            if !self.suite_filter.should_include(test.suite) {
                continue;
            }

            // If a filter was provided then skip all tests that do not match the filter
            if let Some(filter) = &self.filter
                && !test.path.contains(filter)
            {
                *num_skipped.entry(test.suite).or_default() += 1;
                continue;
            }

            // If a feature was specified then skip all tests that do not have that feature
            if let Some(feature) = &self.feature
                && !test.features.contains(feature)
            {
                *num_skipped.entry(test.suite).or_default() += 1;
                continue;
            }

            if self.ignored[&test.suite].should_fail(test) {
                ignored_failures.push(TestResult::failure(
                    test,
                    "Ignored test counted as failure".to_owned(),
                    Duration::ZERO,
                ));
                continue;
            } else if self.ignored[&test.suite].should_ignore(test) {
                *num_skipped.entry(test.suite).or_default() += 1;
                continue;
            }

            let test = test.clone();
            let sender = sender.clone();
            let suite_root = suite_roots[&test.suite].clone();
            let test262_root = test262_root.clone();
            let watchdog = watchdog.clone();

            num_jobs += 1;

            self.thread_pool.execute(move || {
                let start_timestamp = SystemTime::now();

                let panic_result = panic::catch_unwind(|| {
                    if verbose {
                        println!("{i}: {}", test.path);
                    }

                    run_full_test(&test, &suite_root, &test262_root, start_timestamp, &watchdog)
                });

                let duration = start_timestamp.elapsed().unwrap();

                match panic_result {
                    Ok(result) => sender.send(result).unwrap(),
                    Err(err) => {
                        // Attempt to extract string message from panic
                        let message = err.downcast_ref::<&str>();
                        let message = match message {
                            Some(message) => message.to_string(),
                            None => String::from("<panic message not found>"),
                        };

                        let result = TestResult::failure(
                            &test,
                            format!("Thread panicked:\n{message}"),
                            duration,
                        );

                        sender.send(result).unwrap()
                    }
                }
            });
        }

        let mut results: Vec<TestResult> = receiver.iter().take(num_jobs).collect();

        watchdog.stop();

        if !ignored_failures.is_empty() {
            results.extend(ignored_failures);
        }

        TestResults::collate(
            results,
            num_skipped,
            all_tests_start_timestamp.elapsed().unwrap(),
            self.suite_filter.clone(),
        )
    }
}

fn run_full_test(
    test: &Test,
    suite_root: &str,
    test262_root: &str,
    start_timestamp: SystemTime,
    watchdog: &Watchdog,
) -> TestResult {
    match test.mode {
        TestMode::StrictScript => {
            run_single_test(test, suite_root, test262_root, true, start_timestamp, watchdog)
        }
        TestMode::NonStrictScript | TestMode::Module => {
            run_single_test(test, suite_root, test262_root, false, start_timestamp, watchdog)
        }
        // Run in both strict and non strict mode, both must pass for this test to be successful
        TestMode::Script => {
            let non_strict_result =
                run_single_test(test, suite_root, test262_root, false, start_timestamp, watchdog);

            // Raw mode tests for scripts are only run in non-strict mode
            if test.is_raw {
                return non_strict_result;
            }

            if let TestResultCompletion::Success = non_strict_result.result {
                run_single_test(test, suite_root, test262_root, true, start_timestamp, watchdog)
            } else {
                non_strict_result
            }
        }
    }
}

fn run_single_test(
    test: &Test,
    suite_root: &str,
    test262_root: &str,
    force_strict_mode: bool,
    start_timestamp: SystemTime,
    watchdog: &Watchdog,
) -> TestResult {
    // Set up options for test
    let options = OptionsBuilder::new()
        .annex_b(test.is_annex_b)
        .expose_test_262(true)
        .lazy_functions(!NO_LAZY.load(Ordering::Relaxed))
        .lazy_reparse(!NO_REPARSE.load(Ordering::Relaxed))
        .lazy_reparse_verify(REPARSE_VERIFY.load(Ordering::Relaxed))
        .min_heap_size(HEAP_SIZE)
        .gc_verify(std::env::var("BS_GC_VERIFY").is_ok())
        .gc_stress_window(
            std::env::var("BS_GC_STRESS")
                .ok()
                .and_then(|v| v.parse().ok()),
        )
        .build()
        .unwrap();

    // Each test is executed in its own realm
    let cx = ContextBuilder::new()
        .set_options(Rc::new(options))
        .mock_unix_time_nanos(MOCKED_UNIX_TIME_START_NANOS)
        .build()
        .unwrap();
    let options = cx.options.clone();

    // The test runs against the run's wall-clock budget, and is dropped before the context. The
    // budget belongs to this call, not to the test: a Script-mode test arrives here twice, once per
    // mode, and the second pass has to have a budget of its own rather than what the first left.
    // `start_timestamp` stays the start of the whole test, which is what the result reports.
    let phase_start = SystemTime::now();
    let guard = watchdog.arm(cx, phase_start);

    // Optionally run with an embedder host installed, which leaves jobs in the engine's queue but
    // switches on the embedding paths (e.g. promises pinned in the host space)
    if EMBED_HOST.load(Ordering::Relaxed) {
        // A host answers interrupts itself, so hand it the timeout flag; the default answer of
        // "carry on" would ride out the watchdog.
        let fired = guard.as_ref().map(Guard::fired_flag).unwrap_or_default();
        embed::install_host(cx, Box::new(TimeoutHost::new(fired))).unwrap();
    }

    if FUNCTION_CACHE.load(Ordering::Relaxed) {
        embed::set_function_cache_enabled(cx, true);
        embed::set_realm_cache_partition(cx, cx.initial_realm(), b"test262");
        embed::set_function_cache_shared(cx, true);
        embed::set_function_cache_verify(cx, FUNCTION_CACHE_VERIFY.load(Ordering::Relaxed));
    }

    cx.initial_realm().install_optional_globals(cx).unwrap();

    // Wrap in catch_unwind so that we can clean up the context in the event of a panic
    let panic_result = panic::catch_unwind(AssertUnwindSafe(|| {
        #[cfg(feature = "gc_stress_test")]
        {
            let mut cx = cx;
            cx.enable_gc_stress_test();
        }

        // Default harness files are loaded unless running in raw mode
        if !test.is_raw {
            load_harness_test_file(cx, test262_root, "assert.js");
            load_harness_test_file(cx, test262_root, "sta.js");
        }

        // Load all other specified harness files
        for include in &test.includes {
            load_harness_test_file(cx, test262_root, include);
        }

        // Try to parse and analyze file
        let source = new_source(&test.path, suite_root, force_strict_mode).unwrap();
        let program = parse_and_analyze_file(source, options, Some(test));
        let program = match program {
            Ok(program) => Rc::new(program),
            // An error during parse may be a success or failure depending on the expected result of
            // the test.
            Err(HarnessParseError::Parse(err)) => {
                // Do not count IO errors as negative tests results
                let is_parse_error = !matches!(err.error, parser::ParseError::Io(_));

                let duration = start_timestamp.elapsed().unwrap();

                return match test.expected_result {
                    ExpectedResult::Negative { phase: TestPhase::Parse, .. } if is_parse_error => {
                        TestResult::success(test, duration)
                    }
                    _ => TestResult::failure(
                        test,
                        format!("Unexpected error during parsing:\n{err}"),
                        duration,
                    ),
                };
            }
            // An error during analysis may be a success or failure depending on the expected result
            // of the test.
            Err(HarnessParseError::Analyze(err)) => {
                let duration = start_timestamp.elapsed().unwrap();

                return match test.expected_result {
                    ExpectedResult::Negative { phase: TestPhase::Parse, .. } => {
                        TestResult::success(test, duration)
                    }
                    _ => TestResult::failure(
                        test,
                        format!("Unexpected error during analysis:\n{err}"),
                        duration,
                    ),
                };
            }
        };

        let completion = if test.mode == TestMode::Module {
            execute_module_as_bytecode(cx, &program)
        } else {
            execute_script_as_bytecode(cx, &program)
        };

        let duration = start_timestamp.elapsed().unwrap();

        check_expected_completion(cx, test, completion, duration)
    }));

    #[cfg(feature = "handle_stats")]
    println!("{:?}", cx.heap.info().handle_context().handle_stats());

    if FUNCTION_CACHE.load(Ordering::Relaxed) {
        let stats = embed::function_cache_stats(cx);
        FNCACHE_LOOKUPS.fetch_add(stats.lookups, Ordering::Relaxed);
        FNCACHE_HITS.fetch_add(stats.hits, Ordering::Relaxed);
        FNCACHE_MISSES.fetch_add(stats.misses, Ordering::Relaxed);
        FNCACHE_UNCACHEABLE.fetch_add(stats.uncacheable.iter().sum::<u64>(), Ordering::Relaxed);
        FNCACHE_MISMATCHES.fetch_add(stats.verify_mismatches, Ordering::Relaxed);
    }

    let reparse = embed::reparse_stats(cx);
    REPARSED.fetch_add(reparse.reparsed, Ordering::Relaxed);
    REPARSE_FAILED.fetch_add(reparse.reparse_failed, Ordering::Relaxed);
    REPARSE_MISMATCHES.fetch_add(reparse.verify_mismatches, Ordering::Relaxed);
    REPARSE_PROGRAMS_DROPPED.fetch_add(reparse.programs_dropped, Ordering::Relaxed);
    REPARSE_GENERATED_BEFORE_DROP.fetch_add(reparse.generated_before_drop, Ordering::Relaxed);

    // A phase the watchdog interrupted never reached an answer of its own, so the clock decides its
    // outcome whatever its completion was - but only once the budget is actually spent, so that a
    // phase which finished inside it keeps that answer.
    let phase_elapsed = phase_start.elapsed().unwrap_or_default();
    let timed_out = guard
        .as_ref()
        .map(|guard| guard.timed_out(phase_elapsed, watchdog.budget()))
        .unwrap_or(false);
    drop(guard);

    cx.drop();

    if timed_out {
        return TestResult::timeout(test, start_timestamp.elapsed().unwrap_or_default());
    }

    match panic_result {
        Ok(test_result) => test_result,
        Err(err) => panic::resume_unwind(err),
    }
}

/// Cache of harness file contents by file path to avoid re-reading them from disk.
static HARNESS_FILE_CACHE: LazyLock<RwLock<HashMap<String, Wtf8String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn new_harness_source(file_path: &str) -> parser::ParseResult<Rc<Source>> {
    let cached_file = HARNESS_FILE_CACHE.read().unwrap().get(file_path).cloned();

    // Use the file from cache if possible, otherwise read and insert into cache
    let file_contents = match cached_file {
        Some(cached_file) => cached_file,
        None => {
            let file_contents = Source::read_file_to_wtf8_string(file_path)?;

            HARNESS_FILE_CACHE
                .write()
                .unwrap()
                .insert(file_path.to_owned(), file_contents.clone());

            file_contents
        }
    };

    let source = Source::new_for_string(file_path, file_contents)?;

    Ok(Rc::new(source))
}

fn new_source(
    file: &str,
    suite_root: &str,
    force_strict_mode: bool,
) -> parser::ParseResult<Rc<Source>> {
    let full_path = Path::new(suite_root).join(file);
    let mut source = Source::new_from_file(full_path.to_str().unwrap())?;

    // Manually insert use strict directive when forcing strict mode
    if force_strict_mode {
        let mut source_with_directive = Wtf8String::from_str("\"use strict\";\n");
        source_with_directive.push_wtf8_str(&source.contents);

        source.contents = source_with_directive;
    }

    Ok(Rc::new(source))
}

enum HarnessParseError {
    Parse(parser::LocalizedParseError),
    Analyze(parser::LocalizedParseErrors),
}

fn parse_and_analyze_file(
    source: Rc<Source>,
    options: Rc<Options>,
    test: Option<&Test>,
) -> Result<ParsedProgram, HarnessParseError> {
    let is_module = matches!(test, Some(Test { mode: TestMode::Module, .. }));

    ParsedProgram::new(source, |pcx: &ParseContext| {
        let parse_result = if is_module {
            parser::parse_module(pcx, options)
        } else {
            parser::parse_script(pcx, options)
        };
        let parse_result = parse_result.map_err(HarnessParseError::Parse)?;

        parser::analyze::analyze(parse_result).map_err(HarnessParseError::Analyze)
    })
}

fn load_harness_test_file(cx: Context, test262_root: &str, file: &str) {
    let full_path = Path::new(test262_root).join("harness").join(file);
    let source = match new_harness_source(full_path.to_str().unwrap()) {
        Ok(source) => source,
        Err(_) => {
            panic!("Failed to parse test harness file {}", full_path.display());
        }
    };

    let program = match parse_and_analyze_file(source, cx.options.clone(), None) {
        Ok(program) => Rc::new(program),
        Err(_) => {
            panic!("Failed to parse test harness file {}", full_path.display());
        }
    };

    let eval_result = execute_script_as_bytecode(cx, &program);
    if eval_result.is_err() {
        panic!("Failed to evaluate test harness file {}", full_path.display())
    }
}

fn execute_script_as_bytecode(mut cx: Context, program: &Rc<ParsedProgram>) -> EvalResult<()> {
    let realm = cx.initial_realm();
    let generate_result = BytecodeProgramGenerator::generate_from_parsed_script(cx, program, realm);
    let bytecode_script = match generate_result {
        Ok(bytecode_script) => bytecode_script,
        Err(err) => {
            let err_string = cx.alloc_string(&err.to_string())?;
            return eval_err!(err_string.into());
        }
    };

    cx.run_script(bytecode_script)
}

fn execute_module_as_bytecode(mut cx: Context, program: &Rc<ParsedProgram>) -> EvalResult<()> {
    let realm = cx.initial_realm();
    let generate_result = BytecodeProgramGenerator::generate_from_parsed_module(cx, program, realm);
    let module = match generate_result {
        Ok(module) => module,
        Err(err) => {
            let err_string = cx.alloc_string(&err.to_string())?;
            return eval_err!(err_string.into());
        }
    };

    cx.run_module(module)
}

/// Determine whether the evaluation completion for a test corresponds to that test passing or
/// failing, depending on the expected result of the test.
fn check_expected_completion(
    cx: Context,
    test: &Test,
    completion: EvalResult<()>,
    duration: Duration,
) -> TestResult {
    match completion {
        // A normal completion is a success only if the test was expected to not throw
        Ok(_) => match &test.expected_result {
            ExpectedResult::Positive => {
                // Async tests determine success by looking at the output logged to print()
                if test.is_async {
                    // Get the print log stored on the global object
                    let global_object = cx.initial_realm().global_object().as_object();
                    let print_log = Test262Object::get_print_log(cx, global_object);
                    let print_log = match print_log {
                        Ok(log) => log.format().unwrap(),
                        Err(_) => {
                            return TestResult::failure(
                                test,
                                "failed to access print log".to_owned(),
                                duration,
                            );
                        }
                    };

                    // Fail if the print log contains the fail sequence
                    if print_log.contains("Test262:AsyncTestFailure:") {
                        return TestResult::failure(
                            test,
                            format!("Async test failed with the error: {print_log}"),
                            duration,
                        );
                    }

                    // Only succeed if the print log contains the success sequence
                    if !print_log.contains("Test262:AsyncTestComplete") {
                        return TestResult::failure(
                            test,
                            format!(
                                "print log does not contain Test262:AsyncTestComplete, instead contains:\n{print_log}"
                            ),
                            duration,
                        );
                    }
                }

                TestResult::success(test, duration)
            }
            other => TestResult::failure(
                test,
                format!("Test completed without throwing, but expected {other}"),
                duration,
            ),
        },
        // Only embedder hosts terminate uncatchably, which tests never install
        Err(EvalError::Uncatchable) => {
            TestResult::failure(test, "Test terminated uncatchably".to_owned(), duration)
        }
        #[cfg(feature = "alloc_error")]
        Err(EvalError::Alloc(alloc_error)) => TestResult::failure(
            test,
            format!("Test failed due to allocation error: {alloc_error}"),
            duration,
        ),
        // Throw completions succeed if the expected result is negative, expected during runtime,
        // and has the same expected error.
        Err(EvalError::Value(thrown_value)) => match &test.expected_result {
            ExpectedResult::Negative {
                phase: phase @ (TestPhase::Resolution | TestPhase::Runtime),
                type_,
            } if is_error_in_expected_phase(cx, test, *phase) => {
                // Check that the thrown error matches the expected error type
                let is_expected_error = if thrown_value.is_object() {
                    let thrown_object = thrown_value.as_object();
                    if thrown_object.is::<ErrorObject>() {
                        // Check if thrown error type has the same name as the expected error
                        match get(cx, thrown_object, cx.names.name()) {
                            Ok(name_value) if name_value.is_string() => {
                                &name_value.as_string().format().unwrap() == type_
                            }
                            _ => false,
                        }
                    } else if type_ == "Test262Error" {
                        // The Test262Error does not extend the Error type or have a name property,
                        // so check if the message starts with "Test262Error".
                        match to_string(cx, thrown_value) {
                            Ok(message_value) => {
                                message_value.format().unwrap().starts_with("Test262Error")
                            }
                            _ => false,
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                if is_expected_error {
                    TestResult::success(test, duration)
                } else {
                    let thrown_string = to_console_string_test262(cx, thrown_value);
                    TestResult::failure(
                        test,
                        format!(
                            "Test threw the following error during runtime, but expected {}:\n{}",
                            test.expected_result, thrown_string,
                        ),
                        duration,
                    )
                }
            }
            other => {
                let thrown_string = to_console_string_test262(cx, thrown_value);
                TestResult::failure(
                    test,
                    format!(
                        "Test threw the following error during runtime, but expected {other}:\n{thrown_string}",
                    ),
                    duration,
                )
            }
        },
    }
}

fn is_error_in_expected_phase(cx: Context, test: &Test, expected_phase: TestPhase) -> bool {
    if test.mode == TestMode::Module {
        // Verify that resolution errors occur before module resolution is complete, otherwise
        // this must be a runtime error.
        if expected_phase == TestPhase::Resolution {
            !cx.has_finished_module_resolution
        } else {
            expected_phase == TestPhase::Runtime && cx.has_finished_module_resolution
        }
    } else {
        expected_phase == TestPhase::Runtime
    }
}

fn to_console_string_test262(cx: Context, value: Handle<Value>) -> String {
    // Extract message for Test262Error, otherwise print to console normally
    if value.is_object()
        && let Ok(message_value) = to_string(cx, value)
    {
        let message = message_value.format().unwrap();
        if message.starts_with("Test262Error") {
            return message;
        }
    }

    // Default to empty string on OOM instead of crashing
    to_console_string(cx, value, &FormatOptions::default()).unwrap_or_default()
}

pub struct TestResult {
    pub path: String,
    pub suite: Suite,
    pub result: TestResultCompletion,
    // Total time this test took to run
    pub time: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TestResultCompletion {
    Success,
    Failure(String),
    /// Ran over the per-test wall clock (`--test-timeout-secs`) and was interrupted by the
    /// watchdog. Counted as a failure, but listed apart: the test never got to be wrong.
    Timeout,
    #[allow(dead_code)]
    Skipped,
}

impl TestResult {
    fn success(test: &Test, time: Duration) -> TestResult {
        TestResult {
            path: test.path.clone(),
            suite: test.suite,
            result: TestResultCompletion::Success,
            time,
        }
    }

    fn failure(test: &Test, message: String, time: Duration) -> TestResult {
        TestResult {
            path: test.path.clone(),
            suite: test.suite,
            result: TestResultCompletion::Failure(message),
            time,
        }
    }

    fn timeout(test: &Test, time: Duration) -> TestResult {
        TestResult {
            path: test.path.clone(),
            suite: test.suite,
            result: TestResultCompletion::Timeout,
            time,
        }
    }
}

pub struct TestResults {
    /// List of succeeded tests in each test suite
    succeeded: HashMap<Suite, Vec<TestResult>>,
    /// List of failed tests in each test suite
    failed: HashMap<Suite, Vec<TestResult>>,
    /// Tests that ran out of the per-test wall clock, in each test suite. They count as failures
    /// but are listed apart, since their script was cut off rather than found wrong.
    timed_out: HashMap<Suite, Vec<TestResult>>,
    /// Count of skipped tests in each test suite
    num_skipped: HashMap<Suite, u64>,
    /// Total duration of the entire test run
    total_duration: Duration,
    /// Set of suites included in this run
    suite_filter: SuiteFilter,
}

// ANSII codes for pretty printing
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const WHITE: &str = "\x1b[37m";
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

impl TestResults {
    fn collate(
        results: Vec<TestResult>,
        num_skipped: HashMap<Suite, u64>,
        total_duration: Duration,
        suite_filter: SuiteFilter,
    ) -> TestResults {
        let mut collated = TestResults {
            failed: suite_filter.map_for_suites(|_| vec![]),
            succeeded: suite_filter.map_for_suites(|_| vec![]),
            timed_out: suite_filter.map_for_suites(|_| vec![]),
            num_skipped,
            total_duration,
            suite_filter,
        };

        for result in results {
            match result.result {
                TestResultCompletion::Success => {
                    collated
                        .succeeded
                        .entry(result.suite)
                        .or_default()
                        .push(result);
                }
                TestResultCompletion::Failure(_) => {
                    collated
                        .failed
                        .entry(result.suite)
                        .or_default()
                        .push(result);
                }
                TestResultCompletion::Timeout => {
                    collated
                        .timed_out
                        .entry(result.suite)
                        .or_default()
                        .push(result);
                }
                TestResultCompletion::Skipped => {
                    *collated.num_skipped.entry(result.suite).or_default() += 1;
                }
            }
        }

        for tests in collated
            .failed
            .values_mut()
            .chain(collated.succeeded.values_mut())
            .chain(collated.timed_out.values_mut())
        {
            tests.sort_by(|a, b| a.path.cmp(&b.path));
        }

        collated
    }

    /// Every test of the run with its outcome, whatever that outcome was.
    pub fn outcomes(&self) -> impl Iterator<Item = &TestResult> {
        self.succeeded
            .values()
            .chain(self.failed.values())
            .chain(self.timed_out.values())
            .flatten()
    }

    /// Whether any test failed, including the ones that ran out of time.
    pub fn is_successful(&self) -> bool {
        self.failed.values().all(|results| results.is_empty())
            && self.timed_out.values().all(|results| results.is_empty())
    }

    pub fn print_to_console(&self, test262_root: &Path) {
        let test262_prefix = test262_root.join("test");

        for suite in self.suite_filter.iter() {
            for timed_out in &self.timed_out[&suite] {
                println!(
                    "{}{}Timeout{}: ({}) {}after {:.2} s of wall time\n",
                    BOLD,
                    YELLOW,
                    RESET,
                    timed_out.suite,
                    timed_out.path,
                    timed_out.time.as_secs_f64()
                );
            }

            for failed in &self.failed[&suite] {
                if let TestResultCompletion::Failure(message) = &failed.result {
                    // Strip file path from message to reduce length
                    let file_path = test262_prefix.join(&failed.path);
                    let cleaned_message = message.replace(file_path.to_str().unwrap(), "<file>");

                    println!(
                        "{}{}Failed{}: ({}) {}\n{}\n",
                        BOLD, RED, RESET, failed.suite, failed.path, cleaned_message
                    );
                }
            }
        }

        let status = if self.is_successful() {
            format!("{BOLD}{GREEN}Passed{RESET}")
        } else {
            format!("{BOLD}{RED}Failed{RESET}")
        };

        println!(
            "{}: {}Tests completed in {:.2} seconds{}\n",
            status,
            BOLD,
            self.total_duration.as_secs_f64(),
            RESET,
        );

        let mut table_rows = vec![];
        table_rows.push(new_header_row());

        for suite in self.suite_filter.iter() {
            let num_succeeded = &self.succeeded[&suite].len();
            let num_failed = &self.failed[&suite].len();
            let num_timed_out = &self.timed_out[&suite].len();
            let num_skipped = self.num_skipped[&suite];

            table_rows.push(new_suite_row(
                suite,
                *num_succeeded as u64,
                *num_failed as u64,
                *num_timed_out as u64,
                num_skipped,
            ));
        }

        let table = TableFormatter::format(table_rows);
        println!("{table}");
    }

    pub fn print_test262_progress(&self) {
        let succeeded = self
            .succeeded
            .values()
            .map(|results| results.len())
            .sum::<usize>();
        let failed = self
            .failed
            .values()
            .chain(self.timed_out.values())
            .map(|results| results.len())
            .sum::<usize>();

        let total = succeeded + failed;
        let percent = (succeeded as f64 / total as f64) * 100.0;

        println!("\n{BOLD}Test262 progress: {percent:.2}%{RESET}");
        println!("{succeeded}/{total} tests passed");
    }

    pub fn save_to_result_files(&self, result_files_path: String) -> GenericResult {
        let succeeded_paths: Vec<String> = self
            .succeeded
            .iter()
            .flat_map(|(suite, results)| {
                results
                    .iter()
                    .map(move |result| format!("<{}>/{}", suite, &result.path))
            })
            .collect();

        // A test that ran out of time is a failure like any other to everything that reads these
        // files, listed with the paths of the tests that did fail.
        let failed_paths: Vec<String> = self
            .failed
            .iter()
            .chain(self.timed_out.iter())
            .flat_map(|(suite, results)| {
                results
                    .iter()
                    .map(move |result| format!("<{}>/{}", suite, &result.path))
            })
            .collect();

        let succeeded_string = serde_json::to_string_pretty(&succeeded_paths).unwrap();
        let failed_string = serde_json::to_string_pretty(&failed_paths).unwrap();

        let succeeded_file_path = format!("{result_files_path}_success.json");
        let failed_file_path = format!("{result_files_path}_failed.json");

        fs::write(succeeded_file_path, &succeeded_string)?;
        fs::write(failed_file_path, &failed_string)?;

        Ok(())
    }

    pub fn save_to_time_file(&self, tile_file_path: String) -> GenericResult {
        let mut all_test_results = vec![];
        all_test_results.extend(self.succeeded.values().flat_map(|results| results.iter()));
        all_test_results.extend(self.failed.values().flat_map(|results| results.iter()));
        all_test_results.extend(self.timed_out.values().flat_map(|results| results.iter()));

        all_test_results.sort_by(|a, b| a.time.cmp(&b.time).reverse());

        let mut test_result_jsons = vec![];
        for test_result in all_test_results {
            let test_result_json = json!({
                "path": test_result.path,
                "suite": test_result.suite,
                "time": test_result.time.as_secs_f64(),
                "succeeded": test_result.result == TestResultCompletion::Success,
            });

            test_result_jsons.push(test_result_json);
        }

        let time_file_string = serde_json::to_string_pretty(&test_result_jsons).unwrap();

        fs::write(tile_file_path, &time_file_string)?;

        Ok(())
    }
}

fn new_header_row() -> TableRow {
    TableRow::new(vec![
        TableCell::simple(String::new()),
        TableCell::with_modifiers("Succeeded".to_owned(), vec![BOLD, GREEN]),
        TableCell::with_modifiers("Failed".to_owned(), vec![BOLD, RED]),
        TableCell::with_modifiers("Timeout".to_owned(), vec![BOLD, YELLOW]),
        TableCell::with_modifiers("Skipped".to_owned(), vec![BOLD, DIM, WHITE]),
    ])
}

fn new_suite_row(
    suite: Suite,
    succeeded: u64,
    failed: u64,
    timed_out: u64,
    skipped: u64,
) -> TableRow {
    TableRow::new(vec![
        TableCell::new(suite.to_string(), Some(vec![BOLD]), CellAlignment::Left),
        TableCell::simple(succeeded.to_string()),
        TableCell::simple(failed.to_string()),
        TableCell::simple(timed_out.to_string()),
        TableCell::simple(skipped.to_string()),
    ])
}
