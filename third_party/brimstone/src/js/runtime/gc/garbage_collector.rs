use std::{ops::Range, ptr::NonNull};

use super::sizing;
use super::weak_rules::{
    self, CellAction, ListAction, MapEntryAction, RootsEntryAction, ShapeEdgeAction, SweepAction,
    SweptSpace, TransitionFixup, TransitionTag, WeakKind,
};
use crate::runtime::{
    Context, HeapItemKind, PropertyKey, Value,
    bytecode::cache::{Cache, CacheArray, PolymorphicCacheArray},
    embed::{self, HostFlags, HostHeader, HostRegionBases},
    gc::{
        AnyHeapItem, Heap, HeapItem, HeapPtr, HeapVisitor,
        heap_item::{byte_size_for_kind, for_each_heap_item, visit_pointers_for_kind},
    },
    interned_strings::{InternedStrings, InternedStringsSet},
    intrinsics::{
        finalization_registry_object::{FinalizationRegistryCells, FinalizationRegistryObject},
        weak_map_object::{WeakMapObject, WeakValueMap},
        weak_ref_object::WeakRefObject,
        weak_set_object::{WeakSetObject, WeakValueSet},
    },
    shape::{PrototypeObjectChildrenShapesVec, Shape, TransitionVec},
    shape_registry::TransitionTreeRootsMap,
    string_value::FlatString,
    transitions::{Transition, TransitionKind},
};

/// A Cheney-style semispace garbage collector. Partitions heap into from-space and to-space, and
/// copies from one partition to the other on each GC.
pub struct GarbageCollector {
    // Type of GC to run. Normal or grow.
    type_: GcType,

    // Bounds of the space we are copying from
    from_space_bounds: Range<*const u8>,

    // Old bounds of the permanent space. For non-resizing GCs this is the same as the new bounds.
    old_permanent_space_bounds: Range<*const u8>,

    // New bounds of the permanent space. For non-resizing GCs this is the same as the old bounds.
    new_permanent_space_bounds: Range<*const u8>,

    // Pointer to the next to-space item to fix
    fix_ptr: *const u8,

    // Pointer to the end of the allocated items in to-space. Is the address of the next allocated
    // item.
    alloc_ptr: *const u8,

    // Address of the first item in to-space. `fix_ptr` catches up with `alloc_ptr` once every live
    // item has been moved, so this is the other end of the range the new semispace holds.
    to_space_start_ptr: *const u8,

    // Intrusive list of all WeakRefs that have been visited during this GC
    weak_ref_list: Option<HeapPtr<WeakRefObject>>,

    // Intrusive list of all WeakSets that have been visited during this GC
    weak_set_list: Option<HeapPtr<WeakSetObject>>,

    // Intrusive list of all WeakMaps that have been visited during this GC
    weak_map_list: Option<HeapPtr<WeakMapObject>>,

    // Intrusive list of all FinalizationRegistries that have been visited during this GC
    finalization_registry_list: Option<HeapPtr<FinalizationRegistryObject>>,

    // Intrusive list of all PrototypeObjectChildrenShapesVec that have been visited during this GC
    weak_vec_list: Option<HeapPtr<PrototypeObjectChildrenShapesVec>>,

    // Intrusive list of all CacheArrays that have been visited during this GC
    cache_array_list: Option<HeapPtr<CacheArray>>,

    // Intrusive list of all PolymorphicCacheArrays that have been visited during this GC
    polymorphic_cache_array_list: Option<HeapPtr<PolymorphicCacheArray>>,

    // The context being collected. Used to call into the embedder host.
    cx: Context,

    // The regions of the non-moving host space, if an embedder host is installed. Host objects are
    // marked instead of moved.
    host_space: Option<HostRegionBases>,

    // Host objects that have been marked but whose fields have not been visited yet
    host_gray_stack: Vec<NonNull<HostHeader>>,
}

#[derive(Clone, Debug)]
pub enum GcType {
    /// Normal GC, swap semispaces in the heap.
    Normal,
    /// Grow the heap during this GC. Optionally specify an allocation size that must be supported,
    /// and the live bytes the new semispace must hold together with it (the size after the last
    /// collection when the grow is decided before collecting, else the size after this one).
    Grow { alloc_size: Option<usize>, live: usize },
    /// Halve the heap during this GC (not below the minimum heap size), when the live data uses
    /// little of the semispace.
    Shrink,
}

impl GarbageCollector {
    fn new(cx: Context, type_: GcType, new_heap: Option<&Heap>) -> GarbageCollector {
        let dest_heap = new_heap.unwrap_or(&cx.heap);

        // The to-space is either the next heap for a non-resizing GC, or the new starting
        // (aka current) heap for a resizing GC.
        let to_space_start_ptr = match new_heap {
            None => cx.heap.next_heap_bounds().start,
            Some(new_heap) => new_heap.current_heap_bounds().start,
        };

        GarbageCollector {
            type_,
            from_space_bounds: cx.heap.current_heap_bounds(),
            old_permanent_space_bounds: cx.heap.permanent_heap_bounds(),
            new_permanent_space_bounds: dest_heap.permanent_heap_bounds(),
            fix_ptr: to_space_start_ptr,
            alloc_ptr: to_space_start_ptr,
            to_space_start_ptr,
            weak_ref_list: None,
            weak_set_list: None,
            weak_map_list: None,
            finalization_registry_list: None,
            weak_vec_list: None,
            cache_array_list: None,
            polymorphic_cache_array_list: None,
            cx,
            host_space: embed::try_state(cx).map(|state| state.space.region_bases()),
            host_gray_stack: vec![],
        }
    }

    pub fn run(mut cx: Context, type_: GcType) {
        // A GC hook of the embedder (trace, weak update, finalize) may ask for another GC, by
        // calling `embed::gc` or by allocating. A nested GC would treat the outer GC's to-space as
        // unreachable and sweep host objects that are still live, so it does not run: the GC
        // already in progress collects. An allocation on the heap from a hook cannot be served
        // that way, so it is fatal instead (see `Heap::abort_alloc_during_gc`).
        if cx.gc_in_progress {
            return;
        }
        cx.gc_in_progress = true;
        // Close the allocation window for the duration of the collection, so that an allocation
        // from a GC hook takes the slow path and is rejected by `Heap::abort_alloc_during_gc`: it
        // would bump into the from-space, or into the buffer a resizing collection frees. The swap
        // opens the window on the new space and the after-verification walk closes it again, so
        // `reset_alloc_limit` below is the reopen, after the last host call of the collection.
        // `gc_collecting` marks exactly that interval: the containment is armed for a collection,
        // not for every `gc_in_progress`, because teardown finalizes host objects without
        // collecting anything.
        cx.gc_collecting = true;
        cx.heap.close_alloc_window();
        if cx.options.gc_verify {
            super::verify::verify_heap(cx, "before");
        }
        let log_start = cx.options.gc_log.then(std::time::Instant::now);
        let log_heap_before = cx.heap.heap_size();

        // Create the new heap size if this is a resizing GC
        let new_heap_size = Self::calculate_new_heap_size(cx, &type_);
        let new_heap = new_heap_size.map(|size| Heap::new_for_resize(&cx.heap, size));

        let mut gc = Self::new(cx, type_, new_heap.as_ref());

        if let Some(state) = embed::try_state(cx) {
            state.in_gc = true;
        }

        // Phase times for the GC log
        let mut phase = [0f64; 5];
        let mut mark = log_start;
        let mut lap = |i: usize, mark: &mut Option<std::time::Instant>| {
            if let Some(m) = mark {
                let now = std::time::Instant::now();
                phase[i] += (now - *m).as_secs_f64() * 1000.0;
                *mark = Some(now);
            }
        };

        // If resizing first move the entire permanent space from the old to new heap
        if gc.is_resizing() {
            gc.move_permanent_heap();
        }

        // Visit all roots and move them to the to-heap
        gc.visit_all_roots(cx);
        lap(0, &mut mark);

        // Then use set of moved roots to find all remaining live items and move them to the to-heap
        gc.move_all_live_items();
        lap(1, &mut mark);

        // All live items have been identified and moved. We can now prune dead weak references and
        // handle finalizers.
        gc.prune_weak_references_and_handle_finalizers(cx);
        lap(2, &mut mark);

        // Let the embedder update its weak pointers, then finalize and free dead host objects. Must
        // happen while the from-space is still intact.
        if gc.host_space.is_some() {
            embed::sweep_after_gc(cx, &mut gc);
        }
        lap(3, &mut mark);

        // In GC stress test mode, overwrite the old heap with 0x01 bytes to try to catch reads from
        // pointers to the old heap.
        #[cfg(feature = "gc_stress_test")]
        {
            if cx.heap.gc_stress_test && matches!(gc.type_, GcType::Normal) {
                let start = cx.heap.current_heap_bounds().start;
                unsafe {
                    std::ptr::write_bytes(start.cast_mut(), 0x01, cx.heap.bytes_allocated());
                }
            }
        }

        // Stress mode at run time (`Options::gc_stress_window`, for crash hunts in the embedder): the
        // same poisoning, without the compile-time feature.
        if cx.options.gc_stress_window.is_some() && matches!(gc.type_, GcType::Normal) {
            let start = cx.heap.current_heap_bounds().start;
            // SAFETY: the from-space holds `bytes_allocated` bytes from its start; everything live
            // was copied out and the embedder sweep above has run.
            unsafe {
                std::ptr::write_bytes(start.cast_mut(), 0x01, cx.heap.bytes_allocated());
            }
        }

        let resized = new_heap.is_some();
        match new_heap {
            // Normal GCs can simply swap the semispaces in the heap
            None => cx.heap.swap_heaps(gc.alloc_ptr),
            // If resizing then we must replace the old heap with the new heap
            Some(mut new_heap) => {
                new_heap.finish_resized_heap(gc.alloc_ptr);
                cx.heap = new_heap;
            }
        }

        cx.heap.record_gc(&gc.type_, resized);
        let host_bytes = embed::try_state(cx).map_or(0, |state| state.space.used_bytes());
        cx.heap.reset_accounting(host_bytes);

        // The verification walk calls the host trace hooks too, so it runs inside the containment:
        // the window the swap opened is shut again for it and `gc_collecting` is still set, so an
        // allocation from one of those hooks is refused like one from the collection itself instead
        // of served because the copy is over. `reset_alloc_limit` below is the single reopen.
        if cx.options.gc_verify {
            cx.heap.close_alloc_window();
            super::verify::verify_heap(cx, "after");
        }

        let min_window = Heap::min_window(cx);
        let stress_window = cx.options.gc_stress_window;
        cx.heap.reset_alloc_limit(min_window, stress_window);
        // The window is open again: an allocation on the moving heap is a mutator allocation from
        // here, while `gc_in_progress` stays set until the end for the nested-collection rule.
        cx.gc_collecting = false;
        if cx.options.gc_census {
            cx.heap.take_census();
        }
        // Stress mode keeps the poisoned from-space resident (released pages would read as zeros).
        // Every collection hands the free semispace back. SpiderMonkey's rule (decommit only outside high-frequency mode,
        // `accounting::should_release`) was measured on YouTube with the same AV1 stream (round r10, 6 runs each): kernel time
        // 1.96 against 2.05 s (noise), max RSS 745 against 707 MB. A copying heap faults the window in either way, so keeping the
        // free semispace resident only adds memory; the rule stays for shrinking collections (`embed::gc_shrinking`).
        if !resized && cx.options.release_free_semispace && cx.options.gc_stress_window.is_none() {
            cx.heap.release_next_semispace();
        }
        lap(4, &mut mark);

        if let Some(start) = log_start {
            let stats = cx.heap.gc_stats();
            let kind = match gc.type_ {
                GcType::Normal => "normal",
                GcType::Grow { .. } => "grow",
                GcType::Shrink => "shrink",
            };
            eprintln!(
                "jsgc n={} kind={kind} hf={} pause_ms={:.2} (roots {:.1} copy {:.1} weak {:.1} host {:.1} swap {:.1}) heap_mb={}->{} live_mb={:.1} released_total_mb={} retained_parse_mb={:.1} retained_scope_mb={:.1} pending_stubs={} reparsed={} reparse_ms={:.1} handles={} host_objects={} assoc_mb={:.1} host_mb={:.1}",
                stats.gc_count,
                stats.high_frequency as u8,
                start.elapsed().as_secs_f64() * 1000.0,
                phase[0],
                phase[1],
                phase[2],
                phase[3],
                phase[4],
                log_heap_before >> 20,
                cx.heap.heap_size() >> 20,
                stats.last_live_bytes as f64 / 1048576.0,
                stats.released_bytes >> 20,
                cx.lazy_functions.retained_parse_bytes() as f64 / 1048576.0,
                cx.lazy_functions.retained_scope_bytes() as f64 / 1048576.0,
                cx.lazy_functions.num_pending(),
                cx.lazy_functions.reparse.reparsed,
                cx.lazy_functions.reparse.reparse_ns as f64 / 1e6,
                cx.heap.info().handle_context().live_handles(),
                embed::try_state(cx).map_or(0, |state| state.space.num_objects()),
                cx.heap.accounting().associated_bytes as f64 / 1048576.0,
                embed::try_state(cx).map_or(0, |state| state.space.used_bytes()) as f64 / 1048576.0,
            );
        }

        #[cfg(feature = "handle_leak_trace")]
        if cx.options.gc_log {
            eprintln!(
                "jshandles n={}:\n{}",
                cx.heap.gc_stats().gc_count,
                super::handle::leak_trace::report()
            );
        }

        // The heap is consistent again, so the host may allocate (and collect) from here on
        cx.gc_in_progress = false;

        if gc.host_space.is_some() {
            embed::after_gc(cx);
        }
    }

    /// Whether this is a resizing GC.
    pub fn is_resizing(&self) -> bool {
        matches!(self.type_, GcType::Grow { .. } | GcType::Shrink)
    }

    /// Calculate the new heap size if this should be a resizing GC. Otherwise return None.
    fn calculate_new_heap_size(cx: Context, type_: &GcType) -> Option<usize> {
        let heap_size = cx.heap.heap_size();
        let max_heap = Heap::effective_max_heap(cx);
        match type_ {
            GcType::Grow { alloc_size, live } => {
                debug_assert!(
                    heap_size.is_power_of_two()
                        && max_heap.is_power_of_two()
                        && heap_size <= max_heap
                );
                Some(sizing::grow_size(
                    heap_size,
                    *live,
                    *alloc_size,
                    Heap::sizing_overhead(&cx.heap),
                    max_heap,
                ))
            }
            GcType::Shrink => {
                let min_heap = cx.options.min_heap_size;
                debug_assert!(
                    heap_size.is_power_of_two()
                        && min_heap.is_power_of_two()
                        && min_heap <= heap_size
                );
                sizing::shrink_size(heap_size, min_heap)
            }
            GcType::Normal => None,
        }
    }

    /// Visit all roots, moving roots to the to-space and fixing pointers to the root to reflect the
    /// new location.
    ///
    /// Roots may appear in the context or in the permanent space.
    fn visit_all_roots(&mut self, mut cx: Context) {
        cx.visit_roots_for_gc(self);
        self.visit_permanent_heap_roots();
    }

    /// Visit all the root items in the permanent heap, moving the roots to the to-heap and fixing
    /// pointers in the permanent heap to point to the new location of the root.
    ///
    /// The permanent heap may contain pointers into the semispaces if a permanent heap item was
    /// mutated.
    fn visit_permanent_heap_roots(&mut self) {
        for_each_heap_item(self.new_permanent_space_bounds.clone(), |item| {
            let kind = item.shape().kind();
            let byte_size = byte_size_for_kind(item, kind);

            visit_pointers_for_kind(item, self, kind);

            byte_size
        });
    }

    /// Move all remaining live items from the from-heap to the to-heap, fixing pointers to point to
    /// the to-heap.
    ///
    /// Iterates until all live items have been discovered. Liveness includes ephemeron handling.
    fn move_all_live_items(&mut self) {
        loop {
            loop {
                // Then start fixing all pointers in each new heap item until fix pointer catches
                // up with alloc pointer, meaning all live objects have been copied and pointers
                // have been fixed.
                while self.fix_ptr < self.alloc_ptr {
                    let new_heap_item =
                        HeapPtr::from_ptr(self.fix_ptr.cast_mut()).cast::<AnyHeapItem>();
                    AnyHeapItem::visit_pointers(new_heap_item, self);

                    // Increment fix pointer to point to next new heap item
                    let byte_size = AnyHeapItem::byte_size(new_heap_item);
                    let alloc_size = Heap::alloc_size_for_request_size(byte_size);
                    // SAFETY: The fix pointer stays within the allocated part of the to-space.
                    unsafe { self.fix_ptr = self.fix_ptr.add(alloc_size) }
                }

                // Marked host objects may reference more items to move. Repeat until both the
                // to-space and the host gray stack have been fully processed.
                if !self.visit_gray_host_objects() {
                    break;
                }
            }

            // Visit ephemeron values that are known to be live due to live keys. Keep iterating
            // until fixpoint where no new live keys are found.
            let found_live_weak_map_value = self.visit_live_weak_map_entries();
            let found_live_unregister_token = self.visit_live_finalization_registry_cells();

            if !found_live_weak_map_value && !found_live_unregister_token {
                break;
            }
        }
    }

    /// Move the entire contents of the permanent heap from the old bounds to the new bounds.
    fn move_permanent_heap(&self) {
        let mut dest_ptr = self.new_permanent_space_bounds.start;

        for_each_heap_item(self.old_permanent_space_bounds.clone(), |mut item| {
            // Read the kind and byte size before the shape is overwritten with a forwarding pointer
            let kind = item.shape().kind();
            let byte_size = byte_size_for_kind(item, kind);

            Self::move_heap_item(&mut item, &mut dest_ptr);

            byte_size
        });

        // Entire old permanent heap should have been copied 1:1 to the new permanent heap
        debug_assert!(dest_ptr == self.new_permanent_space_bounds.end);
    }

    #[inline]
    fn copy_or_fix_pointer(&mut self, heap_item: &mut HeapPtr<AnyHeapItem>) {
        let heap_item_ptr = heap_item.as_ptr() as *const _ as *const u8;

        // We only need to visit pointers in a space that has had its contents moved. This is
        // normally the to-space but may also include the permanent space if resizing.
        if !self.is_in_moved_space(heap_item_ptr) {
            // Host objects are never moved, only marked. Embedder roots may also contain pointers
            // that were already updated to the to-space, which need no work.
            if self.host_space.is_some() {
                if self.is_in_host_space(heap_item_ptr) {
                    self.mark_host_object(heap_item_ptr);
                }

                return;
            }

            // The only valid pointers are either null, uninitialized (aka NonNull::dangling()), or
            // pointers to the new permanent space.
            debug_assert!(
                self.is_in_new_permanent_space(heap_item_ptr)
                    || heap_item.as_ptr() == NonNull::dangling().as_ptr()
                    || heap_item.as_ptr().is_null()
            );

            return;
        }

        // Every heap item has a known shape
        let shape = heap_item.shape();

        // If shape is actually a forwarding pointer then simply rewrite pointer to old heap
        // item to instead point to new heap item.
        if let Some(forwarding_ptr) = decode_forwarding_pointer(shape) {
            *heap_item = forwarding_ptr;
            return;
        }

        // Move the heap item to the to-space. We are guaranteed to have enough space in the
        // to-space since all allocations fit into the from-space.
        let new_heap_item = Self::move_heap_item(heap_item, &mut self.alloc_ptr);

        // Rewrite pointer to old heap item to instead point to new heap item
        *heap_item = new_heap_item;

        // Type specific actions for live heap items. All weak objects must be collected in lists so
        // they can be traversed later.
        match weak_rules::list_action(holder_of(new_heap_item.shape().kind())) {
            ListAction::WeakRef => self.add_visited_weak_ref(new_heap_item.cast::<WeakRefObject>()),
            ListAction::WeakSet => self.add_visited_weak_set(new_heap_item.cast::<WeakSetObject>()),
            ListAction::WeakMap => self.add_visited_weak_map(new_heap_item.cast::<WeakMapObject>()),
            ListAction::FinalizationRegistry => self.add_visited_finalization_registry(
                new_heap_item.cast::<FinalizationRegistryObject>(),
            ),
            ListAction::WeakVec => {
                self.add_visited_weak_vec(new_heap_item.cast::<PrototypeObjectChildrenShapesVec>())
            }
            ListAction::CacheArray => {
                self.add_visited_cache_array(new_heap_item.cast::<CacheArray>())
            }
            ListAction::PolymorphicCacheArray => self
                .add_visited_polymorphic_cache_array(new_heap_item.cast::<PolymorphicCacheArray>()),
            ListAction::NoList => {}
        }
    }

    /// Move a heap item to a destination address. Leaves a forwarding pointer at the old heap item
    /// old heap item. Updates the destination address to point to the next heap-item-aligned
    /// address after the new heap item.
    ///
    /// Return the new heap item.
    #[inline]
    fn move_heap_item(
        heap_item: &mut HeapPtr<AnyHeapItem>,
        dest_ptr: &mut *const u8,
    ) -> HeapPtr<AnyHeapItem> {
        // Calculate size of heap item. Caller must ensure that there is enough space at the
        // destination to hold the moved item.
        let alloc_size = Heap::alloc_size_for_request_size(AnyHeapItem::byte_size(*heap_item));

        // Copy item from old to new heap, and bump alloc_ptr to point past new allocation
        let new_heap_item = HeapPtr::from_ptr(dest_ptr.cast_mut()).cast::<AnyHeapItem>();
        unsafe {
            std::ptr::copy_nonoverlapping::<u8>(
                heap_item.as_ptr().cast(),
                new_heap_item.as_ptr().cast(),
                alloc_size,
            );
        }

        // Overwrite the shape inside the old heap item to be a forwarding pointer to
        // new heap item.
        let forwarding_pointer = encode_forwarding_pointer(new_heap_item);
        heap_item.set_shape(forwarding_pointer);

        // Bump the `dest_ptr` to point to the next aligned address after the new heap item
        unsafe { *dest_ptr = dest_ptr.add(alloc_size) };

        new_heap_item
    }

    #[inline]
    fn is_in_from_space(&self, ptr: *const u8) -> bool {
        self.from_space_bounds.contains(&ptr)
    }

    /// Whether a pointer points into the host space. Always false if no host is installed.
    #[inline]
    fn is_in_host_space(&self, ptr: *const u8) -> bool {
        match &self.host_space {
            Some(host_space) => host_space.contains(ptr),
            None => false,
        }
    }

    /// Whether a host object is marked. Must only be called on host space pointers.
    #[inline]
    fn is_host_object_marked(ptr: *const u8) -> bool {
        // SAFETY: The caller guarantees `ptr` is a live host space object pointer, so it is
        // directly preceded by its header.
        unsafe { HostHeader::from_object_ptr(ptr) }.is_marked()
    }

    /// Mark a host object, pushing it onto the gray stack if it was not yet marked.
    fn mark_host_object(&mut self, ptr: *const u8) {
        // SAFETY: `ptr` is a live host space object pointer (checked via `is_in_host_space`), so
        // it is directly preceded by its header.
        let header = unsafe { HostHeader::from_object_ptr(ptr) };
        if !header.is_marked() {
            header.set_marked(true);
            self.host_gray_stack.push(NonNull::from(header));
        }
    }

    /// Visit the fields of every host object on the gray stack, forwarding the pointers they hold
    /// and calling the host trace hook. Return whether any host object was visited.
    fn visit_gray_host_objects(&mut self) -> bool {
        let mut visited_any = false;

        while let Some(header) = self.host_gray_stack.pop() {
            visited_any = true;

            // SAFETY: Headers on the gray stack belong to marked, allocated host objects.
            let header = unsafe { &mut *header.as_ptr() };
            let object = header.object();

            // Read the kind before the shape field is updated. The shape may already have been
            // moved, but its contents are intact in the from-space until the GC finishes.
            let kind = object.shape().kind();
            visit_pointers_for_kind(object, self, kind);
            header.visit_pointers(self);

            let flags = header.flags();
            if flags.contains(HostFlags::INTERNAL_CELL) {
                // An engine cell: its own trace hook, never the host's
                embed::trace_engine_cell(self.cx, object, self);
            } else if flags.contains(HostFlags::HAS_TRACE) {
                embed::trace_host_object(self.cx, object, self);
            }
        }

        visited_any
    }

    /// Resolve a weak pointer once liveness is final. Return the new location if the target is
    /// live, or None if it was collected.
    pub fn resolve_weak_pointer(
        &self,
        target: HeapPtr<AnyHeapItem>,
    ) -> Option<HeapPtr<AnyHeapItem>> {
        match self.resolve_weak(target) {
            WeakResolution::Live(new_location) => Some(new_location),
            WeakResolution::Dead => None,
        }
    }

    #[inline]
    fn is_in_old_permanent_space(&self, ptr: *const u8) -> bool {
        self.old_permanent_space_bounds.contains(&ptr)
    }

    #[inline]
    fn is_in_new_permanent_space(&self, ptr: *const u8) -> bool {
        self.new_permanent_space_bounds.contains(&ptr)
    }

    /// Whether a pointer is in a space that was moved. This includes the from space, as well as the
    /// permanent space if the GC is a growing GC.
    #[inline]
    fn is_in_moved_space(&self, ptr: *const u8) -> bool {
        weak_rules::is_moved(
            self.is_in_from_space(ptr),
            self.is_resizing(),
            self.is_in_old_permanent_space(ptr),
        )
    }

    // Add a weak ref to the linked list of weak refs that are live during this GC.
    fn add_visited_weak_ref(&mut self, mut weak_ref: HeapPtr<WeakRefObject>) {
        weak_ref.set_next_weak_ref(self.weak_ref_list);
        self.weak_ref_list = Some(weak_ref);
    }

    // Add a weak set to the linked list of weak sets that are live during this GC.
    fn add_visited_weak_set(&mut self, mut weak_set: HeapPtr<WeakSetObject>) {
        weak_set.set_next_weak_set(self.weak_set_list);
        self.weak_set_list = Some(weak_set);
    }

    // Add a weak map to the linked list of weak maps that are live during this GC.
    fn add_visited_weak_map(&mut self, mut weak_map: HeapPtr<WeakMapObject>) {
        weak_map.set_next_weak_map(self.weak_map_list);
        self.weak_map_list = Some(weak_map);
    }

    // Add a weak vec to the linked list of weak vecs that are live during this GC.
    fn add_visited_weak_vec(&mut self, mut weak_vec: HeapPtr<PrototypeObjectChildrenShapesVec>) {
        weak_vec.set_next_weak_vec(self.weak_vec_list);
        self.weak_vec_list = Some(weak_vec);
    }

    // Add a finalization registry to the linked list of finalization registries that are live
    // during this GC.
    fn add_visited_finalization_registry(
        &mut self,
        mut finalization_registry: HeapPtr<FinalizationRegistryObject>,
    ) {
        finalization_registry.set_next_finalization_registry(self.finalization_registry_list);
        self.finalization_registry_list = Some(finalization_registry);
    }

    // Add a CacheArray to the linked list of CacheArrays that are live during this GC.
    fn add_visited_cache_array(&mut self, mut cache_array: HeapPtr<CacheArray>) {
        cache_array.set_next_cache_array(self.cache_array_list);
        self.cache_array_list = Some(cache_array);
    }

    // Add a PolymorphicCacheArray to the linked list of PolymorphicCacheArrays that are live during
    // this GC.
    fn add_visited_polymorphic_cache_array(
        &mut self,
        mut polymorphic_cache_array: HeapPtr<PolymorphicCacheArray>,
    ) {
        polymorphic_cache_array.set_next_cache_array(self.polymorphic_cache_array_list);
        self.polymorphic_cache_array_list = Some(polymorphic_cache_array);
    }

    /// Resolve a weak pointer to a heap item, determining whether the target is live along with its
    /// current (possibly moved-to) location.
    ///
    /// Must only be called after all live items have been moved. A target in a moved space is
    /// live iff it left behind a forwarding pointer, and a target anywhere else (e.g. in the
    /// permanent space) was never moved and is always live.
    #[inline]
    fn resolve_weak(&self, target: HeapPtr<AnyHeapItem>) -> WeakResolution {
        match self.resolve_weak_addr(target) {
            Some(addr) => WeakResolution::Live(heap_ptr_from_addr(addr)),
            None => WeakResolution::Dead,
        }
    }

    /// The same decision as `resolve_weak`, in the address form `weak_rules::resolve` takes.
    #[inline]
    fn resolve_weak_addr(&self, target: HeapPtr<AnyHeapItem>) -> Option<usize> {
        let ptr = target.as_ptr().cast();

        let in_moved = self.is_in_moved_space(ptr);
        let in_host = self.is_in_host_space(ptr);
        // The mark is only readable for a host object, and only means anything for a target that
        // never moved.
        let host_marked = !in_moved && in_host && Self::is_host_object_marked(ptr);
        // A forwarding pointer only exists where the copy phase left one behind.
        let fwd = if in_moved {
            decode_forwarding_pointer(target.shape()).map(|location| location.as_ptr() as usize)
        } else {
            None
        };

        weak_rules::resolve(in_moved, in_host, host_marked, fwd, ptr as usize)
    }

    fn prune_weak_references_and_handle_finalizers(&mut self, cx: Context) {
        // Prune interned strings that are no longer referenced
        self.prune_weak_interned_strings(cx);

        // Prune shapes in the transition tree that are no longer referenced from the heap
        self.prune_weak_transition_tree(cx);

        // Then repair the weak edges of the items in the new semispace that those walks reach only
        // by name, which is where a shape detached from the transition tree hides
        self.prune_weak_edges_in_to_space();

        // Prune caches whose items are no longer referenced from the heap
        self.prune_weak_cache_arrays();
        self.prune_weak_polymorphic_cache_arrays();

        // Drop the lazy function entries of collected stubs, freeing their AST once no stub of its
        // program is left
        self.prune_weak_lazy_functions(cx);

        // Drop the texts of collected source files
        let mut source_cx = cx;
        source_cx
            .source_texts
            .sweep(|file| self.resolve_weak_pointer(file));

        // Fix the weak objects, handling objects that have been garbage collected
        self.fix_weak_refs();
        self.fix_weak_sets();
        self.fix_weak_maps();
        self.fix_weak_vecs();
        self.fix_finalization_registries(cx);
    }

    // Visit live weak refs and check if their targets are still live. Should only be called after
    // liveness of all items has been determined.
    fn fix_weak_refs(&mut self) {
        // Walk all live weak refs in the list
        let mut next_weak_ref = self.weak_ref_list;
        while let Some(mut weak_ref) = next_weak_ref {
            next_weak_ref = weak_ref.next_weak_ref();

            // Target may have already been cleared to undefined by a prior GC
            let target = weak_ref.weak_ref_target();
            if !target.is_pointer() {
                continue;
            }

            match self.resolve_weak(target.as_pointer()) {
                // Live references are updated to point to the new location
                WeakResolution::Live(target) => {
                    weak_ref.set_weak_ref_target(Value::heap_item(target))
                }
                // Target was garbage collected so reset target to undefined
                WeakResolution::Dead => weak_ref.set_weak_ref_target(Value::undefined()),
            }
        }
    }

    // Visit live weak sets and check if their values are still live. Should only be called after
    // liveness of all items has been determined.
    fn fix_weak_sets(&mut self) {
        // Walk all live weak sets in the list
        let mut next_weak_set = self.weak_set_list;
        while let Some(weak_set) = next_weak_set {
            next_weak_set = weak_set.next_weak_set();

            // Live references are updated to point to the new location, and values that were
            // garbage collected are removed from the set
            self.prune_weak_value_set(weak_set.weak_set_data());
        }
    }

    // Visit live weak maps and check if their keys are still live. Should only be called after
    // liveness of all items has been determined.
    fn fix_weak_maps(&mut self) {
        // Walk all live weak maps in the list
        let mut next_weak_map = self.weak_map_list;
        while let Some(weak_map) = next_weak_map {
            next_weak_map = weak_map.next_weak_map();

            for mut entry in weak_map.weak_map_data().iter_entries_mut_gc_unsafe() {
                let key = entry.key_mut().value_mut();
                debug_assert!(key.is_pointer());

                // Key was garbage collected so remove the entry from the map
                if let WeakResolution::Dead = self.resolve_weak(key.as_pointer()) {
                    entry.remove();
                }

                // Note that live keys and values were already updated to point to their new
                // locations when ephemerons were processed.
            }
        }
    }

    // Visit live weak maps and check if their keys are still live. Visit the values associated with
    // all live keys. Return whether any new live values were found.
    fn visit_live_weak_map_entries(&mut self) -> bool {
        // Walk all live weak maps in the list
        let mut next_weak_map = self.weak_map_list;
        let mut found_new_live_value = false;

        while let Some(weak_map) = next_weak_map {
            next_weak_map = weak_map.next_weak_map();
            for (weak_key, value) in weak_map.weak_map_data().iter_mut_gc_unsafe() {
                found_new_live_value |= self.visit_ephemeron(weak_key.value_mut(), Some(value));
            }
        }

        found_new_live_value
    }

    // Finalization registries are effectively a WeakMap from target value to unregister token.
    // Visit live finalization registries and check if their cells' targets are still live. Visit
    // the weakly held unregister token in each cell where the target is live. Return whether any
    // newly live unregister tokens were found.
    fn visit_live_finalization_registry_cells(&mut self) -> bool {
        // Walk all live weak maps in the list
        let mut next_finalization_registry = self.finalization_registry_list;
        let mut found_new_live_unregister_token = false;

        while let Some(finalization_registry) = next_finalization_registry {
            next_finalization_registry = finalization_registry.next_finalization_registry();

            for cell in finalization_registry.cells().iter_mut_gc_unsafe().flatten() {
                found_new_live_unregister_token |=
                    self.visit_ephemeron(&mut cell.target, cell.unregister_token.as_mut());
            }
        }

        found_new_live_unregister_token
    }

    /// Visit a single ephemeron - a weak key and an optional dependent value that is only live
    /// if the key is live.
    ///
    /// If the key is newly discovered to be live then update the key to point to its new location
    /// and visit the value. Keys in the permanent space are always live so their values are always
    /// visited.
    ///
    /// Return whether a dependent value was newly discovered to be live.
    fn visit_ephemeron(&mut self, key: &mut Value, value: Option<&mut Value>) -> bool {
        debug_assert!(key.is_pointer());
        let key_ptr = key.as_pointer().as_ptr().cast();

        // Check if key has not yet been visited and moved to the to-space
        if self.is_in_moved_space(key_ptr) {
            // Key is known to be live if it has already moved and left a forwarding pointer.
            // Visit the value associated with the key since we now know it is live. Also update
            // key to point into to-space so we can tell it is visited.
            if let Some(forwarding_ptr) = decode_forwarding_pointer(key.as_pointer().shape()) {
                *key = Value::heap_item(forwarding_ptr);

                if let Some(value) = value {
                    self.visit_value(value);
                    return true;
                }
            }
        } else if self.is_in_old_permanent_space(key_ptr) {
            // Key was in the permanent space so it is always live and its value is always visited.
            //
            // Note that we only need to visit the value if it is a pointer. Make sure to only
            // count this as a new live value if it was not already moved.
            if let Some(value) = value {
                if value.is_pointer() && self.is_in_moved_space(value.as_pointer().as_ptr().cast())
                {
                    let is_newly_live =
                        decode_forwarding_pointer(value.as_pointer().shape()).is_none();
                    self.visit_value(value);
                    return is_newly_live;
                }

                return self.visit_host_ephemeron_value(value);
            }
        } else if self.is_in_host_space(key_ptr) {
            // Host object keys are live iff marked. Their values may be visited on every fixpoint
            // iteration, so only count values that were not yet visited as newly live.
            if Self::is_host_object_marked(key_ptr) {
                if let Some(value) = value {
                    if value.is_pointer()
                        && self.is_in_moved_space(value.as_pointer().as_ptr().cast())
                    {
                        let is_newly_live =
                            decode_forwarding_pointer(value.as_pointer().shape()).is_none();
                        self.visit_value(value);
                        return is_newly_live;
                    }

                    return self.visit_host_ephemeron_value(value);
                }
            }
        }

        false
    }

    /// Visit an ephemeron value that may be an unmarked host object. Return whether it was newly
    /// marked.
    fn visit_host_ephemeron_value(&mut self, value: &mut Value) -> bool {
        if !value.is_pointer() {
            return false;
        }

        let value_ptr = value.as_pointer().as_ptr().cast();
        if !self.is_in_host_space(value_ptr) || Self::is_host_object_marked(value_ptr) {
            return false;
        }

        self.mark_host_object(value_ptr);
        true
    }

    // Visit live finalization registries and check if their cells' targets are still live. Should
    // only be called after liveness of all items has been determined.
    fn fix_finalization_registries(&mut self, mut cx: Context) {
        // Walk all live finalization registries in the list
        let mut next_finalization_registry = self.finalization_registry_list;
        while let Some(finalization_registry) = next_finalization_registry {
            next_finalization_registry = finalization_registry.next_finalization_registry();

            for cell_opt in finalization_registry.cells().iter_mut_gc_unsafe() {
                if let Some(cell) = cell_opt {
                    debug_assert!(cell.target.is_pointer());

                    // Target was garbage collected so enqueue the cleanup callback, passing in
                    // the held value, then remove the cell from the registry.
                    if let WeakResolution::Dead = self.resolve_weak(cell.target.as_pointer()) {
                        cx.task_queue().enqueue_callback_1_task(
                            finalization_registry.cleanup_callback().into(),
                            cell.held_value,
                        );

                        finalization_registry.cells().remove_cell(cell_opt);
                    }

                    // Note that live targets were already updated to point to their new locations
                    // when ephemerons were processed.
                }
            }
        }
    }

    /// Walk the intrusive list of live weak vecs and compress each one.
    fn fix_weak_vecs(&mut self) {
        // Walk all live weak vecs in the list
        let mut next_weak_vec = self.weak_vec_list;
        while let Some(weak_vec) = next_weak_vec {
            next_weak_vec = weak_vec.next_weak_vec();

            self.compress_weak_vec(weak_vec);
        }
    }

    /// Compress a `PrototypeObjectChildrenShapesVec` by removing elements that have been garbage
    /// collected.
    ///
    /// Vector is compressed in place by moving live elements down to fill the slots of dead
    /// elements. Note that backing array is never shrunk.
    fn compress_weak_vec(&self, mut weak_vec: HeapPtr<PrototypeObjectChildrenShapesVec>) {
        let len = weak_vec.len();

        let next_kept_index = weak_rules::compact(weak_vec.as_mut_slice(), len, |element| {
            match element {
                Some(element) => match self.resolve_weak(element.as_any()) {
                    WeakResolution::Live(new_location) => Some(Some(new_location.cast::<Shape>())),
                    WeakResolution::Dead => None,
                },
                // Prune removed values
                None => None,
            }
        });

        weak_vec.set_len(next_kept_index);
    }

    fn prune_weak_interned_strings(&mut self, cx: Context) {
        // First remove dead strings from the set of interned strings
        let string_set = InternedStrings::strings(cx);
        self.prune_interned_strings_set(string_set);

        // Then check values of interned strings map
        let mut cloned_cx = cx;
        let string_map = cloned_cx.interned_strings.generator_cache_mut();
        string_map.retain(|_, string_ref| self.resolve_weak_interned_string(string_ref));
    }

    /// Remove interned strings that are no longer referenced from one interned strings set, and fix
    /// the ones that moved.
    fn prune_interned_strings_set(&mut self, mut string_set: HeapPtr<InternedStringsSet>) {
        for mut entry in string_set.iter_entries_mut_gc_unsafe() {
            if !self.resolve_weak_interned_string(entry.key_mut()) {
                entry.remove();
            }
        }
    }

    /// Repair the weak edge one shape holds to its transition children: a single next shape, held
    /// weakly in the slot itself, or a transition vec, held strongly while its contents are weak.
    fn prune_shape_transitions(&mut self, mut shape: HeapPtr<Shape>) {
        let Some(transitions_or_next_shape) = shape.transitions_or_next_shape() else {
            return;
        };

        let has_transitions_vec = shape.has_transitions_vec();
        // The vec pointer is strong, so it is never resolved; the decision for a shape that holds
        // one ignores the resolution.
        let resolved = if has_transitions_vec {
            None
        } else {
            self.resolve_weak_addr(transitions_or_next_shape)
        };

        match weak_rules::shape_edge_action(has_transitions_vec, resolved) {
            // TransitionVecs are held strongly but their contents are held weakly.
            ShapeEdgeAction::CompressVec => {
                self.compress_transition_vec(transitions_or_next_shape.cast::<TransitionVec>())
            }
            // Shapes are held weakly
            ShapeEdgeAction::SetNextShape(new_location) => {
                shape.set_transitions_or_next_shape(Some(heap_ptr_from_addr(new_location)));
            }
            ShapeEdgeAction::Clear => {
                shape.set_transitions_or_next_shape(None);
            }
        }
    }

    /// Prune the dead shapes that the roots map of a transition tree holds weakly.
    fn prune_transition_tree_roots_map(&mut self, mut roots: HeapPtr<TransitionTreeRootsMap>) {
        for mut entry in roots.iter_entries_mut_gc_unsafe() {
            // Keep edge only if the shape is live
            let shape = self.resolve_weak_addr(entry.value_mut().as_any());

            // The prototype of a dead shape is not read: its entry is removed.
            let prototype = match shape {
                Some(_) => entry
                    .key_mut()
                    .prototype
                    .as_mut()
                    .map(|prototype| self.resolve_weak_addr(prototype.as_any())),
                None => None,
            };

            match weak_rules::roots_entry_action(shape, prototype) {
                // Prune dead shapes from the transition tree
                RootsEntryAction::Remove => entry.remove(),
                RootsEntryAction::UpdateShape { shape } => {
                    *entry.value_mut() = heap_ptr_from_addr(shape).cast();
                }
                RootsEntryAction::UpdateShapeAndPrototype { shape, prototype } => {
                    *entry.value_mut() = heap_ptr_from_addr(shape).cast();

                    // Prototype guaranteed to be live since it is referenced by shape
                    entry.key_mut().prototype = Some(heap_ptr_from_addr(prototype).cast());
                }
                RootsEntryAction::DeadPrototype => {
                    panic!("Live shape has a dead prototype in its transition tree");
                }
            }
        }
    }

    /// Resolve a weak reference to an interned string in place. Return whether the string is
    /// still live.
    fn resolve_weak_interned_string(&self, string_ref: &mut HeapPtr<FlatString>) -> bool {
        match self.resolve_weak(string_ref.as_any()) {
            WeakResolution::Live(new_location) => {
                *string_ref = new_location.cast();
                true
            }
            WeakResolution::Dead => false,
        }
    }

    /// Update the stubs in the lazy function registry, which holds them weakly.
    fn prune_weak_lazy_functions(&mut self, cx: Context) {
        let mut cloned_cx = cx;
        cloned_cx
            .lazy_functions
            .sweep(|stub| self.resolve_weak_pointer(stub));
    }

    /// Traverse the transition tree and prune all shapes that are no longer live.
    fn prune_weak_transition_tree(&mut self, cx: Context) {
        // First prune the initial level of the transition tree
        let roots = cx.shapes.transition_tree_roots_ptr();
        self.prune_transition_tree_roots_map(roots);

        // Then prune the remaining branches of the transition tree via a DFS traversal. The walk is
        // what reaches the shapes of the permanent space, which no collection moves.
        let mut stack = vec![];
        for (_, shape) in roots.iter_gc_unsafe() {
            stack.push(shape);
        }

        while let Some(shape) = stack.pop() {
            self.prune_shape_transitions(shape);

            // Descend into the children the repaired edge now names
            if let Some(transitions_or_next_shape) = shape.transitions_or_next_shape() {
                if shape.has_transitions_vec() {
                    let transitions_vec = transitions_or_next_shape.cast::<TransitionVec>();
                    for transition in transitions_vec.as_slice() {
                        stack.push(transition.next_shape_ptr());
                    }
                } else {
                    stack.push(transitions_or_next_shape.cast::<Shape>());
                }
            }
        }
    }

    /// Repair the weak edges that the walks above leave pointing into the vacated semispace, in
    /// every item they reach only by naming a single instance, or not at all.
    ///
    /// A shape detached from the transition tree - the private [[IsHTMLDDA]] shape an emulating
    /// object clones and every shape transitioned from it - is reachable only through its owner's
    /// `shape` edge, so the tree walk never repairs the next-shape slot it holds weakly (offset 56,
    /// strong only once `HAS_TRANSITIONS_VEC` is set). Prototype object shapes are not a second case
    /// of this: `clone_as_prototype_object_shape` clears the slot, and their mutators change the
    /// shape in place and return before anything reads it (the verifier checks they never hold one).
    ///
    /// A container that outgrew itself is moved only because a handle slot of the code that grew
    /// it still names the superseded instance: an interned strings set or a transition tree roots
    /// map (the named walks repair only the instance the context field points at), a transition vec
    /// (`maybe_grow_for_push`), the data of a WeakSet, a WeakMap or a FinalizationRegistry
    /// (`maybe_grow_for_insertion`; the lists repair only the data a live collection owns now).
    /// Nothing reads the superseded instance's entries, but a pointer into memory the next
    /// collection hands out is not something to leave in the heap. An outgrown one is resolved or
    /// cleared, never finalized: running a cleanup callback is the owner's business, from its own
    /// newer cells.
    ///
    /// Every other heap item with weak edges (weak refs, weak sets and maps, finalization
    /// registries, prototype object children vecs, cache arrays) is collected into a list by
    /// `copy_or_fix_pointer` when it moves, outgrown or not, and repaired from that list.
    ///
    /// The permanent space is swept as well. A collection never moves it, so it holds the
    /// containers outgrown there for good, and a resizing collection moves the whole space, which
    /// leaves their weak slots pointing into the old copy. It holds none of the list kinds (a test
    /// pins that), which would otherwise never be repaired.
    fn prune_weak_edges_in_to_space(&mut self) {
        // The data the live weak collections own, which the lists fix with their side effects
        let mut owned_data = rustc_hash::FxHashSet::default();
        let mut next_weak_set = self.weak_set_list;
        while let Some(weak_set) = next_weak_set {
            next_weak_set = weak_set.next_weak_set();
            owned_data.insert(weak_set.weak_set_data().as_ptr() as usize);
        }
        let mut next_weak_map = self.weak_map_list;
        while let Some(weak_map) = next_weak_map {
            next_weak_map = weak_map.next_weak_map();
            owned_data.insert(weak_map.weak_map_data().as_ptr() as usize);
        }
        let mut next_finalization_registry = self.finalization_registry_list;
        while let Some(finalization_registry) = next_finalization_registry {
            next_finalization_registry = finalization_registry.next_finalization_registry();
            owned_data.insert(finalization_registry.cells().as_ptr() as usize);
        }

        let to_space = self.to_space_start_ptr..self.alloc_ptr;
        let permanent_space = self.new_permanent_space_bounds.clone();

        for swept in weak_rules::swept_spaces() {
            let space = match swept {
                SweptSpace::NewPerm => permanent_space.clone(),
                SweptSpace::To => to_space.clone(),
            };

            for_each_heap_item(space, |item| {
                let kind = item.shape().kind();
                let owned = owned_data.contains(&(item.as_ptr() as usize));
                match weak_rules::sweep_action(holder_of(kind), owned) {
                    SweepAction::ShapeEdge => self.prune_shape_transitions(item.cast::<Shape>()),
                    SweepAction::PruneInternedStringsSet => {
                        self.prune_interned_strings_set(item.cast::<InternedStringsSet>())
                    }
                    SweepAction::PruneRootsMap => {
                        self.prune_transition_tree_roots_map(item.cast::<TransitionTreeRootsMap>())
                    }
                    // Compressing a vec a shape still names again is a no-op
                    SweepAction::CompressTransitionVec => {
                        self.compress_transition_vec(item.cast::<TransitionVec>())
                    }
                    SweepAction::PruneWeakValueSet => {
                        self.prune_weak_value_set(item.cast::<WeakValueSet>())
                    }
                    SweepAction::PruneWeakValueMap => {
                        self.prune_outgrown_weak_value_map(item.cast::<WeakValueMap>())
                    }
                    SweepAction::PruneFinalizationRegistryCells => self
                        .prune_outgrown_finalization_registry_cells(
                            item.cast::<FinalizationRegistryCells>(),
                        ),
                    SweepAction::Skip => {}
                }

                byte_size_for_kind(item, kind)
            });
        }
    }

    /// Resolve the values of a weak set's data in place, removing the dead ones.
    fn prune_weak_value_set(&self, mut set: HeapPtr<WeakValueSet>) {
        for mut entry in set.iter_entries_mut_gc_unsafe() {
            let value = entry.key_mut().value_mut();
            debug_assert!(value.is_pointer());

            match self.resolve_weak(value.as_pointer()) {
                WeakResolution::Live(new_location) => *value = Value::heap_item(new_location),
                WeakResolution::Dead => entry.remove(),
            }
        }
    }

    /// Resolve the keys and values of a weak map's outgrown data in place, removing an entry whose
    /// key or value is dead. Nothing traced its values (no ephemeron pass sees outgrown data), so a
    /// value is resolved like the key instead of being assumed live.
    fn prune_outgrown_weak_value_map(&self, mut map: HeapPtr<WeakValueMap>) {
        for mut entry in map.iter_entries_mut_gc_unsafe() {
            let key = {
                let key_slot = entry.key_mut().value_mut();
                debug_assert!(key_slot.is_pointer());
                self.resolve_weak_addr(key_slot.as_pointer())
            };

            // A value of an entry with a dead key is never read: the entry is removed.
            let (value_is_ptr, value) = match key {
                Some(_) => {
                    let value_slot = entry.value_mut();
                    if value_slot.is_pointer() {
                        (true, self.resolve_weak_addr(value_slot.as_pointer()))
                    } else {
                        (false, None)
                    }
                }
                None => (false, None),
            };

            match weak_rules::map_entry_action(key, value_is_ptr, value) {
                MapEntryAction::Remove => entry.remove(),
                MapEntryAction::UpdateKey { key } => {
                    *entry.key_mut().value_mut() = Value::heap_item(heap_ptr_from_addr(key));
                }
                MapEntryAction::UpdateKeyAndValue { key, value } => {
                    *entry.key_mut().value_mut() = Value::heap_item(heap_ptr_from_addr(key));
                    *entry.value_mut() = Value::heap_item(heap_ptr_from_addr(value));
                }
            }
        }
    }

    /// Resolve the targets and unregister tokens of outgrown finalization registry cells in place.
    /// A cell with a dead target is deleted without a cleanup callback: the registry that owned
    /// these cells has copied them, and it runs the callback from its own copy.
    fn prune_outgrown_finalization_registry_cells(
        &self,
        mut cells: HeapPtr<FinalizationRegistryCells>,
    ) {
        let mut counts = cells;
        for cell_opt in cells.iter_mut_gc_unsafe() {
            let Some(cell) = cell_opt else {
                continue;
            };
            debug_assert!(cell.target.is_pointer());

            let target = self.resolve_weak_addr(cell.target.as_pointer());

            // The token of a cell with a dead target is never read: the cell is deleted.
            let (token_is_ptr, token) = match target {
                Some(_) => match cell.unregister_token.as_mut() {
                    Some(token) if token.is_pointer() => {
                        (true, self.resolve_weak_addr(token.as_pointer()))
                    }
                    _ => (false, None),
                },
                None => (false, None),
            };

            match weak_rules::cell_action(target, token_is_ptr, token) {
                // Deleted without a cleanup callback: the registry that owned these cells has
                // copied them, and it runs the callback from its own copy.
                CellAction::DeleteCell => counts.remove_cell(cell_opt),
                CellAction::UpdateTarget { target } => {
                    cell.target = Value::heap_item(heap_ptr_from_addr(target));
                }
                CellAction::UpdateTargetAndToken { target, token } => {
                    cell.target = Value::heap_item(heap_ptr_from_addr(target));

                    // The decision only names this arm for a cell with a pointer token.
                    if let Some(token_slot) = cell.unregister_token.as_mut() {
                        *token_slot = Value::heap_item(heap_ptr_from_addr(token));
                    }
                }
                CellAction::UpdateTargetClearToken { target } => {
                    cell.target = Value::heap_item(heap_ptr_from_addr(target));
                    cell.unregister_token = None;
                }
            }
        }
    }

    /// Compress a TransitionVec in place by removing transitions to shapes that are no longer live.
    fn compress_transition_vec(&self, mut transition_vec: HeapPtr<TransitionVec>) {
        let len = transition_vec.len();

        let next_kept_index =
            weak_rules::compact(transition_vec.as_mut_slice(), len, |mut transition| {
                // Only transitions to live shapes are kept, compressing the TransitionVec in place
                if let WeakResolution::Live(new_shape_location) =
                    self.resolve_weak(transition.next_shape_ptr().as_any())
                {
                    let new_shape = new_shape_location.cast::<Shape>();
                    transition.set_next_shape(new_shape);
                    self.fix_transition_weak_slot(&mut transition);

                    Some(transition)
                } else {
                    None
                }
            });

        transition_vec.set_len(next_kept_index);
    }

    /// Fix up weak references within a kept transition itself. These are all guaranteed to be live
    /// since the live shape has a (transitive) strong reference to them. Which slot that is, is
    /// `weak_rules::transition_fixup`.
    fn fix_transition_weak_slot(&self, transition: &mut Transition) {
        let tag = match *transition.kind_mut() {
            TransitionKind::DefineProperty { .. } => TransitionTag::DefineProperty,
            TransitionKind::SetPrototype { .. } => TransitionTag::SetPrototype,
            TransitionKind::PreventExtensions => TransitionTag::PreventExtensions,
        };

        match weak_rules::transition_fixup(tag) {
            TransitionFixup::None => {}
            TransitionFixup::Key => {
                if let TransitionKind::DefineProperty { ref mut key, .. } = *transition.kind_mut() {
                    if let Some(heap_key) = key.as_heap_item_opt() {
                        if let WeakResolution::Live(new_key_location) = self.resolve_weak(heap_key)
                        {
                            *key = PropertyKey::new_from_heap_item_unchecked(new_key_location);
                        } else {
                            panic!("Live shape has a dead key in its transition tree");
                        }
                    }
                }
            }
            TransitionFixup::Prototype => {
                if let TransitionKind::SetPrototype { prototype: Some(ref mut prototype) } =
                    *transition.kind_mut()
                {
                    if let WeakResolution::Live(new_prototype_location) =
                        self.resolve_weak(prototype.as_any())
                    {
                        *prototype = new_prototype_location.cast();
                    } else {
                        panic!("Live shape has a dead prototype in its transition tree");
                    }
                }
            }
        }
    }

    /// Visit live CacheArrays and fix weak references that are still live. Prune caches that
    /// contain any dead weak references.
    fn prune_weak_cache_arrays(&mut self) {
        // Walk all live CacheArrays in the list
        let mut next_cache_array = self.cache_array_list;
        while let Some(mut cache_array) = next_cache_array {
            next_cache_array = cache_array.next_cache_array();

            for cache in cache_array.as_mut_slice() {
                if !self.fix_cache(cache) {
                    *cache = Cache::Uninitialized;
                }
            }
        }
    }

    /// Visit the weak references of a cache, fixing live references to point to their new location.
    ///
    /// Return whether the cache itself is still live.
    fn fix_cache(&self, cache: &mut Cache) -> bool {
        let mut fixer = CacheFixer { gc: self, is_any_dead: false };
        cache.visit_pointers(&mut fixer);

        // Cache is live iff there are no dead weak references within it
        !fixer.is_any_dead
    }

    /// Visit live PolymorphicCacheArrays and fix weak references that are still live. Prune caches
    /// that contain any dead weak references and compress the remaining array of live caches.
    fn prune_weak_polymorphic_cache_arrays(&mut self) {
        // Walk all live PolymorphicCacheArrays in the list
        let mut next_polymorphic_cache_array = self.polymorphic_cache_array_list;
        while let Some(mut polymorphic_cache_array) = next_polymorphic_cache_array {
            next_polymorphic_cache_array = polymorphic_cache_array.next_cache_array();

            let caches = polymorphic_cache_array.as_mut_slice();

            // The first uninitialized cache signals the end of the array.
            let len = caches
                .iter()
                .position(|cache| matches!(cache, Cache::Uninitialized))
                .unwrap_or(caches.len());

            // Only keep caches that do not contain any dead weak references, compressing the
            // PolymorphicCacheArray in place.
            let next_kept_index = weak_rules::compact(caches, len, |mut cache| {
                if self.fix_cache(&mut cache) {
                    Some(cache)
                } else {
                    None
                }
            });

            caches[next_kept_index..].fill(Cache::Uninitialized);
        }
    }
}

impl HeapVisitor for GarbageCollector {
    fn visit(&mut self, ptr: &mut HeapPtr<AnyHeapItem>) {
        self.copy_or_fix_pointer(ptr);
    }
}

/// The result of resolving a weak pointer once liveness of all items is known.
enum WeakResolution {
    /// The target is live, at this possibly-new location.
    Live(HeapPtr<AnyHeapItem>),
    /// The target was garbage collected.
    Dead,
}

/// The weak-holder class of a heap item kind, which the pure decisions of `weak_rules` are named
/// over. The map stays here because `HeapItemKind` is not a plain value the pure file can name.
fn holder_of(kind: HeapItemKind) -> WeakKind {
    match kind {
        HeapItemKind::Shape => WeakKind::Shape,
        HeapItemKind::TransitionVec => WeakKind::TransitionVec,
        HeapItemKind::TransitionTreeRootsMap => WeakKind::TransitionTreeRootsMap,
        HeapItemKind::InternedStringsSet => WeakKind::InternedStringsSet,
        HeapItemKind::WeakRefObject => WeakKind::WeakRefObject,
        HeapItemKind::WeakSetObject => WeakKind::WeakSetObject,
        HeapItemKind::WeakValueSet => WeakKind::WeakValueSet,
        HeapItemKind::WeakMapObject => WeakKind::WeakMapObject,
        HeapItemKind::WeakValueMap => WeakKind::WeakValueMap,
        HeapItemKind::FinalizationRegistryObject => WeakKind::FinalizationRegistryObject,
        HeapItemKind::FinalizationRegistryCells => WeakKind::FinalizationRegistryCells,
        HeapItemKind::PrototypeObjectChildrenShapesVec => {
            WeakKind::PrototypeObjectChildrenShapesVec
        }
        HeapItemKind::CacheArray => WeakKind::CacheArray,
        HeapItemKind::PolymorphicCacheArray => WeakKind::PolymorphicCacheArray,
        _ => WeakKind::NoWeak,
    }
}

/// An address the pure decisions returned, back into the heap pointer a slot write needs.
fn heap_ptr_from_addr(addr: usize) -> HeapPtr<AnyHeapItem> {
    HeapPtr::from_ptr(addr as *mut AnyHeapItem)
}

/// Resolves the weak pointers of a cache once liveness of all items is known, tracking if any
/// weak reference within the cache was dead.
struct CacheFixer<'a> {
    gc: &'a GarbageCollector,
    is_any_dead: bool,
}

impl HeapVisitor for CacheFixer<'_> {
    fn visit(&mut self, _ptr: &mut HeapPtr<AnyHeapItem>) {}

    fn visit_weak(&mut self, ptr: &mut HeapPtr<AnyHeapItem>) {
        match self.gc.resolve_weak(*ptr) {
            WeakResolution::Live(new_location) => *ptr = new_location,
            WeakResolution::Dead => self.is_any_dead = true,
        }
    }
}

// The first item in each heap item is a pointer, is either:
//   - A pointer to the item's shape if this item has not yet been copied to the from space
//   - A forwarding pointer to the address in the from space the item has been copied to
//
// Tag lowest bit of pointer to signal a forwarding pointer
const FORWARDING_POINTER_TAG: usize = 0x1;

fn decode_forwarding_pointer(shape: HeapPtr<Shape>) -> Option<HeapPtr<AnyHeapItem>> {
    let ptr_bits = shape.as_ptr() as usize;
    if ptr_bits & FORWARDING_POINTER_TAG == FORWARDING_POINTER_TAG {
        return Some(HeapPtr::from_ptr((ptr_bits ^ FORWARDING_POINTER_TAG) as *mut AnyHeapItem));
    }

    None
}

fn encode_forwarding_pointer(heap_ptr: HeapPtr<AnyHeapItem>) -> HeapPtr<Shape> {
    let ptr_bits = heap_ptr.as_ptr() as usize;
    HeapPtr::from_ptr((ptr_bits | FORWARDING_POINTER_TAG) as *mut Shape)
}
