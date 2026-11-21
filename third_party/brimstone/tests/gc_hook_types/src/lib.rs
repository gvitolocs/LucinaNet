//! Compile-time proofs that a hook the garbage collector calls cannot allocate on the JS heap, make
//! handles from the context, run script or ask for a GC (`docs/gc-hooks-cannot-allocate.md`,
//! `docs/engine-invariants.md` row 6d). Run with `cargo test -p brimstone_gc_hook_types --doc`.
//!
//! Each `compile_fail` case names the rustc error it must fail with, so a case cannot pass because
//! of an unrelated mistake in the snippet; [`HookThatObeysTheRule`] is the control: the same host
//! shape, without the forbidden call, must compile.
//!
//! The cases are the reproductions of hook-allocation-lost (an allocation from `after_gc_weak`,
//! `finalize_host_object` or `trace_host_object` lands in the from-space) and hook-allocation-freed (the same during
//! a resizing collection is a use after free), written as a host would write them.

/// The control: a host that traces, updates weak slots and reads a dying object's slots compiles.
///
/// ```
/// use std::{cell::RefCell, ffi::c_void};
///
/// use brimstone_core::runtime::embed::{GcHookCx, Host, HostObjectRef, RootVisitor, WeakUpdater};
///
/// struct Recorder {
///     roots: RefCell<Vec<*mut c_void>>,
///     weak: RefCell<Vec<u64>>,
///     finalized: RefCell<Vec<u64>>,
/// }
///
/// impl Host for Recorder {
///     fn trace_roots(&self, _: GcHookCx<'_>, visitor: &mut dyn RootVisitor) {
///         for slot in self.roots.borrow_mut().iter_mut() {
///             visitor.cell(slot);
///         }
///     }
///
///     fn trace_host_object(
///         &self,
///         _: GcHookCx<'_>,
///         object: HostObjectRef,
///         visitor: &mut dyn RootVisitor,
///     ) {
///         let mut bits = object.reserved_slot(0).as_raw_bits();
///         visitor.value(&mut bits);
///     }
///
///     fn after_gc_weak(&self, _: GcHookCx<'_>, updater: &mut dyn WeakUpdater) {
///         for bits in self.weak.borrow_mut().iter_mut() {
///             updater.update_value(bits);
///         }
///     }
///
///     fn finalize_host_object(&self, _: GcHookCx<'_>, object: HostObjectRef) {
///         self.finalized
///             .borrow_mut()
///             .push(object.reserved_slot(0).as_raw_bits());
///     }
/// }
/// ```
pub struct HookThatObeysTheRule;

/// hook-allocation-lost through `after_gc_weak`: the reproduction allocated a string from the weak pass. The hook's
/// context is not a `Context`.
///
/// ```compile_fail,E0308
/// use brimstone_core::runtime::embed::{GcHookCx, Host, WeakUpdater, api};
///
/// struct Allocating;
///
/// impl Host for Allocating {
///     fn after_gc_weak(&self, cx: GcHookCx<'_>, _: &mut dyn WeakUpdater) {
///         let _ = api::string_from_code_units(cx, &[0x41]);
///     }
/// }
/// ```
pub struct AfterGcWeakCannotAllocateAString;

/// hook-allocation-lost and hook-allocation-freed through `finalize_host_object`: a raw allocation on the moving heap. The hook's
/// context has no allocator.
///
/// ```compile_fail,E0599
/// use brimstone_core::runtime::embed::{GcHookCx, Host, HostObjectRef};
///
/// struct Allocating;
///
/// impl Host for Allocating {
///     fn finalize_host_object(&self, cx: GcHookCx<'_>, _: HostObjectRef) {
///         let _ = cx.alloc_uninit_with_size::<u64>(64);
///     }
/// }
/// ```
pub struct FinalizeHostObjectCannotAllocate;

/// The live-trace hook cannot reach a realm (and through it, script).
///
/// ```compile_fail,E0599
/// use brimstone_core::runtime::embed::{GcHookCx, Host, HostObjectRef, RootVisitor};
///
/// struct RunsScript;
///
/// impl Host for RunsScript {
///     fn trace_host_object(&self, cx: GcHookCx<'_>, _: HostObjectRef, _: &mut dyn RootVisitor) {
///         let _ = cx.initial_realm();
///     }
/// }
/// ```
pub struct TraceHostObjectCannotReachARealm;

/// No hook may ask for a GC.
///
/// ```compile_fail,E0308
/// use brimstone_core::runtime::embed::{self, GcHookCx, Host, RootVisitor};
///
/// struct Collecting;
///
/// impl Host for Collecting {
///     fn trace_roots(&self, cx: GcHookCx<'_>, _: &mut dyn RootVisitor) {
///         embed::gc(cx);
///     }
/// }
/// ```
pub struct TraceRootsCannotCollect;

/// The context inside the token is private.
///
/// ```compile_fail,E0616
/// use brimstone_core::runtime::embed::{GcHookCx, Host, WeakUpdater};
///
/// struct Unwrapping;
///
/// impl Host for Unwrapping {
///     fn after_gc_weak(&self, cx: GcHookCx<'_>, _: &mut dyn WeakUpdater) {
///         let _ = cx.cx;
///     }
/// }
/// ```
pub struct GcHookCxDoesNotExposeTheContext;

/// The old hook shape, a `Context` parameter (what the glue has before its pin bump), is not an
/// implementation of the trait any more.
///
/// ```compile_fail,E0053
/// use brimstone_core::runtime::{Context, embed::{Host, WeakUpdater}};
///
/// struct OldShape;
///
/// impl Host for OldShape {
///     fn after_gc_weak(&self, cx: Context, _: &mut dyn WeakUpdater) {
///         let _ = cx.alloc_uninit_with_size::<u64>(64);
///     }
/// }
/// ```
pub struct TheContextTakingHookShapeIsGone;

/// The token cannot be kept past the hook call (to be used by a later hook or after the collection).
///
/// ```compile_fail,E0521
/// use std::cell::Cell;
///
/// use brimstone_core::runtime::embed::{GcHookCx, Host, WeakUpdater};
///
/// struct Keeping(Cell<Option<GcHookCx<'static>>>);
///
/// impl Host for Keeping {
///     fn after_gc_weak(&self, cx: GcHookCx<'_>, _: &mut dyn WeakUpdater) {
///         self.0.set(Some(cx));
///     }
/// }
/// ```
pub struct GcHookCxCannotBeKept;
