use std::{alloc::Layout, mem::size_of, ops::Range, ptr::NonNull};

use super::{accounting, sizing};
#[cfg(feature = "alloc_error")]
use crate::runtime::alloc_error::AllocError;
use crate::{
    common::{constants::MAX_HEAP_SIZE, serialized_heap::SerializedHeap},
    const_assert,
    runtime::{
        Context,
        alloc_error::AllocResult,
        gc::{
            GcType, HeapPtr, HeapVisitor,
            garbage_collector::GarbageCollector,
            handle::HandleContext,
            heap_item::{HeapItemKind, byte_size_for_kind, for_each_heap_item},
            heap_serializer::{HeapSpaceDeserializer, calculate_extra_offset},
        },
    },
    static_assert,
};

/// Heap Layout:
///
/// | HeapInfo | Permanent | Semispace 1 | Semispace 2 |
///
/// Permanent is an optional region used for objects that are never collected such as some builtins.
/// The rest of the heap is split into two semispaces which are used as the main heap.
///
/// Heap is aligned to a 1GB boundary and contains a reference to the context. This allows any heap
/// pointer to be masked to find the start of the heap along with its context.
pub struct Heap {
    /// Pointer to the start of the heap, where HeapInfo is stored
    heap_start: *const u8,
    /// Pointer to the end of the heap
    heap_end: *const u8,
    /// Pointer to the start of the permanent region
    permanent_start: *const u8,
    /// Pointer to the end of the permanent region
    permanent_end: *const u8,
    /// Pointer to the start of the current semispace
    start: *const u8,
    /// Pointer to where the next heap allocation will occur, grows as more allocations occur
    current: *const u8,
    /// Pointer to the end of the current semispace
    end: *const u8,
    /// Where bump allocation stops and the next collection runs, at most `end`: the live data of
    /// the last collection times SpiderMonkey's heap growth factor (`sizing::alloc_limit`). Pages
    /// past it are never touched, so they cost no memory.
    alloc_limit: *const u8,
    // Pointer to the start of the next semispace
    next_heap_start: *const u8,
    // Pointer to the end of the next semispace
    next_heap_end: *const u8,
    layout: Layout,
    /// Collection statistics, carried over when the heap is resized.
    stats: GcStats,
    /// Live heap items per kind (count, bytes) after the last collection, if the census is on.
    census: Vec<(Option<HeapItemKind>, usize, usize)>,

    #[cfg(feature = "gc_stress_test")]
    pub gc_stress_test: bool,
}

/// SpiderMonkey's GC tunables that LucinaJS honours (Servo sets them with JS_SetGCParameter), with SpiderMonkey's defaults
/// (gc/Scheduling.h). The copying heap's own window keeps its smaller factors (`sizing`); these drive the accounting triggers
/// (`accounting`), the high-frequency time and the heap's maximum.
#[derive(Clone, Copy, Debug)]
pub struct GcTunables {
    /// JSGC_MAX_BYTES: how far the heap may grow (bytes; unlimited by default).
    pub max_bytes: usize,
    /// JSGC_MALLOC_THRESHOLD_BASE: base of the associated-memory trigger (bytes).
    pub malloc_base: usize,
    /// JSGC_ALLOCATION_THRESHOLD: base of the host-space trigger (bytes).
    pub allocation_threshold: usize,
    /// JSGC_SMALL_HEAP_SIZE_MAX and JSGC_LARGE_HEAP_SIZE_MIN (bytes).
    pub small_heap: usize,
    pub large_heap: usize,
    /// JSGC_HIGH_FREQUENCY_SMALL_HEAP_GROWTH, _LARGE_HEAP_GROWTH and JSGC_LOW_FREQUENCY_HEAP_GROWTH, in tenths.
    pub high_small_tenths: usize,
    pub high_large_tenths: usize,
    pub low_tenths: usize,
    /// JSGC_HIGH_FREQUENCY_TIME_LIMIT (ms).
    pub high_frequency_ms: u64,
}

impl Default for GcTunables {
    fn default() -> Self {
        GcTunables {
            max_bytes: usize::MAX,
            malloc_base: accounting::MALLOC_BASE,
            allocation_threshold: accounting::HOST_BASE,
            small_heap: accounting::SMALL_HEAP,
            large_heap: accounting::LARGE_HEAP,
            high_small_tenths: accounting::HIGH_SMALL_TENTHS,
            high_large_tenths: accounting::HIGH_LARGE_TENTHS,
            low_tenths: accounting::LOW_TENTHS,
            high_frequency_ms: sizing::HIGH_FREQUENCY_MS,
        }
    }
}

/// Memory the copying heap does not hold, counted as SpiderMonkey counts it (`accounting`): the associated memory the embedder
/// reports, and the thresholds at which it and the host space make a collection due.
#[derive(Clone, Copy, Debug)]
pub struct Accounting {
    pub associated_bytes: usize,
    pub associated_threshold: usize,
    pub host_threshold: usize,
    pub tunables: GcTunables,
}

impl Default for Accounting {
    fn default() -> Self {
        let tunables = GcTunables::default();
        Accounting {
            associated_bytes: 0,
            associated_threshold: accounting::threshold(
                0,
                tunables.malloc_base,
                tunables.low_tenths,
            ),
            host_threshold: accounting::threshold(
                0,
                tunables.allocation_threshold,
                tunables.low_tenths,
            ),
            tunables,
        }
    }
}

/// Statistics of the collections a heap has seen.
#[derive(Clone, Copy, Debug, Default)]
pub struct GcStats {
    /// Associated memory, host-space and tunables (carried over when the heap is resized, with the rest).
    pub accounting: Accounting,
    /// Number of collections.
    pub gc_count: u64,
    /// Number of collections that grew the heap.
    pub grow_count: u64,
    /// Number of collections that shrank the heap.
    pub shrink_count: u64,
    /// Bytes of live data in the semispace right after the last collection.
    pub last_live_bytes: usize,
    /// Most live bytes seen right after any collection.
    pub peak_live_bytes: usize,
    /// Bytes handed back to the operating system by releasing free semispace pages, in total.
    pub released_bytes: u64,
    /// When the last collection ended, and whether it came less than `sizing::HIGH_FREQUENCY_MS`
    /// after the one before (the first collection counts as frequent).
    pub last_gc_at: Option<std::time::Instant>,
    pub high_frequency: bool,
}

/// Heap must be aligned to max heap size so that we can always mask heap pointers to find the start
/// of the heap.
pub const HEAP_ALIGNMENT: usize = MAX_HEAP_SIZE;
static_assert!(HEAP_ALIGNMENT.is_power_of_two());

/// The operating system's page size.
fn page_size() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: sysconf has no preconditions.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if size > 0 {
            return size as usize;
        }
    }
    4096
}

/// Tell the operating system that the pages in `start..start + len` (page aligned, inside the heap's
/// allocation) are no longer needed: their memory is freed and they read as zero when next touched.
/// Returns whether the pages were released.
fn release_pages(start: usize, len: usize) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: the range lies inside the heap's own anonymous allocation and holds no live data;
        // MADV_DONTNEED keeps the mapping and refills the pages with zeros on the next access.
        unsafe { libc::madvise(start as *mut libc::c_void, len, libc::MADV_DONTNEED) == 0 }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = (start, len);
        false
    }
}

/// All heap items are aligned to 8-byte boundaries
type HeapItemAlignmentType = u64;
const HEAP_ITEM_ALIGNMENT: usize = std::mem::size_of::<HeapItemAlignmentType>();

impl Heap {
    pub fn new(initial_size: usize) -> Heap {
        let heap = Self::new_without_heap_info(initial_size);
        HeapInfo::from_raw_heap_ptr(heap.heap_start).init();
        heap
    }

    /// Create a new heap whose HeapInfo is left uninitialized. Caller must initialize it.
    fn new_without_heap_info(initial_size: usize) -> Heap {
        // Create uninitialized buffer of memory for heap
        unsafe {
            let layout = Layout::from_size_align(initial_size, HEAP_ALIGNMENT).unwrap();
            let heap_start = std::alloc::alloc(layout);
            let heap_end = heap_start.add(initial_size);

            let mut heap = Heap {
                heap_start,
                heap_end,
                // Permanent region is empty to start
                permanent_start: NonNull::dangling().as_ptr(),
                permanent_end: NonNull::dangling().as_ptr(),
                layout,
                stats: GcStats::default(),
                census: Vec::new(),
                // Semispaces initialized below
                start: std::ptr::null(),
                current: std::ptr::null(),
                end: std::ptr::null(),
                next_heap_start: std::ptr::null(),
                next_heap_end: std::ptr::null(),
                alloc_limit: std::ptr::null(),

                #[cfg(feature = "gc_stress_test")]
                gc_stress_test: false,
            };

            // Leave room for heap info struct at start of heap
            heap.init_semispaces(heap_start.add(size_of::<HeapInfo>()), heap_end);

            heap
        }
    }

    /// Split the memory between `semispaces_start` and `semispaces_end` into two equal sized
    /// semispaces with the proper alignment, setting the first to be the current semispace.
    fn init_semispaces(&mut self, semispaces_start: *const u8, semispaces_end: *const u8) {
        let semispaces_start = align_pointer_up(semispaces_start, HEAP_ITEM_ALIGNMENT);
        let available_size = semispaces_end as usize - semispaces_start as usize;

        // Round the semispace size down so that the second semispace is aligned
        let semispace_size = align_down(available_size / 2, HEAP_ITEM_ALIGNMENT);

        self.start = semispaces_start;
        self.current = semispaces_start;
        self.end = unsafe { semispaces_start.add(semispace_size) };
        self.alloc_limit = self.end;

        self.next_heap_start = self.end;
        self.next_heap_end = unsafe { self.end.add(semispace_size) };

        self.debug_assert_heap_well_formed();
    }

    /// Initialize an uninitialized heap with a serialized heap. This will copy the serialized heap
    /// over and fix all pointers within the new heap.
    ///
    /// Pointers were encoded as offsets and can be restored by adding to the new heap base pointer.
    pub fn init_from_serialized(&mut self, cx: Context, serialized: &SerializedHeap) {
        // Size of the `HeapInfo` struct can be different from the serialized heap, e.g. due to
        // feature flags. We must apply an additional offset to account for this.
        let extra_offset = calculate_extra_offset(serialized);
        let new_permanent_start_offset = extra_offset + serialized.permanent_space.start_offset;
        let new_current_start_offset = extra_offset + serialized.current_space.start_offset;

        let permanent_size = serialized.permanent_space.bytes.len();

        // Find bounds of permanent space
        self.permanent_start = unsafe { self.heap_start.add(new_permanent_start_offset) };
        self.permanent_end = unsafe { self.permanent_start.add(permanent_size) };

        // Copy permanent semispace into the actual heap
        self.permanent_heap_mut()
            .copy_from_slice(serialized.permanent_space.bytes);

        // Rewrite offsets in the permanent space to pointers in the new heap
        HeapSpaceDeserializer::deserialize(cx, self.permanent_heap_mut(), extra_offset);

        // Set up bounds of the semispaces in the available space
        let semispaces_start = unsafe { self.heap_start.add(new_current_start_offset) };
        self.init_semispaces(semispaces_start, self.heap_end);

        // Ensure actual heap has enough room for the serialized heap
        let semispace_size = self.end as usize - self.start as usize;
        if serialized.current_space.bytes.len() > semispace_size {
            panic!("Serialized heap is larger than the actual heap");
        }

        // Set the current pointer to the end of the used portion of the current semispace
        self.current = unsafe { self.start.add(serialized.current_space.bytes.len()) };

        // Copy used portion of current semispace into the actual heap
        self.current_used_heap_mut()
            .copy_from_slice(serialized.current_space.bytes);

        // Rewrite offsets in the current semispace to pointers in the new heap
        HeapSpaceDeserializer::deserialize(cx, self.current_used_heap_mut(), extra_offset);

        self.debug_assert_heap_well_formed();
    }

    /// Create a new heap for a resizing GC, provided the old heap and the new heap size.
    pub fn new_for_resize(prev_heap: &Heap, new_size: usize) -> Heap {
        debug_assert!(new_size.is_power_of_two());

        let mut new_heap = Self::new_without_heap_info(new_size);
        new_heap.stats = prev_heap.stats;
        new_heap.census = prev_heap.census.clone();

        // Copy the old HeapInfo into the new heap
        unsafe {
            std::ptr::copy_nonoverlapping(
                prev_heap.heap_start,
                new_heap.heap_start.cast_mut(),
                size_of::<HeapInfo>(),
            )
        };

        // Set up bounds of the new permanent semispace
        new_heap.permanent_start = unsafe {
            let offset = prev_heap.permanent_start.offset_from(prev_heap.heap_start);
            new_heap.heap_start.offset(offset)
        };
        new_heap.permanent_end = unsafe {
            new_heap
                .permanent_start
                .add(prev_heap.permanent_bytes_allocated())
        };

        // Set up bounds of the new semispaces in the remaining heap after the permanent region
        new_heap.init_semispaces(new_heap.permanent_end, new_heap.heap_end);

        // Set additional GC flags
        #[cfg(feature = "gc_stress_test")]
        {
            new_heap.gc_stress_test = prev_heap.gc_stress_test;
        }

        new_heap.debug_assert_heap_well_formed();

        new_heap
    }

    fn debug_assert_heap_well_formed(&self) {
        // The semispaces must not extend beyond the end of the heap
        debug_assert!(self.end <= self.heap_end);
        debug_assert!(self.next_heap_end <= self.heap_end);

        // The semispaces must be aligned to the heap item alignment
        debug_assert!(is_heap_item_aligned(self.start));
        debug_assert!(is_heap_item_aligned(self.next_heap_start));

        // The semispaces must have the exact same size
        debug_assert_eq!(
            self.end as usize - self.start as usize,
            self.next_heap_end as usize - self.next_heap_start as usize
        );

        // The current pointer must be within the bounds of the current semispace
        debug_assert!(self.current >= self.start && self.current < self.end);
    }

    #[inline]
    pub fn info<'a>(&self) -> &'a mut HeapInfo {
        unsafe { &mut *(self.heap_start as *const _ as *mut HeapInfo) }
    }

    pub fn alloc_uninit<T>(cx: Context) -> AllocResult<HeapPtr<T>> {
        Self::alloc_uninit_with_size::<T>(cx, size_of::<T>())
    }

    /// Allocate an object of a given type with the specified size in bytes. When called directly,
    /// is used to allocate dynamically sized objects.
    ///
    /// Allocation will have at least the given size and is guaranteed to have 8-byte alignment.
    #[inline]
    pub fn alloc_uninit_with_size<T>(mut cx: Context, size: usize) -> AllocResult<HeapPtr<T>> {
        // Statically ensure that type is compatible with the heap's alignment.
        const_assert!(align_of::<T>() <= HEAP_ITEM_ALIGNMENT);

        let alloc_size = Self::alloc_size_for_request_size(size);

        // Run a GC on every allocation in stress test mode
        #[cfg(feature = "gc_stress_test")]
        if cx.heap.gc_stress_test {
            Self::run_gc(cx, GcType::Normal);
        }

        // Ensure that this direct reference to heap is not used after a GC or allocation
        let heap = &mut cx.heap;
        let start = heap.current;

        // Calculate where the current will be after this allocation, checking if there is room
        let next_current = start.wrapping_add(alloc_size);
        if next_current > heap.alloc_limit {
            // If there is not room run a gc cycle. The window is closed for the duration of a
            // collection (`GarbageCollector::run`), so an allocation from a GC hook lands here and
            // is rejected by `abort_alloc_during_gc` rather than bumping into the from-space. An
            // allocation at teardown lands here too and is served, see `collect_for_alloc`.

            Self::collect_for_alloc(cx, alloc_size);
            cx.heap.cover_alloc(alloc_size);

            // Make sure there is enough space for allocation after gc, otherwise we are out of
            // heap memory.
            if !cx.heap.has_room_for_alloc(alloc_size) {
                #[cfg(feature = "alloc_error")]
                {
                    return Err(AllocError::oom());
                }

                #[cfg(not(feature = "alloc_error"))]
                {
                    panic!("Ran out of heap memory");
                }
            }

            return Self::alloc_uninit_with_size(cx, alloc_size);
        }

        // Update end pointer and write into memory
        heap.current = next_current;
        let start = start.cast_mut().cast();

        Ok(HeapPtr::from_ptr(start))
    }

    pub fn run_gc(cx: Context, type_: GcType) {
        GarbageCollector::run(cx, type_)
    }

    /// Collect because an allocation of `alloc_size` bytes does not fit, sizing the heap to the
    /// live data: the heap grows only when the live data would leave less than half of the
    /// semispace free, and shrinks (one halving per collection, not below the minimum heap size)
    /// when the live data uses less than an eighth of it. Without this the heap doubled on every
    /// full semispace until it reached the maximum, whatever was live.
    fn collect_for_alloc(cx: Context, alloc_size: usize) {
        if cx.gc_in_progress {
            if cx.gc_collecting {
                // An allocation from a GC hook during a collection: no collection can run, and the
                // item would land in the from-space (or, of a resizing collection, in the heap buffer
                // this collection frees), dead before the collection returns. Row 6d of
                // docs/engine-invariants.md forbids it.
                Self::abort_alloc_during_gc(cx, alloc_size);
            }
            // Teardown (`embed::teardown`) is not a collection: it sets `gc_in_progress` so that no
            // GC nests under the finalizers, and leaves the allocation window as it found it. An
            // item that still fits the semispace is served by `cover_alloc` in the caller and dies
            // with the context, as it did before this containment; one that does not fit the
            // semispace fails as out of memory below, as it always did.
            return;
        }
        let heap_size = cx.heap.heap_size();
        let max_heap = Self::effective_max_heap(cx);
        let min_heap = cx.options.min_heap_size;
        let semispace = cx.heap.semispace_size();
        debug_assert!(
            heap_size.is_power_of_two() && max_heap.is_power_of_two() && heap_size <= max_heap
        );

        // The live size after the last collection predicts this one: grow in the same collection
        // rather than copying everything twice.
        if sizing::grow_before_collect(
            heap_size,
            max_heap,
            cx.heap.stats.gc_count > 0,
            cx.heap.stats.last_live_bytes,
            alloc_size,
            semispace,
        ) {
            let last_live = cx.heap.stats.last_live_bytes;
            Self::run_gc(cx, GcType::Grow { alloc_size: Some(alloc_size), live: last_live });
            return;
        }

        Self::run_gc(cx, GcType::Normal);

        let live = cx.heap.bytes_allocated();
        match sizing::resize_after_collect(
            heap_size,
            max_heap,
            min_heap,
            live,
            alloc_size,
            semispace,
            Self::sizing_overhead(&cx.heap),
        ) {
            sizing::Resize::Grow => {
                Self::run_gc(cx, GcType::Grow { alloc_size: Some(alloc_size), live })
            }
            sizing::Resize::Shrink => Self::run_gc(cx, GcType::Shrink),
            sizing::Resize::Normal => {}
        }
    }

    /// Reject an allocation on the moving heap made while a collection is in progress, naming the
    /// GC hook that made it (`ContextCell::gc_alloc_hook`).
    ///
    /// The item would be handed to the hook as a live `HeapPtr` and die with the collection: a
    /// `Normal` collection releases the from-space pages it bumped into, and a resizing one
    /// deallocates the whole buffer it copied out of. Neither is detectable at the read, so this is
    /// fatal. It aborts rather than panics because the call comes from inside the collector, and a
    /// panic would unwind through it while the heap is mid-copy.
    ///
    /// Before this the rule was prose: with `Options::gc_verify` the collector counted such
    /// allocations and `verify_heap` aborted on the count at the end of the collection, which named
    /// neither the hook nor the rule, and without the verifier nothing reported them at all.
    #[cold]
    fn abort_alloc_during_gc(cx: Context, alloc_size: usize) -> ! {
        if cx.options.gc_verify {
            // Report the allocation with its backtrace before the abort, as the verifier does.
            super::verify::alloc_during_gc(alloc_size);
        }
        let hook = match cx.gc_alloc_hook {
            "" => "no host hook, which is an engine bug".to_owned(),
            hook => format!("the host hook {hook}"),
        };
        eprintln!(
            "brimstone: fatal: allocation of {alloc_size} bytes on the JS heap during a garbage \
             collection, from {hook} (GC hooks must not allocate; docs/embedding.md, \
             docs/engine-invariants.md row 6d)"
        );
        std::process::abort();
    }

    /// The smallest allocation window: the semispace of the minimum heap.
    pub(crate) fn min_window(cx: Context) -> usize {
        match cx.options.min_alloc_window {
            Some(window) => window,
            None => sizing::min_window(cx.options.min_heap_size, Self::sizing_overhead(&cx.heap)),
        }
    }

    /// After a collection: let the program allocate up to `sizing::alloc_limit` bytes into the
    /// semispace before the next one. In stress mode (`Options::gc_stress_window`) at most
    /// `stress_window` bytes more.
    pub(crate) fn reset_alloc_limit(&mut self, min_window: usize, stress_window: Option<usize>) {
        let limit = sizing::alloc_limit(
            self.bytes_allocated(),
            0,
            self.semispace_size(),
            min_window,
            self.stats.high_frequency,
        );
        let limit = match stress_window {
            Some(window) => limit.min(self.bytes_allocated().saturating_add(window)),
            None => limit,
        };
        // SAFETY: the limit is at most the semispace size, so the pointer stays in the semispace.
        self.alloc_limit = unsafe { self.start.add(limit) };
    }

    /// The associated memory, host-space thresholds and tunables.
    pub fn accounting(&self) -> &Accounting {
        &self.stats.accounting
    }

    pub fn accounting_mut(&mut self) -> &mut Accounting {
        &mut self.stats.accounting
    }

    /// Make a collection due at the next allocation, as SpiderMonkey's requestMajorGC makes one due at the next allocation slow
    /// path (gc/GC.cpp:2099-2109, Allocator.cpp:131-146). Not during a collection.
    pub(crate) fn request_collection(&mut self) {
        self.alloc_limit = self.current;
    }

    /// After a collection (finalizers included): the thresholds of the associated-memory and host-space counters, from what is
    /// retained, as SpiderMonkey's updateGCStartThresholds (Zone.cpp:62-71, Scheduling.cpp:555-596).
    pub(crate) fn reset_accounting(&mut self, host_bytes: usize) {
        let high_frequency = self.stats.high_frequency;
        let a = &mut self.stats.accounting;
        let t = a.tunables;
        let factor = |retained: usize| {
            accounting::growth_tenths(
                retained,
                high_frequency,
                t.low_tenths,
                t.high_small_tenths,
                t.high_large_tenths,
                t.small_heap,
                t.large_heap,
            )
        };
        a.associated_threshold =
            accounting::threshold(a.associated_bytes, t.malloc_base, factor(a.associated_bytes));
        a.host_threshold =
            accounting::threshold(host_bytes, t.allocation_threshold, factor(host_bytes));
    }

    /// The largest heap under JSGC_MAX_BYTES (`accounting::max_heap_for`), never below the current heap.
    pub(crate) fn effective_max_heap(cx: Context) -> usize {
        let max = accounting::max_heap_for(
            cx.heap.stats.accounting.tunables.max_bytes,
            cx.options.min_heap_size,
            cx.options.max_heap_size,
        );
        max.max(cx.heap.heap_size())
    }

    /// Send every allocation to the slow path until the next `reset_alloc_limit`. `GarbageCollector::run`
    /// does this for the duration of a collection, which is how an allocation from a GC hook reaches
    /// `abort_alloc_during_gc`; `request_collection` shuts the window for the same effect on the
    /// mutator, and the next collection or `reset_alloc_limit` opens it again.
    pub(crate) fn close_alloc_window(&mut self) {
        self.alloc_limit = self.current;
    }

    /// Make sure that an allocation of `alloc_size` bytes that fits in the semispace is below the
    /// allocation limit, so that it does not collect again.
    fn cover_alloc(&mut self, alloc_size: usize) {
        let needed = self.current.wrapping_add(alloc_size);
        if needed > self.alloc_limit && needed <= self.end {
            self.alloc_limit = needed;
        }
    }

    /// Bytes of the heap that are not semispace: the permanent space moves along unchanged when the
    /// heap is resized, plus the header and one alignment unit.
    pub(super) fn sizing_overhead(heap: &Heap) -> usize {
        heap.permanent_bytes_allocated()
            .saturating_add(size_of::<HeapInfo>())
            .saturating_add(HEAP_ITEM_ALIGNMENT)
    }

    /// Size of one semispace in bytes.
    pub fn semispace_size(&self) -> usize {
        self.end as usize - self.start as usize
    }

    /// Statistics of the collections this heap has seen.
    pub fn gc_stats(&self) -> GcStats {
        self.stats
    }

    /// Record a finished collection: called with the heap in its post-collection state.
    pub(crate) fn record_gc(&mut self, type_: &GcType, resized: bool) {
        let live = self.bytes_allocated();
        let now = std::time::Instant::now();
        self.stats.high_frequency = self.stats.last_gc_at.is_none_or(|last| {
            now.duration_since(last)
                < std::time::Duration::from_millis(self.stats.accounting.tunables.high_frequency_ms)
        });
        self.stats.last_gc_at = Some(now);
        self.stats.gc_count += 1;
        self.stats.last_live_bytes = live;
        self.stats.peak_live_bytes = self.stats.peak_live_bytes.max(live);
        if resized {
            match type_ {
                GcType::Grow { .. } => self.stats.grow_count += 1,
                GcType::Shrink => self.stats.shrink_count += 1,
                GcType::Normal => {}
            }
        }
    }

    /// Count the items in the semispace by kind. Right after a collection these are exactly the
    /// live items.
    pub(crate) fn take_census(&mut self) {
        let mut census = vec![(None, 0usize, 0usize); HeapItemKind::COUNT];
        for_each_heap_item(self.start..self.current, |item| {
            let kind = item.shape().kind();
            let byte_size = byte_size_for_kind(item, kind);
            let entry = &mut census[kind as usize];
            entry.0 = Some(kind);
            entry.1 += 1;
            entry.2 += Self::alloc_size_for_request_size(byte_size);
            byte_size
        });
        self.census = census;
    }

    /// Live heap items by kind (name, count, bytes) after the last collection with the census on.
    pub fn census(&self) -> Vec<(String, usize, usize)> {
        self.census
            .iter()
            .filter_map(|&(kind, count, bytes)| {
                kind.map(|kind| (format!("{kind:?}"), count, bytes))
            })
            .collect()
    }

    /// Hand all pages of the free semispace back to the operating system. It holds only garbage
    /// after a collection; released pages read as zero when the next collection copies into them.
    /// Keeping the first 1.25 times the live size resident (to save the next copy those page
    /// faults) cost the YouTube watch page about 156 MB of RSS throughout playback, for about 35 ms
    /// of faults per collection.
    pub(crate) fn release_next_semispace(&mut self) {
        let page = page_size();
        let start = align_up(self.next_heap_start as usize, page);
        let end = align_down(self.next_heap_end as usize, page);
        if start >= end {
            return;
        }
        if release_pages(start, end - start) {
            self.stats.released_bytes += (end - start) as u64;
        }
    }

    fn has_room_for_alloc(&self, alloc_size: usize) -> bool {
        self.current.wrapping_add(alloc_size) <= self.end
    }

    pub fn heap_size(&self) -> usize {
        self.heap_end as usize - self.heap_start as usize
    }

    pub fn heap_start(&self) -> *const u8 {
        self.heap_start
    }

    pub fn current_heap_bounds(&self) -> Range<*const u8> {
        self.start..self.end
    }

    pub fn next_heap_bounds(&self) -> Range<*const u8> {
        self.next_heap_start..self.next_heap_end
    }

    pub fn permanent_heap_bounds(&self) -> Range<*const u8> {
        self.permanent_start..self.permanent_end
    }

    pub fn permanent_heap(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(self.permanent_start, self.permanent_bytes_allocated())
        }
    }

    pub fn permanent_heap_mut(&mut self) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(
                self.permanent_start.cast_mut(),
                self.permanent_bytes_allocated(),
            )
        }
    }

    pub fn current_used_heap_bounds(&self) -> (*const u8, *const u8) {
        (self.start, self.current)
    }

    pub fn current_used_heap(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.start, self.bytes_allocated()) }
    }

    pub fn current_used_heap_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.start.cast_mut(), self.bytes_allocated()) }
    }

    pub fn bytes_allocated(&self) -> usize {
        self.current as usize - self.start as usize
    }

    pub fn permanent_bytes_allocated(&self) -> usize {
        self.permanent_end as usize - self.permanent_start as usize
    }

    /// Returns true if the current semispace is before the next semispace in memory.
    pub fn is_current_before_next(&self) -> bool {
        self.start < self.next_heap_start
    }

    pub fn swap_heaps(&mut self, free_space_start_ptr: *const u8) {
        let old_start = self.start;
        let old_end = self.end;

        self.start = self.next_heap_start;
        self.end = self.next_heap_end;

        self.next_heap_start = old_start;
        self.next_heap_end = old_end;

        self.current = free_space_start_ptr;
        self.alloc_limit = self.end;
    }

    pub fn finish_resized_heap(&mut self, free_space_start_ptr: *const u8) {
        self.current = free_space_start_ptr;
        self.alloc_limit = self.end;
        self.debug_assert_heap_well_formed();
    }

    pub fn alloc_size_for_request_size(request_byte_size: usize) -> usize {
        align_up(request_byte_size, HEAP_ITEM_ALIGNMENT)
    }

    pub fn visit_roots(&self, visitor: &mut impl HeapVisitor) {
        self.info().handle_context().visit_roots(visitor)
    }

    /// Mark the current semispace as permanent, claiming it for the permanent region. Redistribute
    /// the remaining heap space between the two semispaces.
    pub fn mark_current_semispace_as_permanent(&mut self) {
        assert!(self.is_current_before_next());

        self.permanent_start = self.start;
        self.permanent_end = self.current;

        // Set up bounds of the new semispaces in the remaining heap after the permanent region
        self.init_semispaces(self.permanent_end, self.heap_end);
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        unsafe { std::alloc::dealloc(self.heap_start as *mut u8, self.layout) };
    }
}

// Align a number up, rounding down to zero
fn align_up(ptr_bits: usize, alignment: usize) -> usize {
    (ptr_bits + (alignment - 1)) & !(alignment - 1)
}

// Align a number down, rounding down to zero
fn align_down(value: usize, alignment: usize) -> usize {
    value & !(alignment - 1)
}

// Align a heap pointer, rounding up to infinity
fn align_pointer_up(ptr: *const u8, alignment: usize) -> *const u8 {
    align_up(ptr as usize, alignment) as *const u8
}

fn is_heap_item_aligned(ptr: *const u8) -> bool {
    ptr.cast::<HeapItemAlignmentType>().is_aligned()
}

/// Heap data stored at the beginning of the heap
///
/// A copy of the HeapInfo is also stored at the start of each host space region (see
/// `runtime::embed`), so the handle context lives outside of the heap and is shared by pointer.
pub struct HeapInfo {
    /// Reference to the context that holds this heap.
    context: Context,
    /// The handle context for this heap. Owned by the primary heap's HeapInfo and freed when the
    /// context is dropped. Survives heap resizes since only the pointer is copied.
    handle_context: *mut HandleContext,
}

impl HeapInfo {
    pub fn init(&mut self) {
        self.handle_context = Box::into_raw(Box::new(HandleContext::new()));
    }

    /// Initialize the HeapInfo of a secondary region (e.g. a host space region) that shares the
    /// context and handle context of the primary heap.
    pub fn init_secondary(&mut self, primary: &HeapInfo) {
        self.context = primary.context;
        self.handle_context = primary.handle_context;
    }

    /// Free the handle context. Must only be called once, when the context is dropped.
    pub fn drop_handle_context(&mut self) {
        // SAFETY: The handle context was allocated by `init` with `Box::into_raw` and is only freed
        // here, when the context that owns the heap is dropped.
        unsafe { drop(Box::from_raw(self.handle_context)) };
        self.handle_context = std::ptr::null_mut();
    }

    pub fn set_context(&mut self, cx: Context) {
        self.context = cx;
    }

    #[inline]
    pub fn from_heap_ptr<'a, T>(heap_ptr: HeapPtr<T>) -> &'a mut HeapInfo {
        HeapInfo::from_raw_heap_ptr(heap_ptr.as_ptr())
    }

    #[inline]
    pub fn from_raw_heap_ptr<'a, T>(heap_ptr: *const T) -> &'a mut HeapInfo {
        const HEAP_BASE_MASK: usize = !(HEAP_ALIGNMENT - 1);
        let heap_base = ((heap_ptr as usize) & HEAP_BASE_MASK) as *mut HeapInfo;

        unsafe { &mut *heap_base }
    }

    pub fn cx(&self) -> Context {
        self.context
    }

    #[inline]
    pub fn handle_context(&mut self) -> &mut HandleContext {
        // SAFETY: The handle context is allocated in `init` and stays valid until the context is
        // dropped.
        unsafe { &mut *self.handle_context }
    }
}
