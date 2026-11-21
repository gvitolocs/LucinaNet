use std::{collections::HashMap, path::PathBuf, time::Duration};

use brimstone_integration_test_harness::{
    ignored::IgnoredIndex,
    index::TestIndex,
    manifest::{Suite, SuiteFilter, TestManifest},
    runner::{RunnerConfig, TestRunner},
    timeout::DEFAULT_TEST_TIMEOUT_SECS,
    utils::GenericResult,
};
use clap::Parser;

#[derive(Parser)]
#[command(about)]
struct Args {
    /// Run all tests, including non-standard and slow tests
    #[arg(long, default_value_t = false)]
    all: bool,

    /// Path to the test manifest. If not set attempts to locate the test manifest in the `brimstone`
    /// repo automatically.
    #[arg(long)]
    test_manifest_path: Option<String>,

    /// Only run the provided test suites. If not set, all test suites will be run. Option may
    /// contain a comma separated list of suites or be used multiple times.
    #[arg(short, long, value_delimiter = ',')]
    suite: Option<Vec<Suite>>,

    /// Ignore tests for unimplemented features
    #[arg(long, default_value_t = false)]
    ignore_unimplemented: bool,

    /// Reindex the test suite before running
    #[arg(short, long, default_value_t = false)]
    reindex: bool,

    /// Number of threads to use in test runner
    #[arg(short, long, default_value_t = 8)]
    threads: u8,

    /// Print extra information to console while running tests
    #[arg(short, long, default_value_t = false)]
    verbose: bool,

    /// Optional path to write result files to. Result files will have the names
    /// <path>_success.json and <path>_failure.json.
    #[arg(long)]
    save_result_files: Option<String>,

    /// Optional path to write test runtime statistics file to
    #[arg(long)]
    save_time_file: Option<String>,

    /// Report numbers in terms of progress against the test262 suite. All failed standard test262
    /// tests are counted as failures, even if tests are ignored.
    #[arg(long, default_value_t = false)]
    report_test262_progress: bool,

    /// Install a no-op embedder host in every test context, to run the suites through the
    /// embedding paths (promises pinned in the host space, host-aware job handling)
    #[arg(long, default_value_t = false)]
    embed_host: bool,

    /// Generate the bytecode of every function when its script is compiled, instead of on the
    /// function's first call
    #[arg(long, default_value_t = false)]
    no_lazy: bool,

    /// Run every test with the structural function cache on, shared by all tests of the run
    #[arg(long, default_value_t = false)]
    function_cache: bool,

    /// With --function-cache, also generate every function on a cache hit and compare
    #[arg(long, default_value_t = false)]
    function_cache_verify: bool,

    /// Keep each script's AST for its lazy functions instead of dropping it once the script has
    /// run (then lazy functions are not parsed again on their first call)
    #[arg(long, default_value_t = false)]
    no_reparse: bool,

    /// Keep the AST, and generate every re-parsed function from it too, comparing the two
    #[arg(long, default_value_t = false)]
    lazy_reparse_verify: bool,

    /// Wall-clock seconds that one run of a test may use before it is interrupted and reported as
    /// a timeout, counted as a failure. A test run twice (strict and non-strict) gets it twice. 0
    /// disables the timeout. Defaults to 300, or to the BS_TEST_TIMEOUT_SECS environment variable
    /// when that is set.
    #[arg(long, env = "BS_TEST_TIMEOUT_SECS", default_value_t = DEFAULT_TEST_TIMEOUT_SECS)]
    test_timeout_secs: u64,

    /// Only run tests that match this feature
    #[arg(long)]
    feature: Option<String>,

    /// Only run tests that match this filter string
    filter: Option<String>,
}

fn main_impl() -> GenericResult {
    // Global initialization
    brimstone_serialized_heap::init();

    let args = Args::parse();

    brimstone_integration_test_harness::runner::EMBED_HOST
        .store(args.embed_host, std::sync::atomic::Ordering::Relaxed);
    brimstone_integration_test_harness::runner::NO_LAZY
        .store(args.no_lazy, std::sync::atomic::Ordering::Relaxed);
    brimstone_integration_test_harness::runner::FUNCTION_CACHE.store(
        args.function_cache || args.function_cache_verify,
        std::sync::atomic::Ordering::Relaxed,
    );
    brimstone_integration_test_harness::runner::FUNCTION_CACHE_VERIFY
        .store(args.function_cache_verify, std::sync::atomic::Ordering::Relaxed);
    brimstone_integration_test_harness::runner::NO_REPARSE
        .store(args.no_reparse, std::sync::atomic::Ordering::Relaxed);
    brimstone_integration_test_harness::runner::REPARSE_VERIFY
        .store(args.lazy_reparse_verify, std::sync::atomic::Ordering::Relaxed);

    let manifest_path = args
        .test_manifest_path
        .map(PathBuf::from)
        .unwrap_or_else(find_default_test_manifest_path);

    let manifest = TestManifest::load_from_file(&manifest_path)?;

    let index_path = manifest.index_path();
    let test262_root = manifest.test262_repo_path();

    let index = if args.reindex {
        println!("Indexing test suite...");
        let index = TestIndex::new(&manifest)?;

        println!("Finished indexing. Writing index to file.");
        index.write_to_file(&index_path)?;

        index
    } else {
        TestIndex::load_from_file(&index_path)?
    };

    // `--report-test262-progress` should only run the test262 suite
    let suite_filter = if args.report_test262_progress {
        SuiteFilter::new(Some(vec![Suite::Test262]))
    } else {
        SuiteFilter::new(args.suite.clone())
    };

    // Load all ignored indices
    let mut ignored = HashMap::new();
    for suite_config in &manifest.suites {
        let ignored_path = manifest
            .manifest_dir
            .as_path()
            .join(&suite_config.ignored_file);
        let ignored_index = IgnoredIndex::load_from_file(
            &ignored_path,
            args.all,
            args.ignore_unimplemented,
            args.report_test262_progress,
        )?;
        ignored.insert(suite_config.suite, ignored_index);
    }

    let mut runner = TestRunner::new(
        manifest,
        index,
        ignored,
        RunnerConfig {
            suite_filter,
            num_threads: args.threads,
            filter: args.filter,
            feature: args.feature,
            test_timeout: Duration::from_secs(args.test_timeout_secs),
        },
    );
    let results = runner.run(args.verbose);

    results.print_to_console(&test262_root);
    brimstone_integration_test_harness::runner::print_function_cache_totals();
    brimstone_integration_test_harness::runner::print_reparse_totals();

    if args.report_test262_progress {
        results.print_test262_progress();
    }

    if let Some(result_files_path) = args.save_result_files {
        results.save_to_result_files(result_files_path)?;
    }

    if let Some(time_file_path) = args.save_time_file {
        results.save_to_time_file(time_file_path)?;
    }

    if !results.is_successful() {
        std::process::exit(1);
    }

    Ok(())
}

/// Wrapper to pretty print errors
fn main() {
    match main_impl() {
        Ok(_) => (),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

/// The default path to the root directory for the test suite if none was provided. Attempts to
/// return the `tests/test_manifest.jsonc` file in the root of the `brimstone` repository.
///
/// Will only succeed when the program is run from within the `brimstone` repository.
fn find_default_test_manifest_path() -> PathBuf {
    let mut path = find_workspace_root_path();
    path.push("tests");
    path.push("test_manifest.jsonc");
    path
}

/// Find the root of the `brimstone` repository by searching upwards for a `Cargo.toml` file that
/// starts with the string `[workspace]`.
fn find_workspace_root_path() -> PathBuf {
    let mut path = std::env::current_dir().unwrap();

    loop {
        if path.join("Cargo.toml").exists() {
            let contents = std::fs::read_to_string(path.join("Cargo.toml")).unwrap();
            if contents.starts_with("[workspace]") {
                return path;
            }
        }

        if !path.pop() {
            panic!("Could not find the root of the brimstone repository");
        }
    }
}
