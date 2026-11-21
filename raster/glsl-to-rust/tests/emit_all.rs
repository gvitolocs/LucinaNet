/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Translates every program swgl builds, the way swgl's build script preprocesses them, into
//! `$CARGO_TARGET_DIR/emitted/<program>.rs`; checks each parses as a Rust file; regenerates the runtime inventory
//! at the end of `README.md` from the emitted files.

use glsl_to_rust::build::{shader_file, shader_keys, translate_program};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

const MODULE_ORDER: [&str; 4] = ["glsl", "texture", "ext", "program"];

fn shader_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../swgl/res")
}

fn out_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(d) => PathBuf::from(d).join("emitted"),
        None => Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("emitted"),
    }
}

/// What the emitted code takes from the runtime.
#[derive(Default)]
struct Inventory {
    /// `use crate::<module>::{..}` names, by module.
    imports: BTreeMap<String, BTreeSet<String>>,
    /// Methods called that the file does not define.
    methods: BTreeSet<String>,
    /// `T::from(..)` for runtime types `T`.
    from: BTreeSet<String>,
    /// Other `T::f(..)` calls on runtime types (`mat4_scalar::load_from_ptr`).
    assoc: BTreeSet<String>,
    /// `// requires:` lines.
    requires: BTreeSet<String>,
}

/// Names a file defines itself: functions and methods, structs.
#[derive(Default)]
struct Defined {
    fns: BTreeSet<String>,
    structs: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Defined {
    fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
        self.fns.insert(i.sig.ident.to_string());
        syn::visit::visit_item_fn(self, i);
    }
    fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
        self.fns.insert(i.sig.ident.to_string());
        syn::visit::visit_impl_item_fn(self, i);
    }
    fn visit_item_struct(&mut self, i: &'ast syn::ItemStruct) {
        self.structs.insert(i.ident.to_string());
        syn::visit::visit_item_struct(self, i);
    }
}

struct Collect<'a> {
    defined: &'a Defined,
    inv: &'a mut Inventory,
}

fn use_leaves(tree: &syn::UseTree, out: &mut Vec<String>) {
    match tree {
        syn::UseTree::Path(p) => use_leaves(&p.tree, out),
        syn::UseTree::Group(g) => g.items.iter().for_each(|t| use_leaves(t, out)),
        syn::UseTree::Name(n) => out.push(n.ident.to_string()),
        syn::UseTree::Rename(r) => out.push(r.ident.to_string()),
        syn::UseTree::Glob(_) => {}
    }
}

impl<'ast> Visit<'ast> for Collect<'_> {
    fn visit_item_use(&mut self, i: &'ast syn::ItemUse) {
        if let syn::UseTree::Path(root) = &i.tree {
            if root.ident == "crate" {
                if let syn::UseTree::Path(module) = &*root.tree {
                    let mut names = Vec::new();
                    use_leaves(&module.tree, &mut names);
                    self.inv.imports.entry(module.ident.to_string()).or_default().extend(names);
                }
            }
        }
    }

    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        let m = e.method.to_string();
        if !self.defined.fns.contains(&m) {
            self.inv.methods.insert(m);
        }
        syn::visit::visit_expr_method_call(self, e);
    }

    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*e.func {
            let segs: Vec<String> = p.path.segments.iter().map(|s| s.ident.to_string()).collect();
            if segs.len() >= 2 {
                let (ty, f) = (&segs[segs.len() - 2], &segs[segs.len() - 1]);
                let local = self.defined.structs.contains(ty) || matches!(ty.as_str(), "Self" | "Box" | "core" | "std");
                if !local && f == "from" {
                    self.inv.from.insert(ty.clone());
                } else if !local && !self.defined.fns.contains(f) {
                    self.inv.assoc.insert(format!("{}::{}", ty, f));
                }
            }
        }
        syn::visit::visit_expr_call(self, e);
    }
}

/// Names as one Markdown paragraph of backticked items, wrapped at about 110 columns.
fn paragraph(names: impl IntoIterator<Item = String>) -> String {
    let mut s = String::new();
    let mut col = 0;
    for (i, n) in names.into_iter().enumerate() {
        let item = format!("`{}`", n);
        if i > 0 {
            s.push(',');
            col += 1;
            if col + item.len() + 1 > 110 {
                s.push('\n');
                col = 0;
            } else {
                s.push(' ');
                col += 1;
            }
        }
        col += item.len();
        s.push_str(&item);
    }
    s.push('\n');
    s
}

fn write_readme(inv: &Inventory, programs: usize, lines: usize) {
    let mut sec = String::new();
    let _ = writeln!(sec, "### Runtime inventory (generated by `tests/emit_all.rs`)\n");
    let _ = writeln!(sec, "{} programs, {} lines of Rust.\n", programs, lines);
    let mut modules: Vec<&String> = inv.imports.keys().collect();
    modules.sort_by_key(|m| (MODULE_ORDER.iter().position(|o| o == m).unwrap_or(MODULE_ORDER.len()), (*m).clone()));
    for m in modules {
        let names = &inv.imports[m];
        let _ = writeln!(sec, "#### `crate::{}` ({})\n", m, names.len());
        sec.push_str(&paragraph(names.iter().cloned()));
        sec.push('\n');
    }
    let _ = writeln!(sec, "#### Methods called on runtime types ({})\n", inv.methods.len());
    sec.push_str(&paragraph(inv.methods.iter().cloned()));
    sec.push('\n');
    let _ = writeln!(sec, "#### Associated functions of runtime types ({})\n", inv.assoc.len());
    sec.push_str(&paragraph(inv.assoc.iter().cloned()));
    sec.push('\n');
    let _ = writeln!(sec, "#### `T::from` conversions, by target type ({})\n", inv.from.len());
    sec.push_str(&paragraph(inv.from.iter().cloned()));
    sec.push('\n');
    let _ = writeln!(sec, "#### Signatures and operator impls the emitted code relies on ({})\n", inv.requires.len());
    for r in &inv.requires {
        let _ = writeln!(sec, "- `{}`", r);
    }

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let (begin, end) = ("<!-- runtime:begin -->", "<!-- runtime:end -->");
    let new = match (old.find(begin), old.find(end)) {
        (Some(b), Some(e)) if b < e => format!("{}\n\n{}\n{}", &old[..b + begin.len()], sec, &old[e..]),
        _ => format!("{}\n{}\n\n{}{}\n", old, begin, sec, end),
    };
    if new != old {
        std::fs::write(&path, new).unwrap();
    }
}

#[test]
fn emit_all_programs() {
    let keys = shader_keys();
    println!("{} programs", keys.len());
    let shader_dir = shader_dir();
    let out = out_dir();
    let pre = out.join("pre");
    std::fs::create_dir_all(&pre).unwrap();

    let mut failures = Vec::new();
    let mut sizes = Vec::new();
    let mut inv = Inventory::default();
    for key in &keys {
        let file = shader_file(key);
        let (code, _sources) = translate_program(key, &shader_dir, &pre);
        let path = out.join(format!("{}.rs", file));
        std::fs::write(&path, &code).unwrap();
        sizes.push((code.lines().count(), file.clone()));
        for l in code.lines() {
            if let Some(r) = l.strip_prefix("// requires: ") {
                inv.requires.insert(r.to_string());
            }
        }
        match syn::parse_file(&code) {
            Ok(ast) => {
                let mut defined = Defined::default();
                defined.visit_file(&ast);
                Collect { defined: &defined, inv: &mut inv }.visit_file(&ast);
            }
            Err(e) => {
                let start = e.span().start();
                let line = code.lines().nth(start.line.saturating_sub(1)).unwrap_or("").to_string();
                failures.push(format!("{}:{}:{}: {}\n    {}", path.display(), start.line, start.column, e, line));
            }
        }
    }

    let total: usize = sizes.iter().map(|(n, _)| n).sum();
    println!("{} lines of Rust in {}", total, out.display());
    sizes.sort_by(|a, b| b.cmp(a));
    for (n, f) in sizes.iter().take(5) {
        println!("  {:6} {}.rs", n, f);
    }
    for (m, names) in &inv.imports {
        println!("crate::{}: {} names", m, names.len());
    }
    println!(
        "methods: {}, associated functions: {}, From targets: {}, requirements: {}",
        inv.methods.len(),
        inv.assoc.len(),
        inv.from.len(),
        inv.requires.len()
    );
    write_readme(&inv, keys.len(), total);

    for f in &failures {
        println!("{}", f);
    }
    assert!(failures.is_empty(), "{} of {} programs do not parse", failures.len(), keys.len());
}
