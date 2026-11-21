//! `gltrace info`: what a trace holds.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use swgl::trace::{Op, TraceReader};

/// Prints the number of records, frames, threads and bytes, then the count and payload bytes of each op,
/// most frequent first.
pub fn info(path: &Path) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut reader = TraceReader::open(path).map_err(err)?;
    let mut ops: HashMap<String, (u64, u64)> = HashMap::new();
    let mut threads = HashSet::new();
    let (mut records, mut frames) = (0u64, 0u64);
    while let Some(rec) = reader.next_record().map_err(err)? {
        records += 1;
        threads.insert(rec.thread);
        let name = match Op::from_u16(rec.op) {
            Some(Op::FrameMarker) => {
                frames += 1;
                "FrameMarker".to_string()
            }
            Some(op) => op.name().to_string(),
            None => format!("unknown({})", rec.op),
        };
        let e = ops.entry(name).or_default();
        e.0 += 1;
        e.1 += rec.payload.len() as u64;
    }
    let bytes = std::fs::metadata(path).map_err(err)?.len();
    println!("trace {}", path.display());
    println!("records {records}  frames {frames}  threads {}  bytes {bytes}", threads.len());
    if reader.truncated {
        println!("truncated tail");
    }
    let mut rows: Vec<_> = ops.into_iter().collect();
    rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then_with(|| a.0.cmp(&b.0)));
    println!("{:<26}{:>10}{:>15}", "op", "count", "payload bytes");
    for (name, (count, payload)) in rows {
        println!("{name:<26}{count:>10}{payload:>15}");
    }
    Ok(())
}
