//! `gltrace perf-table <cxx.log> <rust.log> [--page NAME]`: the `LUCINANET_SWGL_PROFILE` lines of a replay on each
//! backend, side by side as a markdown table (raster/PERF.md). A key is (shader or operation, blend), summed over all
//! windows of a log; rows are sorted by Rust time, largest first (ties by key).

use std::collections::BTreeMap;

#[derive(Clone, Copy, Default)]
struct Sums {
    calls: u64,
    px: u64,
    ms: f64,
}

type Table = BTreeMap<(String, i64), Sums>;

/// `swgl-prof NAME blend B calls C inst I px P ms M`; the `swgl-prof window ...` lines have no `blend` and are skipped.
fn parse(text: &str) -> Table {
    let mut t = Table::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("swgl-prof ") else { continue };
        let f: Vec<&str> = rest.split_whitespace().collect();
        let field = |k: &str| f.iter().position(|&w| w == k).and_then(|i| f.get(i + 1)).copied();
        let (Some(name), Some(blend), Some(ms)) = (f.first(), field("blend"), field("ms")) else { continue };
        let (Ok(blend), Ok(ms)) = (blend.parse::<i64>(), ms.parse::<f64>()) else { continue };
        let s = t.entry(((*name).to_owned(), blend)).or_default();
        s.calls += field("calls").and_then(|v| v.parse().ok()).unwrap_or(0);
        s.px += field("px").and_then(|v| v.parse().ok()).unwrap_or(0);
        s.ms += ms;
    }
    t
}

fn ms_str(ms: f64) -> String {
    if ms > 0.0 {
        format!("{ms:.2}")
    } else {
        "-".into()
    }
}

fn ns_per_px(s: Option<&Sums>) -> String {
    match s {
        Some(s) if s.px > 0 => format!("{:.2}", s.ms * 1e6 / s.px as f64),
        _ => "-".into(),
    }
}

/// The table for two logs' contents.
pub fn table(cxx: &str, rust: &str, page: Option<&str>) -> String {
    let (c, r) = (parse(cxx), parse(rust));
    let mut keys: Vec<&(String, i64)> = c.keys().chain(r.keys().filter(|k| !c.contains_key(*k))).collect();
    let rust_ms = |k: &(String, i64)| r.get(k).map_or(0.0, |s| s.ms);
    keys.sort_by(|a, b| rust_ms(b).total_cmp(&rust_ms(a)).then_with(|| a.cmp(b)));
    let mut out = String::new();
    if let Some(p) = page {
        out += &format!("### {p}\n\n");
    }
    out += "| key | blend | calls | px | C++ ms | Rust ms | C++ ns/px | Rust ns/px | Rust/C++ |\n";
    out += "|---|---|---|---|---|---|---|---|---|\n";
    let (mut total_c, mut total_r) = (0.0, 0.0);
    for k in keys {
        let (cs, rs) = (c.get(k), r.get(k));
        let zero = Sums::default();
        let (cv, rv) = (cs.unwrap_or(&zero), rs.unwrap_or(&zero));
        let calls = if cv.calls > 0 { cv.calls } else { rv.calls };
        let px = if cv.px > 0 { cv.px } else { rv.px };
        total_c += cv.ms;
        total_r += rv.ms;
        let ratio = if cv.ms == 0.0 { "-".into() } else { format!("{:.2}", rv.ms / cv.ms) };
        out += &format!(
            "| {} | {} | {calls} | {px} | {} | {} | {} | {} | {ratio} |\n",
            k.0,
            k.1,
            ms_str(cv.ms),
            ms_str(rv.ms),
            ns_per_px(cs),
            ns_per_px(rs)
        );
    }
    let ratio = if total_c == 0.0 || total_r == 0.0 { "-".into() } else { format!("{:.2}", total_r / total_c) };
    out += &format!("\ntotal: C++ {total_c:.2} ms, Rust {total_r:.2} ms, Rust/C++ {ratio}\n");
    out
}

/// `perf-table <cxx.log> <rust.log> [--page NAME]`.
pub fn run(args: &[String]) -> Result<(), String> {
    const USAGE: &str = "usage: gltrace perf-table <cxx.log> <rust.log> [--page NAME]";
    let mut logs = Vec::new();
    let mut page = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--page" {
            page = Some(it.next().ok_or(USAGE)?.clone());
        } else {
            logs.push(a);
        }
    }
    let [cxx, rust] = logs.as_slice() else { return Err(USAGE.into()) };
    let read = |p: &String| std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"));
    print!("{}", table(&read(cxx)?, &read(rust)?, page.as_deref()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::table;

    const CXX: &str = "swgl-prof window 2001 ms, swgl 3.0 ms
swgl-prof ps_quad_gradient                                 blend   2 calls      2 inst      4 px       1000 ms     2.00
swgl-prof Clear                                            blend   0 calls      1 inst      1 px          0 ms     1.00
swgl-prof window 10 ms, swgl 1.0 ms
swgl-prof ps_quad_gradient                                 blend   2 calls      1 inst      1 px       1000 ms     1.00
";
    const RUST: &str = "swgl-prof ps_quad_gradient blend 2 calls 3 inst 5 px 2000 ms 6.00
swgl-prof Composite blend 1 calls 1 inst 1 px 10 ms 0.50
";

    #[test]
    fn sums_windows_and_sorts_by_rust_time() {
        let t = table(CXX, RUST, Some("p"));
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines[0], "### p");
        assert_eq!(lines[4], "| ps_quad_gradient | 2 | 3 | 2000 | 3.00 | 6.00 | 1500.00 | 3000.00 | 2.00 |");
        assert_eq!(lines[5], "| Composite | 1 | 1 | 10 | - | 0.50 | - | 50000.00 | - |");
        assert_eq!(lines[6], "| Clear | 0 | 1 | 0 | 1.00 | - | - | - | 0.00 |");
        assert_eq!(*lines.last().unwrap(), "total: C++ 4.00 ms, Rust 6.50 ms, Rust/C++ 1.62");
    }
}
