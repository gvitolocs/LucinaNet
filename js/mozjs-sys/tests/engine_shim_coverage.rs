/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The static check of the engine edge wrap (docs/edge-wrap.md): every `extern "C"` function defined under
//! `src/engine/` carries `#[engine_shim]` (any form), unless it is in the ratchet `src/engine/edge-wrap-pending.txt` (not
//! wrapped yet; lines are only ever deleted, one slice at a time). The permanent exceptions carry
//! `#[engine_shim(exempt = "<reason>")]` (named in the code, so the ast-grep rule R3 sees an attribute) and must also be on
//! `EXEMPT` below; an exempt attribute on any other function fails.
//!
//! It fails on: an unwrapped function in neither list; a pending line whose function is wrapped or gone (delete it); an
//! exempt entry that no longer exists; a pending file that is not sorted or has duplicates; a shim that returns a
//! `c_char` pointer without `failure = ...` (C strings fail with null, element data with a dangling pointer, and
//! `c_char` is `i8` or `u8` by target); any `extern "C-unwind"` function (docs/edge-wrap.md section a).
//!
//! `EDGE_WRAP_PRINT_PENDING=1` prints what the pending file would list today (to create it, never to grow it).
//! The per-file inventory is printed with `--nocapture`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use syn::visit::Visit;

/// Functions that stay unwrapped, with the reason.
const EXEMPT: &[(&str, &str, &str)] = &[
    (
        "crash_bt.rs",
        "handler",
        "the fatal-signal handler: runs after the crash in signal context; it re-raises the signal itself",
    ),
    (
        "cx.rs",
        "JS_RequestInterruptCallback",
        "called from other threads (Servo's hang monitor) while the script thread runs; touches only the context's \
         Interrupt through a poison-tolerant lock, cannot panic, and must still work on a poisoned context (the wrap \
         itself calls it)",
    ),
    (
        "cx.rs",
        "JS_RequestInterruptCallbackCanWait",
        "as JS_RequestInterruptCallback, which it calls",
    ),
    (
        "func.rs",
        "glue__JS_CallArgsFromVp",
        "pure pointer arithmetic on the caller's vp; returns JS::CallArgs by value, which has no failure value",
    ),
    (
        "profile.rs",
        "flush_at_exit",
        "an atexit handler: runs after main returned, with no context",
    ),
];

/// Return types that are bindgen aliases of element types (`c_char` is `i8`/`u8`, the rooted vectors are `u8`): by type
/// they would get the element-data failure value (a dangling pointer), so their shims must write `failure = ...`.
const ALIASED_RETURNS: &[&str] = &["c_char", "PersistentRootedIdVector", "PersistentRootedObjectVector"];

/// Engine `extern "C"` functions that are tracers, write barriers, finalizers or weak-pointer updates. A collection
/// started in an engine frame above one of them may still be running, so a skipped body (a gated shim on a poisoned
/// thread) would free live objects under it: these must be `#[engine_shim(gc)]`, never a plain gated shim (review
/// change 3, docs/edge-wrap.md section d). The check below refuses a plain `#[engine_shim]` on any name here, by name,
/// so a slice cannot wrap one without marking it `gc`. `JS_AddExtraGCRootsTracer` / `JS_RemoveExtraGCRootsTracer` (cx.rs)
/// are deliberately NOT listed: they only add/remove a tracer from a list (SpiderMonkey's GC-callback setters) and the
/// design keeps them gated.
const MUST_BE_GC: &[(&str, &str)] = &[
    // gc.rs: the glue's Call*Tracer family (each reports one kind of slot to the collector).
    ("gc.rs", "glue__CallValueTracer"),
    ("gc.rs", "glue__CallIdTracer"),
    ("gc.rs", "glue__CallObjectTracer"),
    ("gc.rs", "glue__CallStringTracer"),
    ("gc.rs", "glue__CallSymbolTracer"),
    ("gc.rs", "glue__CallBigIntTracer"),
    ("gc.rs", "glue__CallScriptTracer"),
    ("gc.rs", "glue__CallFunctionTracer"),
    ("gc.rs", "glue__CallUnbarrieredObjectTracer"),
    ("gc.rs", "glue__CallObjectRootTracer"),
    ("gc.rs", "glue__CallValueRootTracer"),
    ("gc.rs", "glue__CallPropertyDescriptorTracer"),
    // gc.rs: the weak-pointer updates (a dead slot reads as null; the caller cannot tell that from "the object died").
    ("gc.rs", "JS_UpdateWeakPointerAfterGC"),
    ("gc.rs", "JS_UpdateWeakPointerAfterGCUnbarriered"),
    // gc.rs: the store-buffer write barriers.
    ("gc.rs", "JS__HeapObjectWriteBarriers"),
    ("gc.rs", "JS__HeapValueWriteBarriers"),
    ("gc.rs", "JS__HeapStringWriteBarriers"),
    ("gc.rs", "JS__HeapBigIntWriteBarriers"),
    ("gc.rs", "JS__HeapScriptWriteBarriers"),
    // trace_* hooks the collector calls (one per traced shape).
    ("promise.rs", "trace_tables"),
    ("values.rs", "trace_obj_vec"),
    ("values.rs", "trace_raw_value"),
    ("property.rs", "trace_id_vec"),
    ("proxy.rs", "trace_id_vec"),
    ("env_chain.rs", "trace_env_chain"),
    ("security.rs", "trace_policy"),
    // realm.rs: the global-object class trace hook. The collector calls it (Servo's Window class installs it as its
    // `JSClassOps::trace`), and it dispatches to the per-global `JSTraceOp` registered in `GLOBAL_TRACE`, so a skipped
    // body leaves the Window's fields untraced under a live collection. It does not match the `trace_*` naming rule, so
    // this list is the only thing that catches it.
    ("realm.rs", "JS_GlobalObjectTraceHook"),
    // finalize_* hooks that run at the finalize phase of a collection.
    ("module.rs", "finalize_module"),
    ("module.rs", "finalize_loading_state"),
];

/// Engine `extern "C"` functions that RETURN element data (a `*mut`/`*const` to `u8 u16 u32 u64 i8 i16 i32 i64 f32 f64`
/// or a Latin-1/char16 unit) and whose OWN miss path returns null. By type these get the element-data failure value
/// (`NonNull::dangling()` from `dangling_failure!`), which a caller that null-checks the return - exactly how these
/// functions report "no data" today - would read as a live pointer. So when a slice wraps one it MUST override the
/// failure value to null, matching the function's own miss path: `#[engine_shim(failure = ::std::ptr::null_mut())]`
/// (review change 5, docs/edge-wrap.md section b). The check below refuses a gated wrap of any name here without a
/// `failure = ...`. A dangling pointer stays correct ONLY where the caller feeds it to `slice::from_raw_parts` together
/// with a length the caller already knows is 0 - that is the two string char getters
/// (`JS_GetLatin1StringCharsAndLength`, `JS_GetTwoByteStringCharsAndLength`, read by `mozjs/src/conversions.rs` which
/// zeroes the length first and asserts a non-null pointer), and they are deliberately NOT listed here.
const NULL_DATA_RETURNS: &[(&str, &str)] = &[
    ("typed.rs", "JS__GetArrayBufferData"),
    ("typed.rs", "JS__GetArrayBufferMaybeSharedData"),
    ("typed.rs", "JS__GetSharedArrayBufferData"),
    ("typed.rs", "JS_GetArrayBufferViewFixedData"),
    ("typed.rs", "JS_GetInt8ArrayData"),
    ("typed.rs", "JS_GetUint8ArrayData"),
    ("typed.rs", "JS_GetUint8ClampedArrayData"),
    ("typed.rs", "JS_GetInt16ArrayData"),
    ("typed.rs", "JS_GetUint16ArrayData"),
    ("typed.rs", "JS_GetInt32ArrayData"),
    ("typed.rs", "JS_GetUint32ArrayData"),
    ("typed.rs", "JS_GetFloat16ArrayData"),
    ("typed.rs", "JS_GetFloat32ArrayData"),
    ("typed.rs", "JS_GetFloat64ArrayData"),
    ("typed.rs", "JS_GetBigInt64ArrayData"),
    ("typed.rs", "JS_GetBigUint64ArrayData"),
];

#[derive(Debug)]
struct Shim {
    file: String,
    name: String,
    abi: String,
    wrapped: bool,
    /// `#[engine_shim(exempt = "...")]`: left as written, allowed only for the names in `EXEMPT`.
    exempt_attr: bool,
    gc: bool,
    has_failure: bool,
    returns_c_char: bool,
}

struct Finder<'a> {
    file: &'a str,
    out: &'a mut Vec<Shim>,
}

struct HasIdent<'a>(&'a str, bool);

impl<'ast> Visit<'ast> for HasIdent<'_> {
    fn visit_ident(&mut self, i: &'ast syn::Ident) {
        if i == self.0 {
            self.1 = true;
        }
    }
}

impl Finder<'_> {
    fn check(&mut self, attrs: &[syn::Attribute], sig: &syn::Signature) {
        let Some(abi) = &sig.abi else { return };
        let abi = abi.name.as_ref().map_or_else(|| "C".to_owned(), |n| n.value());
        let shim_attr = attrs
            .iter()
            .find(|a| a.path().segments.last().is_some_and(|s| s.ident == "engine_shim"));
        let (mut gc, mut has_failure, mut exempt_attr) = (false, false, false);
        if let Some(a) = shim_attr {
            if let syn::Meta::List(list) = &a.meta {
                // The macro itself refuses anything but `gc` and `failure = <expr>`; the first word says which.
                let text = list.tokens.to_string();
                let first = text
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .find(|w| !w.is_empty());
                gc = first == Some("gc");
                has_failure = first == Some("failure");
                exempt_attr = first == Some("exempt");
            }
        }
        let returns_c_char = match &sig.output {
            syn::ReturnType::Default => false,
            syn::ReturnType::Type(_, ty) => ALIASED_RETURNS.iter().any(|name| {
                let mut v = HasIdent(name, false);
                v.visit_type(ty);
                v.1
            }),
        };
        self.out.push(Shim {
            file: self.file.to_owned(),
            name: sig.ident.to_string(),
            abi,
            wrapped: shim_attr.is_some() && !exempt_attr,
            exempt_attr,
            gc,
            has_failure,
            returns_c_char,
        });
    }
}

impl<'ast> Visit<'ast> for Finder<'_> {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.check(&f.attrs, &f.sig);
        syn::visit::visit_item_fn(self, f);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.check(&f.attrs, &f.sig);
        syn::visit::visit_impl_item_fn(self, f);
    }
}

fn engine_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engine")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn scan() -> Vec<Shim> {
    let dir = engine_dir();
    let mut files = Vec::new();
    rs_files(&dir, &mut files);
    files.sort();
    let mut shims = Vec::new();
    for path in files {
        let rel = path.strip_prefix(&dir).unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        let ast = syn::parse_file(&text).unwrap_or_else(|e| panic!("{rel}: {e}"));
        Finder {
            file: &rel,
            out: &mut shims,
        }
        .visit_file(&ast);
    }
    shims
}

fn pending_path() -> PathBuf {
    engine_dir().join("edge-wrap-pending.txt")
}

/// `file<TAB>name` lines; `#` comments and blank lines ignored.
fn read_pending() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(pending_path()).unwrap_or_default();
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (f, n) = l
                .split_once('\t')
                .unwrap_or_else(|| panic!("edge-wrap-pending.txt: not `file<TAB>name`: {l:?}"));
            (f.to_owned(), n.to_owned())
        })
        .collect()
}

#[test]
fn every_engine_extern_c_fn_is_wrapped() {
    let shims = scan();
    let listed = |s: &Shim| EXEMPT.iter().any(|(f, n, _)| *f == s.file && *n == s.name);
    let exempt = |s: &Shim| s.exempt_attr && listed(s);

    if std::env::var_os("EDGE_WRAP_PRINT_PENDING").is_some() {
        let mut lines: Vec<String> = shims
            .iter()
            .filter(|s| !s.wrapped && !exempt(s))
            .map(|s| format!("{}\t{}", s.file, s.name))
            .collect();
        lines.sort();
        lines.dedup();
        println!("----- edge-wrap-pending.txt -----");
        for l in lines {
            println!("{l}");
        }
        println!("----- end -----");
    }

    let pending = read_pending();
    let mut problems = Vec::new();

    let mut sorted = pending.clone();
    sorted.sort();
    sorted.dedup();
    if sorted != pending {
        problems.push("edge-wrap-pending.txt is not sorted or has duplicate lines".to_owned());
    }

    for s in &shims {
        if s.abi != "C" {
            problems.push(format!(
                "{}: {} is `extern \"{}\"`: engine shims are `extern \"C\"` (docs/edge-wrap.md a)",
                s.file, s.name, s.abi
            ));
            continue;
        }
        if s.exempt_attr && !listed(s) {
            problems.push(format!(
                "{}: `{}` uses #[engine_shim(exempt = ..)] but is not on this test's EXEMPT list",
                s.file, s.name
            ));
        }
        if listed(s) && !s.exempt_attr {
            problems.push(format!(
                "{}: `{}` is on the EXEMPT list: mark it #[engine_shim(exempt = \"<reason>\")]",
                s.file, s.name
            ));
        }
        let is_pending = pending.iter().any(|(f, n)| *f == s.file && *n == s.name);
        if !s.wrapped && !s.exempt_attr && !is_pending {
            problems.push(format!(
                "{}: `{}` is an extern \"C\" fn without #[engine_shim]",
                s.file, s.name
            ));
        }
        if s.wrapped && s.returns_c_char && !s.gc && !s.has_failure {
            problems.push(format!(
                "{}: `{}` returns an aliased pointer type ({ALIASED_RETURNS:?}): write #[engine_shim(failure = ::std::ptr::null_mut())] (or null())",
                s.file, s.name
            ));
        }
        if s.wrapped && !s.gc && MUST_BE_GC.iter().any(|(f, n)| *f == s.file && *n == s.name) {
            problems.push(format!(
                "{}: `{}` is a tracer, write barrier, finalizer or weak-pointer update: it must be a `gc` shim \
                 (#[engine_shim(gc)]), never a gated one (docs/edge-wrap.md d, review change 3)",
                s.file, s.name
            ));
        }
        if s.wrapped && !s.gc && !s.has_failure && NULL_DATA_RETURNS.iter().any(|(f, n)| *f == s.file && *n == s.name) {
            problems.push(format!(
                "{}: `{}` returns element data whose own miss path is null: the type default is a dangling pointer a \
                 null-checking caller reads as live; write #[engine_shim(failure = ::std::ptr::null_mut())] \
                 (docs/edge-wrap.md b, review change 5)",
                s.file, s.name
            ));
        }
    }
    for (f, n) in &pending {
        let found: Vec<&Shim> = shims.iter().filter(|s| s.file == *f && s.name == *n).collect();
        if found.is_empty() {
            problems.push(format!(
                "edge-wrap-pending.txt: {f}\t{n} no longer exists: delete the line"
            ));
        } else if found.iter().all(|s| s.wrapped || s.exempt_attr) {
            problems.push(format!(
                "edge-wrap-pending.txt: {f}\t{n} is wrapped now: delete the line"
            ));
        }
    }
    for (f, n, _) in EXEMPT {
        if !shims.iter().any(|s| s.file == *f && s.name == *n) {
            problems.push(format!("EXEMPT: {f} {n} no longer exists: delete the entry"));
        }
    }

    // The inventory, for the slice plan.
    let mut per_file: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new();
    for s in &shims {
        let e = per_file.entry(&s.file).or_default();
        e.0 += 1;
        if s.wrapped {
            e.1 += 1;
        } else if exempt(s) {
            e.3 += 1;
        } else {
            e.2 += 1;
        }
    }
    println!("file\ttotal\twrapped\tpending\texempt");
    let mut t = (0, 0, 0, 0);
    for (f, (a, b, c, d)) in &per_file {
        println!("{f}\t{a}\t{b}\t{c}\t{d}");
        t = (t.0 + a, t.1 + b, t.2 + c, t.3 + d);
    }
    println!("TOTAL\t{}\t{}\t{}\t{}", t.0, t.1, t.2, t.3);
    println!("gc shims: {}", shims.iter().filter(|s| s.gc).count());
    // A non-zero pending count means the wrap is partial, and a partial wrap is not safe to pin (section f, point 7):
    // this line is the number the pin gate reads. It must reach 0 (only the prototype + exempt functions left unwrapped)
    // before the fork is re-pinned.
    println!("pending lines in the ratchet: {}", pending.len());

    assert!(
        problems.is_empty(),
        "engine edge wrap coverage (docs/edge-wrap.md):\n{}",
        problems.join("\n")
    );
}
