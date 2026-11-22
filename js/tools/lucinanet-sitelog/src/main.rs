//! Aggregate the run-time site logs of `lucinanet --render` runs and of `LUCINANET_JS_MISSING_LOG`
//! into the missing-feature inventory this tool writes.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = lucinanet_sitelog::run_args(&args) {
        eprintln!("error: {e}");
        std::process::exit(2);
    }
}
