//! Heap verification for crash hunts (`Options::gc_verify`; the glue's `LUCINANET_JS_GC_VERIFY=1`).
//!
//! Before and after each collection, every pointer held by a root, a heap item or a host object
//! must point at the start of a heap item in the permanent space or the current semispace, or at a
//! host object that has not been freed. A bad pointer is reported with what holds it (item kind,
//! address and field offset, or "root"); a walk that runs off an item boundary is reported as a
//! desync. Any failure aborts the process, so the log and a debugger show the state at that point.
//! Costs two walks of the heap per collection: for hunts only.

use std::{
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
};

use rustc_hash::FxHashSet;

use crate::runtime::{
    Context, HeapItemKind,
    embed::{self, HostFlags},
    gc::{
        AnyHeapItem, Heap, HeapPtr, HeapVisitor,
        heap_item::{byte_size_for_kind, visit_pointers_for_kind},
    },
    shape::Shape,
};

/// Number of verifications run in this process, for the occasional "ok" line.
static VERIFICATIONS: AtomicU64 = AtomicU64::new(0);

/// Allocations made during collections since the last verification.
static ALLOCS_DURING_GC: AtomicU64 = AtomicU64::new(0);

/// An allocation from a GC hook (trace, weak update, finalizer): the item lands in the from-space
/// and is dead when the collection ends. The first few are printed with a backtrace.
pub(crate) fn alloc_during_gc(size: usize) {
    let n = ALLOCS_DURING_GC.fetch_add(1, Ordering::Relaxed);
    static PRINTED: AtomicU64 = AtomicU64::new(0);
    if PRINTED.fetch_add(1, Ordering::Relaxed) < 4 {
        eprintln!(
            "jsverify: allocation of {size} bytes during a collection (#{}), it lands in the from-space:\n{}",
            n + 1,
            std::backtrace::Backtrace::force_capture()
        );
    }
}

/// Most bad pointers printed per verification.
const MAX_REPORTS: usize = 25;

/// The item starts of one contiguous range of the heap, one bit per alignment unit.
struct Starts {
    range: Range<usize>,
    unit: usize,
    bits: Vec<u64>,
}

impl Starts {
    fn new(range: Range<usize>, unit: usize) -> Self {
        let units = (range.end - range.start) / unit + 1;
        Starts { range, unit, bits: vec![0; units / 64 + 1] }
    }

    fn insert(&mut self, p: usize) {
        let i = (p - self.range.start) / self.unit;
        self.bits[i / 64] |= 1 << (i % 64);
    }

    fn is_start(&self, p: usize) -> bool {
        if !self.range.contains(&p) || !(p - self.range.start).is_multiple_of(self.unit) {
            return false;
        }
        let i = (p - self.range.start) / self.unit;
        (self.bits[i / 64] >> (i % 64)) & 1 == 1
    }
}

#[derive(Default)]
struct Report {
    items: usize,
    pointers: usize,
    bad: usize,
    /// Bad pointers by (holder kind or what, where they point), for the summary.
    by_class: rustc_hash::FxHashMap<String, usize>,
}

impl Report {
    fn fail(&mut self, when: &str, message: impl FnOnce() -> String) {
        self.bad += 1;
        if self.bad <= MAX_REPORTS {
            eprintln!("jsverify {when}: {}", message());
        }
    }

    fn count(&mut self, class: String) {
        *self.by_class.entry(class).or_default() += 1;
    }
}

/// Walk one range item by item, recording item starts. Stops at the first item whose shape does
/// not point into the heap or whose size runs past the range.
fn walk(
    range: Range<usize>,
    heap: &[Range<usize>],
    unit: usize,
    region: &str,
    when: &str,
    items: &mut Vec<(usize, HeapItemKind)>,
    report: &mut Report,
) -> Starts {
    let mut starts = Starts::new(range.clone(), unit);
    let mut prev: Option<(usize, HeapItemKind)> = None;
    let mut cur = range.start;
    while cur < range.end {
        let item = HeapPtr::<AnyHeapItem>::from_ptr(cur as *mut AnyHeapItem);
        let shape = item.shape().as_ptr() as usize;
        if !heap.iter().any(|r| r.contains(&shape)) {
            report.fail(when, || {
                format!(
                    "{region} space desync at {cur:#x}: shape field {shape:#x} is not in the heap (previous item {prev:x?})"
                )
            });
            break;
        }
        let kind = item.shape().kind();
        let size = Heap::alloc_size_for_request_size(byte_size_for_kind(item, kind));
        if size == 0 || cur + size > range.end {
            report.fail(when, || {
                format!(
                    "{region} space desync at {cur:#x}: {kind:?} of {size} bytes runs past {:#x} (previous item {prev:x?})",
                    range.end
                )
            });
            break;
        }
        starts.insert(cur);
        items.push((cur, kind));
        prev = Some((cur, kind));
        cur += size;
    }
    starts
}

/// Checks every pointer it visits against the item starts and the live host objects.
struct Checker<'a> {
    cx: Context,
    when: &'a str,
    permanent: &'a Starts,
    current: &'a Starts,
    other_semispace: Range<usize>,
    host_objects: &'a FxHashSet<usize>,
    /// The item whose fields are visited (0 for roots) and its kind.
    holder: usize,
    holder_kind: Option<HeapItemKind>,
    /// What is being visited, for the report: "root", "item", "host header", "host trace".
    what: &'static str,
    report: &'a mut Report,
    /// Second pass on failure: instead of checking, report every slot that points at one of these.
    find: Option<&'a FxHashSet<usize>>,
    /// Holders of bad pointers (first few, distinct), for the second pass.
    bad_holders: Vec<usize>,
}

impl Checker<'_> {
    fn label(&mut self, what: &'static str) {
        self.what = what;
        self.holder = 0;
        self.holder_kind = None;
    }
}

impl HeapVisitor for Checker<'_> {
    /// Weak pointers are checked like strong ones, in every item of both spaces, outgrown
    /// containers included: the collector repairs (or clears) every weak slot it leaves in the
    /// heap, whether or not anything reads it again.
    fn visit_weak(&mut self, ptr: &mut HeapPtr<AnyHeapItem>) {
        self.visit_common(ptr);
    }

    fn visit_common(&mut self, ptr: &mut HeapPtr<AnyHeapItem>) {
        let field = ptr as *mut HeapPtr<AnyHeapItem> as usize;
        // Read the slot as an integer: `HeapPtr` is non-null by type, so an optimized build would
        // drop a null test on `as_ptr()`, yet roots do hold null.
        // SAFETY: `field` is the address of the visited slot, a pointer-sized and aligned location.
        let p = unsafe { std::ptr::read_volatile(field as *const usize) };
        // Null and dangling (an uninitialized pointer) are what the collector also accepts.
        if p == 0 || p == std::ptr::NonNull::<AnyHeapItem>::dangling().as_ptr() as usize {
            return;
        }
        if let Some(targets) = self.find {
            if targets.contains(&p) {
                let (holder, kind, what) = (self.holder, self.holder_kind, self.what);
                eprintln!(
                    "jsverify {}: referrer of {p:#x}: {what} {kind:?} at {holder:#x} (slot {field:#x})",
                    self.when
                );
            }
            return;
        }
        self.report.pointers += 1;
        if self.permanent.is_start(p) || self.current.is_start(p) || self.host_objects.contains(&p)
        {
            return;
        }
        let place = if self.permanent.range.contains(&p) {
            "inside the permanent space, not at an item start"
        } else if self.current.range.contains(&p) {
            "inside the current semispace, not at an item start"
        } else if self.other_semispace.contains(&p) {
            "into the other semispace (not moved)"
        } else if embed::host_space_contains(self.cx, p) {
            "into the host space, not a live host object (freed?)"
        } else {
            "outside the heap"
        };
        let (holder, kind, what) = (self.holder, self.holder_kind, self.what);
        self.report.count(format!("{what} {kind:?} -> {place}"));
        if holder != 0 && self.bad_holders.len() < 3 && !self.bad_holders.contains(&holder) {
            self.bad_holders.push(holder);
        }
        let live_roots_map = self.cx.shapes.transition_tree_roots_ptr().as_ptr() as usize;
        let live_strings =
            crate::runtime::interned_strings::InternedStrings::strings(self.cx).as_ptr() as usize;
        let what = if holder != 0 && holder == live_roots_map {
            "item, the live transition roots map"
        } else if holder != 0 && holder == live_strings {
            "item, the live interned strings set"
        } else if holder != 0 && kind == Some(HeapItemKind::InternedStringsSet) {
            "item, NOT the live interned strings set"
        } else {
            what
        };
        self.report.fail(self.when, || {
            if holder == 0 {
                format!("pointer {p:#x} from a {what} (slot {field:#x}) points {place}")
            } else {
                let offset = field as isize - holder as isize;
                format!(
                    "pointer {p:#x} in {kind:?} at {holder:#x} ({what}, field offset {offset}) points {place}"
                )
            }
        });
    }
}

/// Visit every root (labelled by kind), every heap item and every host object with the checker.
fn traverse(
    cx: Context,
    checker: &mut Checker,
    items: &[(usize, HeapItemKind)],
    headers: &[std::ptr::NonNull<embed::HostHeader>],
) {
    let mut cx_roots = cx;
    cx_roots.visit_roots_for_verify(checker, |c: &mut Checker, what| c.label(what));
    embed::visit_roots_for_verify(cx, checker, |c: &mut Checker, what| c.label(what));

    for &(item, kind) in items {
        checker.what = if checker.permanent.range.contains(&item) {
            "permanent item"
        } else {
            "item"
        };
        checker.holder = item;
        checker.holder_kind = Some(kind);
        visit_pointers_for_kind(HeapPtr::from_ptr(item as *mut AnyHeapItem), checker, kind);
    }

    for header in headers {
        // SAFETY: the space lists only allocated, not yet freed host objects.
        let header = unsafe { &mut *header.as_ptr() };
        let object = header.object();
        let kind = object.shape().kind();
        checker.holder = object.as_ptr() as usize;
        checker.holder_kind = Some(kind);
        checker.what = "host object";
        visit_pointers_for_kind(object, checker, kind);
        checker.what = "host header";
        header.visit_pointers(checker);
        if header.flags().contains(HostFlags::HAS_TRACE) {
            checker.what = "host trace";
            embed::trace_host_object_for_verify(cx, object, checker);
        }
    }
}

/// Verify the heap; `when` is "before" or "after" (the collection). Aborts on any bad pointer.
pub(crate) fn verify_heap(cx: Context, when: &str) {
    let unit = Heap::alloc_size_for_request_size(1);
    let permanent = cx.heap.permanent_heap_bounds();
    let permanent = permanent.start as usize..permanent.end as usize;
    let current_start = cx.heap.current_heap_bounds().start as usize;
    let current = current_start..current_start + cx.heap.bytes_allocated();
    let other = cx.heap.next_heap_bounds();
    let other = other.start as usize..other.end as usize;

    let ranges = format!(
        "permanent {:#x}..{:#x} current {:#x}..{:#x} other {:#x}..{:#x}",
        permanent.start, permanent.end, current.start, current.end, other.start, other.end
    );
    let mut report = Report::default();
    let mut items = Vec::new();
    let heap = [permanent.clone(), current.clone()];
    let permanent_starts = walk(permanent, &heap, unit, "permanent", when, &mut items, &mut report);
    let current_starts = walk(current, &heap, unit, "current", when, &mut items, &mut report);
    report.items = items.len();

    let headers = embed::host_headers_for_verify(cx);
    let host_objects: FxHashSet<usize> = headers
        .iter()
        .map(|h| unsafe { h.as_ref() }.object().as_ptr() as usize)
        .collect();

    // A prototype object shape is detached from the transition tree and changes in place, so it
    // never holds a transition edge (it is repaired only by the pass over the swept spaces if it
    // did, and read by nothing that expects one).
    for &(item, kind) in &items {
        if kind != HeapItemKind::Shape {
            continue;
        }
        let shape = HeapPtr::<AnyHeapItem>::from_ptr(item as *mut AnyHeapItem).cast::<Shape>();
        if shape.is_prototype_object() && shape.transitions_or_next_shape().is_some() {
            report.fail(when, || {
                format!("prototype object shape at {item:#x} holds a transition edge")
            });
        }
    }

    let mut checker = Checker {
        cx,
        when,
        permanent: &permanent_starts,
        current: &current_starts,
        other_semispace: other,
        host_objects: &host_objects,
        holder: 0,
        holder_kind: None,
        what: "root",
        report: &mut report,
        find: None,
        bad_holders: Vec::new(),
    };

    traverse(cx, &mut checker, &items, &headers);

    // On failure, a second pass names whatever still references the holders of bad pointers.
    let bad_holders = std::mem::take(&mut checker.bad_holders);
    if !bad_holders.is_empty() {
        let targets: FxHashSet<usize> = bad_holders.iter().copied().collect();
        checker.find = Some(&targets);
        traverse(cx, &mut checker, &items, &headers);
        checker.find = None;
    }

    for message in duplicate_host_slot_messages(&headers) {
        report.fail(when, || message);
    }

    let during = ALLOCS_DURING_GC.swap(0, Ordering::Relaxed);
    if during > 0 {
        report.fail(when, || {
            format!("{during} allocations during the collection (into the from-space)")
        });
    }

    let n = VERIFICATIONS.fetch_add(1, Ordering::Relaxed) + 1;
    if report.bad > 0 {
        eprintln!(
            "jsverify {when} n={n}: FAILED, {} bad of {} pointers in {} items and {} host objects; aborting",
            report.bad,
            report.pointers,
            report.items,
            headers.len()
        );
        eprintln!("jsverify {when} n={n}: {ranges}");
        let mut classes: Vec<_> = report.by_class.iter().collect();
        classes.sort_by(|a, b| b.1.cmp(a.1));
        for (class, count) in classes.iter().take(15) {
            eprintln!("jsverify {when} n={n}:   {count:>7} {class}");
        }
        std::process::abort();
    }
    if n % 100 == 1 {
        eprintln!(
            "jsverify {when} n={n}: ok, {} pointers in {} items and {} host objects",
            report.pointers,
            report.items,
            headers.len()
        );
    }
}

/// Reserved slot 0 holding the embedder's `PrivateValue(nullptr)`: the glue encodes a private
/// pointer as a double with the pointer's bits, and a double is stored bitwise negated, so a null
/// private is `!0` = all ones (the double +0.0). The browser writes it on purpose, to mean "no
/// native object": into a window proxy when it is created and into the old one after a
/// transplant, and its finalize and trace return early on it. Any number of live host objects may
/// hold it. No other bit pattern is skipped.
const NULL_PRIVATE_BITS: u64 = u64::MAX;

/// A slot value for the failure message: its kind, and for a number the decoded double (a private
/// pointer is stored as one).
fn describe_slot(slot: crate::runtime::value::Value) -> String {
    if slot.is_double() {
        format!("double bits {:#x}", slot.as_double().to_bits())
    } else if slot.is_smi() {
        format!("smi {}", slot.as_smi())
    } else {
        "other value".to_string()
    }
}

/// Invariant: one reflector per host-side object. Two live host objects with a finalizer whose
/// first reserved slot holds the same non-pointer value (Servo's DOM_OBJECT_SLOT private: the
/// Rust object's address) would both be finalized, and the Rust object dropped twice. Returns one
/// message per violation; the caller reports them.
pub(crate) fn duplicate_host_slot_messages(
    headers: &[std::ptr::NonNull<embed::HostHeader>],
) -> Vec<String> {
    let mut messages = vec![];
    let mut privates: rustc_hash::FxHashMap<u64, usize> = rustc_hash::FxHashMap::default();
    for header in headers {
        // SAFETY: as in `verify_heap`: the headers are live host space allocations.
        let header = unsafe { header.as_ref() };
        if !header.flags().contains(HostFlags::HAS_FINALIZE) || header.num_reserved_slots() == 0 {
            continue;
        }
        let slot = header.reserved_slots()[0];
        if slot.is_pointer() || slot.is_undefined() || slot.is_null() || slot.is_bool() {
            continue;
        }
        let bits = slot.as_raw_bits();
        if bits == NULL_PRIVATE_BITS {
            continue;
        }
        let object = header.object().as_ptr() as usize;
        if let Some(other) = privates.insert(bits, object) {
            messages.push(format!(
                "host objects {other:#x} and {object:#x} (class {:#x}, handler {:#x}) share reserved slot 0 ({bits:#x}, decoded as {}): two reflectors of one host object",
                header.class() as usize,
                header.handler() as usize,
                describe_slot(slot)
            ));
        }
    }
    messages
}

/// Every item of one space with its kind, in address order. For tests that inspect what a space
/// holds; the space must be walkable (not in the middle of a collection).
#[cfg(test)]
pub(crate) fn items_of_space(space: Range<*const u8>) -> Vec<(HeapPtr<AnyHeapItem>, HeapItemKind)> {
    let mut items = vec![];
    crate::runtime::gc::heap_item::for_each_heap_item(space, |item| {
        let kind = item.shape().kind();
        items.push((item, kind));
        byte_size_for_kind(item, kind)
    });
    items
}
