/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The two views of a `Report`: the text listing and the JSON document.
//!
//! Both carry the same numbers; the text one is for a terminal, the JSON one
//! for a script.

use crate::inspect::{Report, other_proposals, producer_text};
use crate::verdict::{self, Feature};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write as _;

fn row(out: &mut String, label: &str, value: &str) {
    let _ = writeln!(out, "  {label:<22}{value}");
}

fn heading(out: &mut String, title: &str) {
    let _ = writeln!(out, "\n{title}");
}

fn padded(text: &str, width: usize) -> String {
    if text.len() >= width {
        format!("{text} ")
    } else {
        format!("{text}{}", " ".repeat(width - text.len()))
    }
}

/// One memory or table, in words.
fn limits_text(
    min: u64,
    max: Option<u64>,
    index64: bool,
    shared: bool,
    page_bits: Option<u32>,
) -> String {
    let mut text = format!("min {min}");
    match max {
        Some(max) => {
            let _ = write!(text, ", max {max}");
        }
        None => text.push_str(", max none"),
    }
    let _ = write!(
        text,
        ", {} index, {}shared",
        if index64 { "64-bit" } else { "32-bit" },
        if shared { "" } else { "not " }
    );
    if let Some(bits) = page_bits {
        let _ = write!(text, ", page 2^{bits} = {}", 1u64 << bits);
    }
    text
}

pub fn to_text(report: &Report, features: &[Feature]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "wasm-inspect: {}", report.file);
    row(
        &mut out,
        "size",
        &format!("{} bytes", verdict::group(report.size as u64)),
    );
    row(
        &mut out,
        "format",
        &format!("{}, version {}", report.format, report.version),
    );

    heading(
        &mut out,
        "SECTIONS                       offset        bytes   contents",
    );
    for section in &report.sections {
        let name = match &section.name {
            Some(name) => format!("{} ({name})", section.kind),
            None => section.kind.clone(),
        };
        let _ = writeln!(
            out,
            "  {:<28}{:#010x}  {:>12}  {:>12}",
            name,
            section.offset,
            verdict::group(section.bytes as u64),
            verdict::group(section.contents as u64)
        );
    }

    heading(&mut out, "COUNTS");
    let structural = report.types.iter().filter(|t| t.kind != "func").count();
    row(
        &mut out,
        "types",
        &format!(
            "{} ({} function, {structural} structural)",
            report.types.len(),
            report.types.len() - structural
        ),
    );
    let mut per_module: BTreeMap<&str, u64> = BTreeMap::new();
    for import in &report.imports {
        *per_module.entry(import.module.as_str()).or_default() += 1;
    }
    let per_module = per_module
        .iter()
        .map(|(module, count)| format!("{module} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    row(
        &mut out,
        "imports",
        &format!("{} from {}", report.imports.len(), per_module),
    );
    row(
        &mut out,
        "functions",
        &format!(
            "{} defined + {} imported = {} in the index space",
            verdict::group(report.funcs_declared),
            verdict::group(report.funcs_imported),
            verdict::group(report.func_space())
        ),
    );
    row(
        &mut out,
        "code",
        &format!(
            "{} bodies, {} operators scanned",
            verdict::group(report.scan.bodies),
            verdict::group(report.scan.operators)
        ),
    );
    row(&mut out, "exports", &report.exports.len().to_string());
    row(
        &mut out,
        "memories / tables",
        &format!("{} / {}", report.memories.len(), report.tables.len()),
    );
    row(&mut out, "globals", &report.globals.len().to_string());
    row(
        &mut out,
        "data segments",
        &format!(
            "{} ({} bytes total), data count section {}",
            report.data.len(),
            verdict::group(report.data.iter().map(|d| d.bytes).sum()),
            match report.scan.data_count {
                Some(declared) => format!("says {declared}"),
                None => "absent".to_string(),
            }
        ),
    );
    row(
        &mut out,
        "element segments",
        &report.elements.len().to_string(),
    );
    let start = match report.start {
        Some(func) => report.describe_func(func),
        None => "none".to_string(),
    };
    row(&mut out, "start", &start);
    if report.component_payloads > 0 {
        row(
            &mut out,
            "component payloads",
            &report.component_payloads.to_string(),
        );
    }

    heading(&mut out, "IMPORTS");
    for (index, import) in report.imports.iter().enumerate() {
        let _ = writeln!(
            out,
            "  #{}  {}  {}:{}",
            index,
            padded(&import.kind, 7),
            import.module,
            import.name
        );
        let _ = writeln!(out, "       {}", import.detail);
    }

    heading(&mut out, "EXPORTS");
    for (index, export) in report.exports.iter().enumerate() {
        let _ = writeln!(
            out,
            "  #{}  {}  {:<8} #{:<6} {}",
            index,
            padded(&export.name, 34),
            export.kind,
            export.index,
            match export.kind.as_str() {
                "func" => report.describe_func(export.index),
                "memory" => match report.memories.get(export.index as usize) {
                    Some(m) => limits_text(m.min, m.max, m.memory64, m.shared, m.page_size_log2),
                    None => "out of range".to_string(),
                },
                "table" => match report.tables.get(export.index as usize) {
                    Some(t) => format!(
                        "{} {}",
                        t.elem_type,
                        limits_text(t.min, t.max, t.table64, t.shared, None)
                    ),
                    None => "out of range".to_string(),
                },
                _ => String::new(),
            }
        );
    }

    heading(&mut out, "MEMORIES");
    for memory in &report.memories {
        let bits = memory.page_size_log2.unwrap_or(16);
        let bytes = memory.min.checked_shl(bits).unwrap_or(u64::MAX);
        let _ = writeln!(
            out,
            "  #{}  {}  (min {bytes} bytes){}",
            memory.index,
            limits_text(
                memory.min,
                memory.max,
                memory.memory64,
                memory.shared,
                memory.page_size_log2
            ),
            if memory.imported { ", imported" } else { "" }
        );
    }

    heading(&mut out, "TABLES");
    for table in &report.tables {
        let _ = writeln!(
            out,
            "  #{}  {:<10}{}{}",
            table.index,
            table.elem_type,
            limits_text(table.min, table.max, table.table64, table.shared, None),
            if table.imported { ", imported" } else { "" }
        );
    }

    heading(&mut out, "GLOBALS");
    for global in &report.globals {
        let _ = writeln!(
            out,
            "  #{:<4} {} {}",
            global.index,
            padded(
                &format!("{}{}", if global.mutable { "mut " } else { "" }, global.ty),
                18
            ),
            global.init
        );
    }

    heading(&mut out, "ELEMENT SEGMENTS");
    for element in &report.elements {
        let _ = writeln!(
            out,
            "  #{}  {:<32}{} item(s) of {}",
            element.index,
            element.kind,
            verdict::group(element.count),
            element.elem_type
        );
    }

    heading(&mut out, "DATA SEGMENTS");
    for segment in &report.data {
        let _ = writeln!(
            out,
            "  #{}  {}  {} bytes",
            segment.index,
            padded(&segment.kind, 40),
            verdict::group(segment.bytes)
        );
    }

    if !report.custom.is_empty() {
        heading(&mut out, "CUSTOM SECTIONS");
        for custom in &report.custom {
            let _ = writeln!(
                out,
                "  {:<28}{:#010x}  {:>12} bytes",
                padded(&custom.name, 28),
                custom.offset,
                verdict::group(custom.bytes as u64)
            );
            if !custom.note.is_empty() {
                let _ = writeln!(out, "      {}", custom.note);
            }
        }
    }

    if !report.producers.is_empty() {
        heading(&mut out, "PRODUCERS");
        for (field, values) in &report.producers {
            let _ = writeln!(
                out,
                "  {field:<14}{}",
                values
                    .iter()
                    .map(|(name, version)| producer_text(name, version))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    if !report.target_features.is_empty() {
        heading(&mut out, "TARGET FEATURES");
        let _ = writeln!(
            out,
            "  (+ built into this module, - ruled out by its toolchain)"
        );
        for feature in &report.target_features {
            let _ = writeln!(out, "  {feature}");
        }
    }

    heading(&mut out, "TYPES");
    for ty in &report.types {
        let _ = writeln!(
            out,
            "  #{}  {}  {}{}",
            ty.index,
            padded(&ty.kind, 8),
            ty.signature,
            if ty.gc { " (GC index space)" } else { "" }
        );
    }

    heading(&mut out, "FEATURE VERDICT");
    let _ = writeln!(
        out,
        "  {:<24}{:<6}{:>10}  evidence",
        "feature", "used", "count"
    );
    for feature in features {
        let _ = writeln!(
            out,
            "  {:<24}{:<6}{:>10}  {}",
            feature.name,
            if feature.used { "yes" } else { "no" },
            verdict::group(feature.count),
            feature.evidence
        );
    }
    let other = other_proposals(report);
    if !other.is_empty() {
        let _ = writeln!(out, "  other proposals:");
        for (name, count, example) in other {
            let _ = match &example {
                Some((offset, variant)) => writeln!(
                    out,
                    "    {name:<22}{:>10} operators, first at {offset:#x} ({variant})",
                    verdict::group(count)
                ),
                None => writeln!(out, "    {name:<22}{count}"),
            };
        }
    }
    if report.types.iter().any(|t| t.gc) {
        let _ = writeln!(
            out,
            "  note: structural types present, so the type indices shown are the expanded ones"
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", verdict::needs_line(features));
    let _ = writeln!(out, "{}", verdict::wasmi_config_line(features));
    out
}

pub fn to_json(report: &Report, features: &[Feature]) -> Value {
    let structural = report.types.iter().filter(|t| t.kind != "func").count();
    json!({
        "file": report.file,
        "size": report.size,
        "format": report.format,
        "version": report.version,
        "sections": report.sections.iter().map(|s| json!({
            "kind": s.kind,
            "name": s.name,
            "offset": s.offset,
            "bytes": s.bytes,
            "contents": s.contents,
        })).collect::<Vec<_>>(),
        "counts": {
            "types": report.types.len(),
            "functionTypes": report.types.len() - structural,
            "structuralTypes": structural,
            "imports": report.imports.len(),
            "exports": report.exports.len(),
            "functionsDefined": report.funcs_declared,
            "functionsImported": report.funcs_imported,
            "functionIndexSpace": report.func_space(),
            "codeBodies": report.scan.bodies,
            "operatorsScanned": report.scan.operators,
            "memories": report.memories.len(),
            "tables": report.tables.len(),
            "globals": report.globals.len(),
            "dataSegments": report.data.len(),
            "dataBytes": report.data.iter().map(|d| d.bytes).sum::<u64>(),
            "elementSegments": report.elements.len(),
            "dataCount": report.scan.data_count,
            "componentPayloads": report.component_payloads,
        },
        "start": report.start.map(|f| json!({
            "index": f,
            "description": report.describe_func(f),
        })),
        "types": report.types.iter().map(|t| json!({
            "index": t.index,
            "kind": t.kind,
            "signature": t.signature,
            "params": t.params,
            "results": t.results,
            "gc": t.gc,
        })).collect::<Vec<_>>(),
        "imports": report.imports.iter().map(|i| json!({
            "module": i.module,
            "name": i.name,
            "kind": i.kind,
            "detail": i.detail,
            "typeIndex": i.type_index,
            "signature": if i.kind == "func" { Value::String(report.signature(i.type_index)) } else { Value::Null },
        })).collect::<Vec<_>>(),
        "exports": report.exports.iter().map(|e| json!({
            "name": e.name,
            "kind": e.kind,
            "index": e.index,
        })).collect::<Vec<_>>(),
        "memories": report.memories.iter().map(|m| json!({
            "index": m.index,
            "imported": m.imported,
            "minPages": m.min,
            "maxPages": m.max,
            "shared": m.shared,
            "memory64": m.memory64,
            "pageSizeLog2": m.page_size_log2,
        })).collect::<Vec<_>>(),
        "tables": report.tables.iter().map(|t| json!({
            "index": t.index,
            "imported": t.imported,
            "elementType": t.elem_type,
            "min": t.min,
            "max": t.max,
            "shared": t.shared,
            "table64": t.table64,
        })).collect::<Vec<_>>(),
        "globals": report.globals.iter().map(|g| json!({
            "index": g.index,
            "type": g.ty,
            "mutable": g.mutable,
            "shared": g.shared,
            "init": g.init,
        })).collect::<Vec<_>>(),
        "elementSegments": report.elements.iter().map(|e| json!({
            "index": e.index,
            "kind": e.kind,
            "elementType": e.elem_type,
            "count": e.count,
        })).collect::<Vec<_>>(),
        "dataSegments": report.data.iter().map(|d| json!({
            "index": d.index,
            "kind": d.kind,
            "bytes": d.bytes,
        })).collect::<Vec<_>>(),
        "customSections": report.custom.iter().map(|c| json!({
            "name": c.name,
            "offset": c.offset,
            "bytes": c.bytes,
            "note": c.note,
        })).collect::<Vec<_>>(),
        "producers": report.producers.iter().map(|(field, values)| json!({
            "field": field,
            "values": values.iter().map(|(name, version)| json!({
                "name": name, "version": version,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "targetFeatures": report.target_features,
        "features": features.iter().map(|f| json!({
            "name": f.name,
            "used": f.used,
            "count": f.count,
            "example": f.example.as_ref().map(|(offset, what)| json!({
                "offset": offset, "operator": what,
            })),
            "evidence": f.evidence,
            "wasmiSetter": f.setter,
            "supportedByWasmi": f.supported,
        })).collect::<Vec<_>>(),
        "otherProposals": other_proposals(report).iter().map(|(name, count, example)| json!({
            "proposal": name,
            "operators": count,
            "example": example.as_ref().map(|(offset, what)| json!({
                "offset": offset, "operator": what,
            })),
        })).collect::<Vec<_>>(),
        "needs": verdict::needs_line(features),
        "wasmiConfig": verdict::wasmi_config_line(features),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_never_shrinks_a_long_label() {
        assert_eq!(padded("ab", 5), "ab   ");
        assert_eq!(padded("abcdef", 4), "abcdef ");
    }

    #[test]
    fn limits_read_like_a_person() {
        assert_eq!(
            limits_text(16, None, false, false, None),
            "min 16, max none, 32-bit index, not shared"
        );
        assert_eq!(
            limits_text(1, Some(2), true, true, Some(16)),
            "min 1, max 2, 64-bit index, shared, page 2^16 = 65536"
        );
    }
}
