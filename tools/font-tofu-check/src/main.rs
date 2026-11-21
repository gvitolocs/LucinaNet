//! font-tofu-check: which cells of tests/fonts/cjk-emoji.html did the engine draw as the notdef box (tofu), and which did
//! it leave blank. See README.md.
//!
//!   font-tofu-check --page cjk-emoji.html --png render.png [--max-tofu N] [--json]
//!   font-tofu-check --page cjk-emoji.html --size          prints the WxH to render the page at
//!
//! Exit 0 with the table; 1 when --max-tofu N is given and the tofu cells of the test blocks exceed N; 2 on an error
//! (bad arguments, an unreadable file, a render of the wrong size, or --max-tofu with a blank tofu reference).

mod check;
mod page;
mod report;

use std::process::ExitCode;

const USAGE: &str = "usage: font-tofu-check --page <cjk-emoji.html> (--png <render.png> [--max-tofu N] [--json] | --size)";

struct Args {
    page: String,
    png: Option<String>,
    max_tofu: Option<u32>,
    json: bool,
    size: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args { page: String::new(), png: None, max_tofu: None, json: false, size: false };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--page" => a.page = value("--page")?,
            "--png" => a.png = Some(value("--png")?),
            "--max-tofu" => {
                let v = value("--max-tofu")?;
                a.max_tofu = Some(v.parse().map_err(|_| format!("--max-tofu {v}: not a whole number"))?);
            }
            "--json" => a.json = true,
            "--size" => a.size = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.page.is_empty() {
        return Err("--page is required".to_string());
    }
    if !a.size && a.png.is_none() {
        return Err("--png or --size is required".to_string());
    }
    Ok(a)
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let a = parse_args(args)?;
    let html = std::fs::read_to_string(&a.page).map_err(|e| format!("{}: {e}", a.page))?;
    let page = page::parse(&html)?;
    if a.size {
        println!("{}x{}", page.width(), page.height);
        return Ok(ExitCode::SUCCESS);
    }
    let png_path = a.png.as_deref().unwrap_or_default();
    let bytes = std::fs::read(png_path).map_err(|e| format!("{png_path}: {e}"))?;
    let img = check::decode(&bytes)?;
    let out = check::analyse(&page, &img)?;
    if a.json {
        println!("{}", report::json(&out));
    } else {
        println!("font-tofu-check: {} ({}x{}), page {}", png_path, img.w, img.h, a.page);
        print!("{}", report::table(&out));
    }
    if let Some(max) = a.max_tofu {
        if !out.refs_usable() {
            eprintln!("font-tofu-check: the tofu reference is blank or missing: cannot judge --max-tofu {max}");
            return Ok(ExitCode::from(2));
        }
        if out.tofu_total() > max {
            eprintln!("font-tofu-check: {} cells drew as tofu, more than --max-tofu {max}", out.tofu_total());
            return Ok(ExitCode::from(1));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("font-tofu-check: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_args;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_are_checked() {
        let a = parse_args(&v(&["--page", "p.html", "--png", "r.png", "--max-tofu", "7", "--json"])).unwrap();
        assert_eq!((a.max_tofu, a.json, a.size), (Some(7), true, false));
        assert!(parse_args(&v(&["--png", "r.png"])).is_err());
        assert!(parse_args(&v(&["--page", "p.html"])).is_err());
        assert!(parse_args(&v(&["--page", "p.html", "--size"])).unwrap().size);
        assert!(parse_args(&v(&["--page", "p.html", "--png", "r.png", "--max-tofu", "x"])).is_err());
        assert!(parse_args(&v(&["--page"])).is_err());
    }
}
