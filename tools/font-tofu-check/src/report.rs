//! The table and the json form of an `Outcome`.

use crate::check::Outcome;

/// block, kind, cells, tofu, blank; then the tofu and blank code points of each block that has any.
pub fn table(out: &Outcome) -> String {
    let width = out.blocks.iter().map(|b| b.name.chars().count()).max().unwrap_or(5).max("total (test blocks)".len());
    let mut s = String::new();
    s.push_str(&format!("{:<width$}  {:<7}  {:>5}  {:>5}  {:>5}\n", "block", "kind", "cells", "tofu", "blank"));
    for b in &out.blocks {
        let kind = if b.control { "control" } else { "test" };
        s.push_str(&format!("{:<width$}  {:<7}  {:>5}  {:>5}  {:>5}\n", b.name, kind, b.cells, b.tofu, b.blank));
    }
    s.push_str(&format!(
        "{:<width$}  {:<7}  {:>5}  {:>5}  {:>5}\n",
        "total (test blocks)",
        "test",
        out.cells_total(),
        out.tofu_total(),
        out.blank_total()
    ));
    s.push_str(&format!(
        "tofu reference: {} cells, {} distinct drawings, {} blank\n",
        out.ref_cells, out.ref_distinct, out.ref_blank
    ));
    if !out.refs_usable() {
        s.push_str("WARNING: a tofu reference cell is blank or missing, so the tofu counts above cannot be trusted\n");
    }
    for b in &out.blocks {
        if !b.tofu_cps.is_empty() {
            s.push_str(&format!("tofu  {}: {}\n", b.name, b.tofu_cps.join(" ")));
        }
    }
    for b in &out.blocks {
        if !b.blank_cps.is_empty() {
            s.push_str(&format!("blank {}: {}\n", b.name, b.blank_cps.join(" ")));
        }
    }
    s
}

pub fn json(out: &Outcome) -> String {
    let blocks: Vec<serde_json::Value> = out
        .blocks
        .iter()
        .map(|b| {
            serde_json::json!({
                "block": b.name,
                "control": b.control,
                "cells": b.cells,
                "tofu": b.tofu,
                "blank": b.blank,
                "tofu_cps": b.tofu_cps,
                "blank_cps": b.blank_cps,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "blocks": blocks,
        "total": { "cells": out.cells_total(), "tofu": out.tofu_total(), "blank": out.blank_total() },
        "reference": { "cells": out.ref_cells, "distinct": out.ref_distinct, "blank": out.ref_blank, "usable": out.refs_usable() },
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::BlockResult;

    fn sample() -> Outcome {
        Outcome {
            blocks: vec![
                BlockResult {
                    name: "Chinese".into(),
                    cells: 4,
                    tofu: 2,
                    blank: 1,
                    tofu_cps: vec!["U+4E2D".into(), "U+4E00".into()],
                    blank_cps: vec!["U+4E8C".into()],
                    ..Default::default()
                },
                BlockResult { name: "Latin".into(), control: true, cells: 2, ..Default::default() },
            ],
            ref_cells: 2,
            ref_distinct: 1,
            ref_blank: 0,
        }
    }

    #[test]
    fn the_table_has_a_row_per_block_a_total_and_the_code_points() {
        let t = table(&sample());
        assert!(t.contains("Chinese"));
        assert!(t.contains("control"));
        assert!(t.contains("total (test blocks)"));
        assert!(t.contains("tofu  Chinese: U+4E2D U+4E00"));
        assert!(t.contains("blank Chinese: U+4E8C"));
        assert!(!t.contains("WARNING"));
    }

    #[test]
    fn json_carries_the_same_totals() {
        let v: serde_json::Value = serde_json::from_str(&json(&sample())).unwrap();
        assert_eq!(v["total"]["tofu"], 2);
        assert_eq!(v["total"]["cells"], 4);
        assert_eq!(v["reference"]["usable"], true);
    }
}
