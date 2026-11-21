//! `media-trace-check [LOG|-] [--clock-window S] [--play-window S] [--strict]`
//!
//! Reads a LUCINANET_MEDIA_TRACE log (a file, or stdin with `-` or no argument) and prints either
//! `ok: no violation (N lines, properties checked: …)` or the first property the trace broke, with five lines around it.
//! Exit 0 when nothing was broken, 1 on a violation — and with `--strict`, on a property the trace said nothing about, which
//! is not a pass. Exit 2 is a usage or input error.

use std::io::{Read as _, Write as _};
use std::process::exit;

use media_trace_check::{check_log, parse, Config};

fn usage(why: Option<&str>) -> ! {
    if let Some(why) = why {
        let _ = writeln!(std::io::stderr(), "media-trace-check: {why}");
    }
    // Every exit-2 path says the same shape, so a script that reads stderr sees both what went wrong and what would have worked.
    let _ =
        writeln!(std::io::stderr(), "usage: media-trace-check [LOG|-] [--clock-window S] [--play-window S] [--strict]");
    exit(2)
}

/// Takes `--name value` out of the list and hands back the value.
fn option(args: &mut Vec<String>, name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    if at + 1 >= args.len() {
        usage(Some(&format!("{name} wants a value")));
    }
    let v = args.remove(at + 1);
    args.remove(at);
    Some(v)
}

fn seconds(args: &mut Vec<String>, name: &str, fallback: f64) -> f64 {
    match option(args, name) {
        Some(v) => v.parse().unwrap_or_else(|_| usage(Some(&format!("{name} wants seconds, not {v}")))),
        None => fallback,
    }
}

fn flag(args: &mut Vec<String>, name: &str) -> bool {
    let at = args.iter().position(|a| a == name);
    if let Some(at) = at {
        args.remove(at);
    }
    at.is_some()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if flag(&mut args, "--help") || flag(&mut args, "-h") {
        usage(None);
    }
    let strict = flag(&mut args, "--strict");
    let cfg = Config {
        clock_window: seconds(&mut args, "--clock-window", Config::default().clock_window),
        play_window: seconds(&mut args, "--play-window", Config::default().play_window),
    };
    let path = match args.as_slice() {
        [] => None,
        [p] if p == "-" => None,
        // A lone `-` means stdin; anything else that starts with a dash was meant to be an option.
        [p] if p.starts_with('-') => usage(Some(&format!("unknown argument {p}"))),
        [p] => Some(p.clone()),
        _ => usage(Some("one log at a time")),
    };
    let text = match path {
        Some(p) => std::fs::read_to_string(&p).unwrap_or_else(|e| usage(Some(&format!("cannot read {p}: {e}")))),
        None => {
            let mut from_stdin = String::new();
            std::io::stdin()
                .read_to_string(&mut from_stdin)
                .unwrap_or_else(|e| usage(Some(&format!("cannot read stdin: {e}"))));
            from_stdin
        }
    };
    let lines = parse(&text);
    let report = check_log(&lines, &cfg);
    print!("{}", report.render(&lines));
    let _ = std::io::stdout().flush();
    exit(if !report.violations.is_empty() || (strict && !report.complete()) { 1 } else { 0 });
}
