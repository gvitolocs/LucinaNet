//! Write the test262 gap inventory of a results.jsonl, without the engine.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = lucinanet_inventory::run_args(&args) {
        eprintln!("error: {e}");
        std::process::exit(2);
    }
}
