use std::{alloc::Layout, collections::HashMap, ffi::c_void, mem::size_of, ptr::NonNull};

use bitflags::bitflags;

use crate::runtime::{
    Realm, Value,
    gc::{AnyHeapItem, HEAP_ALIGNMENT, HeapInfo, HeapPtr, HeapVisitor},
};

/// Default size of a host space region. Regions are reserved with the heap alignment so that
/// masking any pointer into a region finds the region's HeapInfo, but only the pages that are
/// actually used are touched.
const DEFAULT_REGION_SIZE: usize = 16 * 1024 * 1024;

const ITEM_ALIGNMENT: usize = 8;

bitflags! {
    /// Flags describing the hooks and behavior of a host object.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct HostFlags: u32 {
        /// The class has a trace hook, `Host::trace_host_object` is called during every GC.
        const HAS_TRACE = 1 << 0;
        /// The class has a finalizer, `Host::finalize_host_object` is called when the object dies.
        const HAS_FINALIZE = 1 << 1;
        /// The class has a resolve hook, `Host::resolve` is called on an own property miss.
        const HAS_RESOLVE = 1 << 2;
        /// The object is the global object of a realm.
        const IS_GLOBAL = 1 << 3;
        /// The object is callable, calls go to `Host::call_host_object`.
        const CALLABLE = 1 << 4;
        /// The object is a constructor, `new` goes to `Host::call_host_object`.
        const CONSTRUCTOR = 1 << 5;
        /// The object has the [[IsHTMLDDA]] internal slot of ECMA-262 B.3.6: `typeof` says
        /// "undefined", `ToBoolean` says false and `==` compares equal to null and undefined. This
        /// is what a host needs for `document.all` (SpiderMonkey's `JSCLASS_EMULATES_UNDEFINED`).
        /// The slot lives on a private copy of the object's shape, made when the object is created.
        const IS_HTMLDDA = 1 << 6;
        /// Internal: the object wraps an engine job (see `embed::jobs`). Never set by hosts.
        const INTERNAL_JOB = 1 << 31;
        /// Internal: an engine object (e.g. a promise) placed in the host space only so that it
        /// never moves. Not a host object to the embedding API. Never set by hosts.
        const PINNED = 1 << 30;
        /// Internal: an engine cell (see `embed::alloc_engine_cell`), an engine-owned object with
        /// its own trace and finalize hooks; the class pointer is its `EngineCellHooks`. Always
        /// set together with `PINNED`, so the cell is not a host object to the embedding API, and
        /// with `HAS_TRACE` when the hooks have a trace hook. Never set by hosts.
        const INTERNAL_CELL = 1 << 29;
    }
}

/// Header stored directly before every object in the host space.
///
/// Allocation layout: | reserved slots | HostHeader | object |
///
/// The object pointer is what the rest of the engine sees. The header and reserved slots are found
/// at fixed negative offsets from it.
#[repr(C)]
pub struct HostHeader {
    /// Total size of the allocation in bytes, including reserved slots and this header.
    alloc_size: u32,
    num_reserved_slots: u32,
    flags: HostFlags,
    /// GC mark bit, only set during a GC.
    marked: bool,
    /// The host's class pointer (e.g. a `JSClass*`), opaque to the engine.
    class: *const c_void,
    /// The host's proxy handler pointer, null if this is not a proxy.
    handler: *const c_void,
    /// The proxy private value. Undefined for non-proxies.
    proxy_private: Value,
    /// The realm the object was created in.
    realm: HeapPtr<Realm>,
}

const HEADER_SIZE: usize = size_of::<HostHeader>();
crate::static_assert!(HEADER_SIZE.is_multiple_of(ITEM_ALIGNMENT));

impl HostHeader {
    /// # Safety
    /// `object` must be the object pointer of a live host space allocation (its header is stored
    /// directly before the object), and no other reference to that header may be live for `'a`.
    #[inline]
    pub unsafe fn from_object_ptr<'a, T>(object: *const T) -> &'a mut HostHeader {
        // SAFETY: The caller guarantees `object` is the object of a live host space allocation,
        // which is always directly preceded by its header in the same allocation.
        unsafe { &mut *(object.cast::<u8>().sub(HEADER_SIZE) as *mut HostHeader) }
    }

    #[inline]
    pub fn object(&self) -> HeapPtr<AnyHeapItem> {
        // SAFETY: The object directly follows the header in the same allocation.
        let ptr = unsafe { (self as *const HostHeader as *const u8).add(HEADER_SIZE) };
        HeapPtr::from_ptr(ptr.cast_mut().cast())
    }

    #[inline]
    pub fn flags(&self) -> HostFlags {
        self.flags
    }

    #[inline]
    pub fn class(&self) -> *const c_void {
        self.class
    }

    #[inline]
    pub fn handler(&self) -> *const c_void {
        self.handler
    }

    #[inline]
    pub fn is_proxy(&self) -> bool {
        !self.handler.is_null()
    }

    #[inline]
    pub fn proxy_private(&self) -> Value {
        self.proxy_private
    }

    #[inline]
    pub fn set_proxy_private(&mut self, value: Value) {
        self.proxy_private = value;
    }

    #[inline]
    pub fn realm(&self) -> HeapPtr<Realm> {
        self.realm
    }

    #[inline]
    pub fn num_reserved_slots(&self) -> u32 {
        self.num_reserved_slots
    }

    #[inline]
    pub fn is_marked(&self) -> bool {
        self.marked
    }

    #[inline]
    pub fn set_marked(&mut self, marked: bool) {
        self.marked = marked;
    }

    #[inline]
    pub fn reserved_slots_mut(&mut self) -> &mut [Value] {
        let len = self.num_reserved_slots as usize;
        // SAFETY: The reserved slots directly precede the header in the same allocation and were
        // initialized when the object was allocated.
        unsafe {
            let start = (self as *mut HostHeader as *mut Value).sub(len);
            std::slice::from_raw_parts_mut(start, len)
        }
    }

    #[inline]
    pub fn reserved_slots(&self) -> &[Value] {
        let len = self.num_reserved_slots as usize;
        // SAFETY: See `reserved_slots_mut`.
        unsafe {
            let start = (self as *const HostHeader as *const Value).sub(len);
            std::slice::from_raw_parts(start, len)
        }
    }

    /// Visit the pointers held by the header itself (reserved slots, proxy private and realm).
    pub fn visit_pointers(&mut self, visitor: &mut impl HeapVisitor) {
        for slot in self.reserved_slots_mut() {
            visitor.visit_value(slot);
        }

        visitor.visit_value(&mut self.proxy_private);
        visitor.visit_pointer(&mut self.realm);
    }

    fn alloc_start(&self) -> *mut u8 {
        // SAFETY: The allocation starts with the reserved slots directly before the header.
        unsafe {
            (self as *const HostHeader as *mut u8)
                .sub(self.num_reserved_slots as usize * size_of::<Value>())
        }
    }
}

/// Swap the contents of two host space allocations of the same layout (allocation size and reserved slot count): reserved
/// slots, header fields and the object itself. Each allocation keeps its own GC mark bit. Returns false, changing nothing,
/// if the layouts differ.
///
/// # Safety
/// `a` and `b` must be the object pointers of live host space allocations, no GC may be running, and no reference into
/// either allocation may be live.
pub(crate) unsafe fn swap_allocations(a: *const u8, b: *const u8) -> bool {
    // SAFETY: the caller guarantees both are live host allocations, each preceded by its header.
    unsafe {
        let ha = a.sub(HEADER_SIZE) as *mut HostHeader;
        let hb = b.sub(HEADER_SIZE) as *mut HostHeader;
        let (size_a, slots_a, marked_a) =
            ((*ha).alloc_size, (*ha).num_reserved_slots, (*ha).marked);
        let (size_b, slots_b, marked_b) =
            ((*hb).alloc_size, (*hb).num_reserved_slots, (*hb).marked);
        if size_a != size_b || slots_a != slots_b {
            return false;
        }
        let start_a = (ha as *mut u8).sub(slots_a as usize * size_of::<Value>());
        let start_b = (hb as *mut u8).sub(slots_b as usize * size_of::<Value>());
        std::ptr::swap_nonoverlapping(start_a, start_b, size_a as usize);
        (*ha).marked = marked_a;
        (*hb).marked = marked_b;
    }
    true
}

/// A pointer to the header of a host space allocation, for code outside the GC.
///
/// Any number of copies may exist at once, also across calls into the host, which may access the
/// same header again. Every accessor reads or writes a single field (or reserved slot) through the
/// raw pointer and never creates a reference to the header or its slots, so no two references to
/// them can overlap.
///
/// Invariant: the pointer is the header of a host space allocation that has not been freed. Like a
/// `HeapPtr`, a `HostHeaderPtr` must not be used after its object died (was finalized and swept).
#[derive(Clone, Copy)]
pub struct HostHeaderPtr(NonNull<HostHeader>);

impl HostHeaderPtr {
    /// # Safety
    /// `object` must be the object pointer of a host space allocation (its header is stored directly
    /// before the object, in the same allocation) that is not freed while the result is used.
    #[inline]
    pub unsafe fn from_object_ptr<T>(object: *const T) -> HostHeaderPtr {
        // SAFETY: The caller guarantees `object` is the object of a host space allocation, which is
        // directly preceded by its header in the same allocation, so the result is in bounds and
        // not null.
        unsafe {
            let header = object.cast::<u8>().sub(HEADER_SIZE) as *mut HostHeader;
            HostHeaderPtr(NonNull::new_unchecked(header))
        }
    }

    #[inline]
    fn ptr(self) -> *mut HostHeader {
        self.0.as_ptr()
    }

    #[inline]
    pub fn flags(self) -> HostFlags {
        // SAFETY: The header is allocated and initialized (type invariant). Reading a field
        // through the raw pointer creates no reference.
        unsafe { (*self.ptr()).flags }
    }

    #[inline]
    pub fn class(self) -> *const c_void {
        // SAFETY: See `flags`.
        unsafe { (*self.ptr()).class }
    }

    #[inline]
    pub fn handler(self) -> *const c_void {
        // SAFETY: See `flags`.
        unsafe { (*self.ptr()).handler }
    }

    #[inline]
    pub fn is_proxy(self) -> bool {
        !self.handler().is_null()
    }

    #[inline]
    pub fn proxy_private(self) -> Value {
        // SAFETY: See `flags`.
        unsafe { (*self.ptr()).proxy_private }
    }

    #[inline]
    pub fn set_proxy_private(self, value: Value) {
        // SAFETY: The header is allocated (type invariant). Writing a field through the raw
        // pointer creates no reference, so it does not conflict with other copies of this pointer.
        unsafe { (*self.ptr()).proxy_private = value }
    }

    #[inline]
    pub fn realm(self) -> HeapPtr<Realm> {
        // SAFETY: See `flags`.
        unsafe { (*self.ptr()).realm }
    }

    #[inline]
    pub fn num_reserved_slots(self) -> u32 {
        // SAFETY: See `flags`.
        unsafe { (*self.ptr()).num_reserved_slots }
    }

    /// Pointer to a reserved slot. Panics if the index is out of range.
    #[inline]
    fn reserved_slot_ptr(self, index: u32) -> *mut Value {
        let len = self.num_reserved_slots();
        assert!(index < len, "reserved slot index {index} out of range ({len} slots)");
        // SAFETY: The `len` reserved slots directly precede the header in the same allocation, and
        // `index < len`, so the slot is in bounds.
        unsafe { self.ptr().cast::<Value>().sub((len - index) as usize) }
    }

    /// Read a reserved slot. Panics if the index is out of range.
    #[inline]
    pub fn reserved_slot(self, index: u32) -> Value {
        // SAFETY: The slot is in bounds and was initialized when the object was allocated. The
        // read creates no reference.
        unsafe { self.reserved_slot_ptr(index).read() }
    }

    /// Write a reserved slot. Panics if the index is out of range.
    #[inline]
    pub fn set_reserved_slot(self, index: u32, value: Value) {
        // SAFETY: The slot is in bounds. The write creates no reference.
        unsafe { self.reserved_slot_ptr(index).write(value) }
    }

    /// The reserved slots as a slice.
    ///
    /// # Safety
    /// No other reference to the reserved slots of this object may be used during `'a`, and nothing
    /// may read or write them in another way (`reserved_slot`, `set_reserved_slot`, a GC) during
    /// `'a`. The object must not be freed during `'a`.
    #[inline]
    pub unsafe fn reserved_slots_mut<'a>(self) -> &'a mut [Value] {
        let len = self.num_reserved_slots() as usize;
        // SAFETY: The `len` reserved slots directly precede the header in the same allocation and
        // are initialized. The caller guarantees the slice is the only access to them for `'a`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr().cast::<Value>().sub(len), len) }
    }

    /// The header as a mutable reference.
    ///
    /// # Safety
    /// No other reference to this header may be used during `'a`, nothing may access it through a
    /// `HostHeaderPtr` during `'a`, and the object must not be freed during `'a`.
    #[inline]
    pub unsafe fn as_mut<'a>(self) -> &'a mut HostHeader {
        // SAFETY: The header is allocated and initialized, and the caller guarantees uniqueness.
        unsafe { &mut *self.ptr() }
    }
}

/// Parameters for a new host space allocation.
pub struct HostAllocation {
    pub object_size: usize,
    pub num_reserved_slots: u32,
    pub flags: HostFlags,
    pub class: *const c_void,
    pub handler: *const c_void,
    pub proxy_private: Value,
    pub realm: HeapPtr<Realm>,
}

/// A 4 GiB-aligned region of the host space, starting with its own HeapInfo.
struct Region {
    base: *mut u8,
    bump: *mut u8,
    end: *mut u8,
    layout: Layout,
}

/// The bases of the regions of a host space when it was copied. The garbage collector uses its own
/// copy, so it keeps no pointer into the embedding state while host hooks run. The copy stays exact
/// for a whole GC: no host object (so no region) can be created during a GC (`alloc_host_object`),
/// and regions are only freed when the space is dropped.
pub struct HostRegionBases(Vec<usize>);

impl HostRegionBases {
    /// Whether a pointer points into one of the regions.
    #[inline]
    pub fn contains(&self, ptr: *const u8) -> bool {
        let base = (ptr as usize) & !(HEAP_ALIGNMENT - 1);
        self.0.contains(&base)
    }
}

/// The non-moving space for host objects. The copying collector never moves objects in this space.
/// Instead they are marked when first reached, have their fields visited, and are swept (finalized
/// and freed) when unmarked at the end of a GC.
pub struct HostSpace {
    regions: Vec<Region>,
    /// Every allocated (live or not yet swept) host object, by header.
    objects: Vec<NonNull<HostHeader>>,
    /// Freed allocations, by allocation size.
    free_lists: HashMap<u32, Vec<*mut u8>>,
    /// Bytes in allocations that are in use (live, or dead but not yet swept).
    used_bytes: usize,
    /// Bytes reserved for all regions.
    region_bytes: usize,
}

impl HostSpace {
    pub fn new() -> Self {
        HostSpace {
            regions: vec![],
            objects: vec![],
            free_lists: HashMap::new(),
            used_bytes: 0,
            region_bytes: 0,
        }
    }

    /// Whether a pointer points into the host space.
    #[inline]
    pub fn contains(&self, ptr: *const u8) -> bool {
        let base = (ptr as usize) & !(HEAP_ALIGNMENT - 1);
        self.regions
            .iter()
            .any(|region| region.base as usize == base)
    }

    /// A copy of the region bases, answering `contains` without a reference to this space.
    pub fn region_bases(&self) -> HostRegionBases {
        HostRegionBases(
            self.regions
                .iter()
                .map(|region| region.base as usize)
                .collect(),
        )
    }

    /// Bytes in allocations that are in use.
    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Bytes reserved for regions (address space; only touched pages use memory).
    pub fn region_bytes(&self) -> usize {
        self.region_bytes
    }

    /// The headers of every allocated (live or not yet swept) host object.
    pub fn headers(&self) -> &[NonNull<HostHeader>] {
        &self.objects
    }

    pub fn num_objects(&self) -> usize {
        self.objects.len()
    }

    /// Allocate a new host object with an initialized header and reserved slots (all undefined).
    /// The object itself is left uninitialized and must be initialized by the caller.
    pub fn alloc(
        &mut self,
        primary_heap_info: &HeapInfo,
        allocation: HostAllocation,
    ) -> NonNull<HostHeader> {
        let reserved_size = allocation.num_reserved_slots as usize * size_of::<Value>();
        let object_size = align_up(allocation.object_size, ITEM_ALIGNMENT);
        let alloc_size = reserved_size + HEADER_SIZE + object_size;
        let alloc_size_u32 = u32::try_from(alloc_size).expect("host object too large");

        let start = match self
            .free_lists
            .get_mut(&alloc_size_u32)
            .and_then(|list| list.pop())
        {
            Some(start) => start,
            None => self.bump_alloc(primary_heap_info, alloc_size),
        };

        // SAFETY: `start` points to `alloc_size` writable bytes owned by this space, and the header
        // is written at its aligned position inside them.
        let header = unsafe {
            let header = start.add(reserved_size) as *mut HostHeader;
            header.write(HostHeader {
                alloc_size: alloc_size_u32,
                num_reserved_slots: allocation.num_reserved_slots,
                flags: allocation.flags,
                marked: false,
                class: allocation.class,
                handler: allocation.handler,
                proxy_private: allocation.proxy_private,
                realm: allocation.realm,
            });

            for slot in (*header).reserved_slots_mut() {
                *slot = Value::undefined();
            }

            NonNull::new_unchecked(header)
        };

        self.objects.push(header);
        self.used_bytes += alloc_size;

        header
    }

    fn bump_alloc(&mut self, primary_heap_info: &HeapInfo, alloc_size: usize) -> *mut u8 {
        if let Some(region) = self.regions.last_mut() {
            if (region.end as usize) - (region.bump as usize) >= alloc_size {
                let start = region.bump;
                // SAFETY: There is room in the region for the allocation.
                region.bump = unsafe { region.bump.add(alloc_size) };
                return start;
            }
        }

        let header_size = align_up(size_of::<HeapInfo>(), ITEM_ALIGNMENT);
        let Some(region_size) = region_size_for(alloc_size) else {
            // The allocation can never fit inside a region's HEAP_ALIGNMENT window. Refuse it
            // with the same out-of-memory result as a failed allocation.
            let layout = Layout::from_size_align(alloc_size, ITEM_ALIGNMENT).unwrap();
            std::alloc::handle_alloc_error(layout);
        };
        let layout = Layout::from_size_align(region_size, HEAP_ALIGNMENT).unwrap();

        // SAFETY: The layout has a non-zero size. The HeapInfo at the start of the region is
        // initialized before any pointer into the region is handed out.
        let base = unsafe { std::alloc::alloc(layout) };
        if base.is_null() {
            std::alloc::handle_alloc_error(layout);
        }

        HeapInfo::from_raw_heap_ptr(base).init_secondary(primary_heap_info);

        // SAFETY: The region is large enough to hold its HeapInfo and the allocation.
        let (start, bump, end) = unsafe {
            let start = base.add(header_size);
            (start, start.add(alloc_size), base.add(region_size))
        };

        self.regions.push(Region { base, bump, end, layout });
        self.region_bytes += region_size;

        start
    }

    /// Remove every unmarked object from the space and clear the mark bit of every marked object.
    /// Return the headers of the removed objects. Their memory stays intact until `free` is called.
    pub fn take_unmarked(&mut self) -> Vec<NonNull<HostHeader>> {
        let mut dead = vec![];

        self.objects.retain(|header| {
            // SAFETY: Every header in the list points to a live allocation of this space.
            let header_ref = unsafe { &mut *header.as_ptr() };
            if header_ref.marked {
                header_ref.marked = false;
                true
            } else {
                dead.push(*header);
                false
            }
        });

        dead
    }

    /// Free allocations previously returned by `take_unmarked`.
    pub fn free(&mut self, dead: Vec<NonNull<HostHeader>>) {
        for header in dead {
            // SAFETY: The header was removed from the object list and is not referenced by any live
            // object, so its allocation can be reused.
            let header_ref = unsafe { &mut *header.as_ptr() };
            let alloc_size = header_ref.alloc_size;
            let start = header_ref.alloc_start();

            // Poison the memory to catch use after free in debug builds
            #[cfg(debug_assertions)]
            // SAFETY: The allocation is `alloc_size` bytes starting at `start`.
            unsafe {
                std::ptr::write_bytes(start, 0xdb, alloc_size as usize)
            };

            self.free_lists.entry(alloc_size).or_default().push(start);
            self.used_bytes -= alloc_size as usize;
        }
    }

    /// Remove every object from the space, returning their headers. Used at context teardown.
    pub fn take_all(&mut self) -> Vec<NonNull<HostHeader>> {
        std::mem::take(&mut self.objects)
    }
}

impl Drop for HostSpace {
    fn drop(&mut self) {
        for region in &self.regions {
            // SAFETY: Each region was allocated with this layout in `bump_alloc`.
            unsafe { std::alloc::dealloc(region.base, region.layout) };
        }
    }
}

fn align_up(value: usize, alignment: usize) -> usize {
    (value + (alignment - 1)) & !(alignment - 1)
}

/// The size of the region needed for an allocation of `alloc_size` bytes, or `None` if it can
/// never fit: every byte of a region must stay inside its HEAP_ALIGNMENT window so that masking
/// any pointer into the region finds the region's HeapInfo.
fn region_size_for(alloc_size: usize) -> Option<usize> {
    let header_size = align_up(size_of::<HeapInfo>(), ITEM_ALIGNMENT);
    let needed = header_size + alloc_size;
    if needed > HEAP_ALIGNMENT {
        return None;
    }
    Some(DEFAULT_REGION_SIZE.max(align_up(needed, 4096)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_just_over_region_cap_is_refused() {
        let header_size = align_up(size_of::<HeapInfo>(), ITEM_ALIGNMENT);
        // Just over the cap: refused (the out-of-memory path) without allocating.
        assert_eq!(region_size_for(HEAP_ALIGNMENT - header_size + 1), None);
        // Just under the cap: still fits.
        assert!(region_size_for(HEAP_ALIGNMENT - header_size).is_some());
    }
}
