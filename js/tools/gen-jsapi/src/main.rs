// mozjs_sys's jsapi.rs and gluebindings.rs are bindgen output: every C++ struct as Rust, every C++
// function as `extern "C"`. This tool keeps the structs and rewrites the functions:
//   - a function the engine implements (`pub unsafe extern "C" fn <flat name>` in src/engine) becomes a
//     re-export, so call sites are type checked against the implementation;
//   - any other function becomes a stub that names itself and aborts, which is the to-do list at run time.
// The flat name is the namespace path joined with `__`: JS::Evaluate2 is JS__Evaluate2, js::Foo is
// js__Foo, a root function keeps its own name, glue functions are glue__Foo.
//
// Usage: gen-jsapi <bindgen dir> <engine src dir> <out dir>

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{env, fs};

use quote::{format_ident, quote};
use syn::{ForeignItem, Item, parse_quote};

fn walk_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn implemented(engine: &Path) -> BTreeSet<String> {
    let mut files = vec![];
    walk_rs(engine, &mut files);
    let mut set = BTreeSet::new();
    let needle = "pub unsafe extern \"C\" fn ";
    for f in files {
        let text = fs::read_to_string(f).unwrap();
        for (i, _) in text.match_indices(needle) {
            let rest = &text[i + needle.len()..];
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            set.insert(name);
        }
        // Statics the engine defines (`pub static mut JS__NullHandleValue: ...`, `pub static js__ProxyClassOps: ...`).
        for (i, _) in text.match_indices("pub static ") {
            let rest = &text[i + "pub static ".len()..];
            let rest = rest.strip_prefix("mut ").unwrap_or(rest);
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            set.insert(name);
        }
        for mac in ["realm_proto!(", "to_int!(", "tracer_fn!("] {
            for (i, _) in text.match_indices(mac) {
                let rest = &text[i + mac.len()..];
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !name.is_empty() && name != "$name" {
                    set.insert(name);
                }
            }
        }
        for (i, _) in text.match_indices(" as js__") {
            let rest = &text[i + 4..];
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            set.insert(name);
        }
    }
    set
}

#[derive(Default)]
struct Stats {
    stubs: Vec<String>,
    used: Vec<String>,
    variadic: Vec<String>,
    statics: Vec<String>,
}

fn flat_name(path: &[String], name: &str) -> String {
    // The module named `root` is bindgen's wrapper, not a namespace.
    let mut parts: Vec<&str> = path.iter().map(|s| s.as_str()).filter(|s| *s != "root").collect();
    parts.push(name);
    parts.join("__")
}

fn rewrite_items(items: &mut Vec<Item>, path: &mut Vec<String>, api: &BTreeSet<String>, stats: &mut Stats) {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        match item {
            Item::ForeignMod(fm) => {
                for fi in fm.items {
                    match fi {
                        ForeignItem::Fn(mut f) => {
                            let name = f.sig.ident.to_string();
                            let flat = flat_name(path, &name);
                            if f.sig.variadic.is_some() {
                                // Rust can't define C-variadic functions. No caller passes extra
                                // arguments once the fixed ones are enough (checked by compiling).
                                f.sig.variadic = None;
                                stats.variadic.push(flat.clone());
                            }
                            if api.contains(&flat) {
                                let flat_id = format_ident!("{}", flat);
                                let name_id = format_ident!("{}", name);
                                stats.used.push(flat);
                                // The implementation must have the signature bindgen read from the C++ header.
                                let inputs = f.sig.inputs.iter().map(|a| match a {
                                    syn::FnArg::Typed(t) => (*t.ty).clone(),
                                    other => panic!("receiver in {}", quote!(#other)),
                                });
                                let output = &f.sig.output;
                                out.push(parse_quote! {
                                    const _: unsafe extern "C" fn(#(#inputs),*) #output = crate::engine::api::#flat_id;
                                });
                                if flat_id == name_id {
                                    out.push(parse_quote!(pub use crate::engine::api::#flat_id;));
                                } else {
                                    out.push(parse_quote!(pub use crate::engine::api::#flat_id as #name_id;));
                                }
                            } else {
                                stats.stubs.push(flat.clone());
                                let vis = &f.vis;
                                let mut sig = f.sig.clone();
                                sig.unsafety = Some(Default::default());
                                sig.abi = Some(parse_quote!(extern "C"));
                                out.push(parse_quote! {
                                    #[allow(unused_variables)]
                                    #vis #sig { crate::engine::missing(#flat) }
                                });
                            }
                        }
                        ForeignItem::Static(s) => {
                            let flat = flat_name(path, &s.ident.to_string());
                            let (id, ty, mutability, vis) = (&s.ident, &s.ty, &s.mutability, &s.vis);
                            if api.contains(&flat) {
                                let flat_id = format_ident!("{}", flat);
                                out.push(parse_quote!(#vis use crate::engine::api::#flat_id as #id;));
                            } else {
                                stats.statics.push(flat);
                                let init = quote!(unsafe { ::core::mem::MaybeUninit::zeroed().assume_init() });
                                let _ = mutability; out.push(parse_quote!(#vis static mut #id: #ty = #init;));
                            }
                        }
                        other => panic!("foreign item {:?}", quote!(#other).to_string()),
                    }
                }
            }
            Item::Mod(mut m) => {
                if let Some((_, inner)) = m.content.as_mut() {
                    path.push(m.ident.to_string());
                    rewrite_items(inner, path, api, stats);
                    path.pop();
                }
                out.push(Item::Mod(m));
            }
            other => out.push(other),
        }
    }
    *items = out;
}

fn generate(input: &Path, output: &Path, prefix: &[&str], api: &BTreeSet<String>, stats: &mut Stats) {
    let src = fs::read_to_string(input).unwrap();
    let mut file = syn::parse_file(&src).unwrap();
    // Doc comments are 60% of the bindings and nobody reads them here.
    file.attrs.clear();
    strip_docs(&mut file.items);
    let mut path: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
    rewrite_items(&mut file.items, &mut path, api, stats);
    let header = "// @generated by tools/gen-jsapi from mozjs_sys's bindgen output. Do not edit.\n";
    fs::write(output, format!("{header}{}", prettyplease::unparse(&file))).unwrap();
}

fn strip_docs(items: &mut Vec<Item>) {
    use syn::visit_mut::{self, VisitMut};
    struct V;
    impl VisitMut for V {
        fn visit_attributes_mut(&mut self, attrs: &mut Vec<syn::Attribute>) {
            attrs.retain(|a| !a.path().is_ident("doc") && !a.path().is_ident("link_name"));
        }
        fn visit_item_foreign_mod_mut(&mut self, i: &mut syn::ItemForeignMod) {
            visit_mut::visit_item_foreign_mod_mut(self, i);
        }
    }
    let mut v = V;
    for it in items.iter_mut() {
        v.visit_item_mut(it);
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let [_, bindgen, engine, out] = &args[..] else {
        panic!("usage: gen-jsapi <bindgen dir> <engine src dir> <out dir>")
    };
    let api = implemented(Path::new(engine));
    let mut stats = Stats::default();
    generate(&Path::new(bindgen).join("jsapi.rs"), &Path::new(out).join("jsapi_gen.rs"), &[], &api, &mut stats);
    generate(
        &Path::new(bindgen).join("gluebindings.rs"),
        &Path::new(out).join("glue_gen.rs"),
        &["glue"],
        &api,
        &mut stats,
    );
    stats.stubs.sort();
    fs::write(Path::new(out).join("missing_api.txt"), stats.stubs.join("\n") + "\n").unwrap();
    let unknown: Vec<_> = api.iter().filter(|n| !stats.used.contains(n)).collect();
    println!(
        "{} implemented, {} stubs, {} variadic trimmed, {} zeroed statics",
        stats.used.len(),
        stats.stubs.len(),
        stats.variadic.len(),
        stats.statics.len()
    );
    println!("variadic: {:?}", stats.variadic);
    if !unknown.is_empty() {
        println!("in the engine but not in the bindings (helpers?): {unknown:?}");
    }
}
