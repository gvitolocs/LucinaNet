use std::ptr::NonNull;

use crate::{
    extend_object,
    runtime::{
        Context, Handle, HeapPtr,
        collections::{ArrayInstance, array::ByteArray},
        error::{range_error, type_error},
        eval_result::EvalResult,
        gc::{HeapItem, HeapVisitor},
        intrinsics::intrinsics::Intrinsic,
        object_value::ObjectValue,
        ordinary_object::ObjectBuilder,
    },
};

// 4GB max array buffer size
const MAX_ARRAY_BUFFER_SIZE: usize = 1 << 32;

/// The alignment `new_external` requires of non-empty external memory: that of the largest typed
/// array element (Float64, BigInt64). Heap data blocks are at least this aligned too.
pub const EXTERNAL_ALIGNMENT: usize = 8;

/// Who may detach an external ArrayBuffer, and what the operations that detach a buffer as a side
/// effect do with it (the spec's [[ArrayBufferDetachKey]], design §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetachKey {
    /// No key: memory owned by the embedder (or freed by a callback). `transfer`,
    /// `transferToFixedLength`, structured-clone transfer and `JS::StealArrayBufferContents` copy
    /// the bytes into a heap buffer first and then detach this one.
    None,
    /// The buffer of a `WebAssembly.Memory`: only the memory detaches it. `transfer`,
    /// `transferToFixedLength`, structured-clone transfer and `JS::StealArrayBufferContents` throw a
    /// TypeError (the JS API sets the detach key to the memory).
    WasmMemory,
}

/// The backing of an external ArrayBuffer: memory that does not live on the JS heap.
///
/// Invariant while the buffer is attached: `ptr` points to `byte_length` bytes that are readable
/// and writable, that nothing else reads or writes while engine code holds a slice of them (no
/// other attached buffer covers any of them), and that stay allocated as long as `owner` (if any)
/// is alive; `ptr` is aligned to `EXTERNAL_ALIGNMENT` (checked by `new_external`). Who keeps that true:
/// - `DetachKey::WasmMemory`: invariant B1 (design §3.2): the buffer is detached at the first
///   crossing back into engine code after the memory moved or changed size.
/// - an owner cell that frees the memory in its finalizer: the buffer holds the cell.
/// - no owner: the embedder's contract (`embed::api::array_buffer_new_external`).
#[derive(Clone, Copy)]
pub struct ExternalBacking {
    ptr: NonNull<u8>,
    /// The object that keeps the memory alive, visited by the GC.
    owner: Option<HeapPtr<ObjectValue>>,
    key: DetachKey,
}

extend_object! {
    /// ArrayBuffer Objects (https://tc39.es/ecma262/#sec-arraybuffer-objects)
    pub struct ArrayBufferObject {
        /// Byte length of the array buffer. Stored separately from data as data might be detached.
        /// The only length of an external buffer.
        byte_length: usize,
        /// Maximum byte length that the array buffer can grow to. Fixed length if no max is set.
        max_byte_length: Option<usize>,
        /// Data block containing array buffer's binary data, for buffers whose bytes live on the
        /// JS heap. None for external buffers and for detached buffers.
        data: Option<HeapPtr<ByteArray>>,
        /// The backing of an external buffer (bytes outside the JS heap, e.g. a wasm linear
        /// memory). None for heap buffers and for detached buffers. A buffer is detached when both
        /// `data` and `external` are None.
        external: Option<ExternalBacking>,
    }
}

impl ArrayBufferObject {
    /// AllocateArrayBuffer (https://tc39.es/ecma262/#sec-allocatearraybuffer)
    pub fn new(
        cx: Context,
        constructor: Handle<ObjectValue>,
        byte_length: usize,
        max_byte_length: Option<usize>,
        data: Option<Handle<ByteArray>>,
    ) -> EvalResult<Handle<ArrayBufferObject>> {
        if let Some(max_byte_length) = max_byte_length {
            if byte_length > max_byte_length {
                return range_error(cx, "byte length exceeds max byte length");
            }
        }

        let mut object = ObjectBuilder::<ArrayBufferObject>::new(cx)
            .constructor_proto(constructor, Intrinsic::ArrayBufferPrototype)?
            .build()?;

        // Temporarily fill default values so object is fully initialized before GC may be triggered
        object.byte_length = byte_length;
        object.max_byte_length = max_byte_length;
        object.data = None;
        object.external = None;

        if byte_length > MAX_ARRAY_BUFFER_SIZE {
            return range_error(cx, &format!("cannot allocate array buffer of size {byte_length}"));
        }

        if let Some(max_byte_length) = max_byte_length {
            if max_byte_length > MAX_ARRAY_BUFFER_SIZE {
                return range_error(cx, "max byte length exceeds maximum array buffer size");
            }
        }

        // Save object pointer behind handle as we are about to allocate
        let mut object = object.to_handle();

        object.data = if let Some(data) = data {
            // If requested size matches the underlying data block then simply reuse it
            if byte_length == data.len() {
                Some(*data)
            } else {
                // Otherwise allocate new zeroed data block and copy the data into it
                let mut new_uninit = ByteArray::new_uninit(cx, byte_length)?;

                // Copy data from the old to new data block
                let copied_size = byte_length.min(data.len());
                new_uninit.as_mut_slice()[..copied_size]
                    .copy_from_slice(&data.as_slice()[..copied_size]);

                // Zero out the rest of the new data block
                new_uninit.as_mut_slice()[copied_size..].fill(0);

                Some(new_uninit)
            }
        } else {
            // Initialize data block to all zeros
            Some(ByteArray::new(cx, byte_length, 0)?)
        };

        Ok(object)
    }

    /// Create a fixed-length ArrayBuffer whose bytes are the `byte_length` bytes at `data`, memory
    /// that is not on the JS heap (design §3.1). The bytes are not copied or initialized.
    ///
    /// Refused with a TypeError: a null `data` with a non-zero length, and a non-zero length whose
    /// `data` is not aligned to `EXTERNAL_ALIGNMENT` (8, the largest typed array element). The
    /// alignment is checked here, once, so every element access (the VM fast path and
    /// `read_element_value`/`write_element` do aligned reads and writes of the element type) stays
    /// an aligned move without a check of its own. A zero-length buffer never touches its pointer,
    /// so its `data` is replaced by an aligned dangling pointer (wasmi's empty `Vec<u8>` has the
    /// dangling address 1). RangeError above `MAX_ARRAY_BUFFER_SIZE`.
    ///
    /// `owner`, if any, is kept alive by the buffer until it is detached.
    ///
    /// # Safety
    /// While the buffer is attached (until `detach_external`, `detach`, or the buffer dies), the
    /// `byte_length` bytes at `data` must be allocated, readable and writable, and nothing else may
    /// read or write them while engine code runs: no Rust reference to them, and no other attached
    /// ArrayBuffer covering any of them. For key `WasmMemory` that is invariant B1 (§3.2), kept by
    /// the memory's records; for an `owner` that frees the memory when it dies, the buffer's edge
    /// to the owner keeps it allocated; otherwise the caller keeps it allocated until it detaches the
    /// buffer.
    pub(crate) unsafe fn new_external(
        cx: Context,
        constructor: Handle<ObjectValue>,
        data: *mut u8,
        byte_length: usize,
        owner: Option<Handle<ObjectValue>>,
        key: DetachKey,
    ) -> EvalResult<Handle<ArrayBufferObject>> {
        if byte_length > MAX_ARRAY_BUFFER_SIZE {
            return range_error(cx, &format!("cannot create array buffer of size {byte_length}"));
        }

        let ptr = if byte_length == 0 {
            NonNull::<u64>::dangling().cast::<u8>()
        } else {
            match NonNull::new(data) {
                None => return type_error(cx, "external array buffer data is null"),
                Some(ptr) if !(ptr.as_ptr() as usize).is_multiple_of(EXTERNAL_ALIGNMENT) => {
                    return type_error(cx, "external array buffer data is not 8-byte aligned");
                }
                Some(ptr) => ptr,
            }
        };

        // The only allocation. `owner` is a handle, so it survives a GC here.
        let mut object = ObjectBuilder::<ArrayBufferObject>::new(cx)
            .constructor_proto(constructor, Intrinsic::ArrayBufferPrototype)?
            .build()?;

        object.byte_length = byte_length;
        object.max_byte_length = None;
        object.data = None;
        object.external = Some(ExternalBacking { ptr, owner: owner.map(|owner| *owner), key });

        Ok(object.to_handle())
    }

    pub fn byte_length(&self) -> usize {
        self.byte_length
    }

    pub fn set_byte_length(&mut self, byte_length: usize) {
        debug_assert!(self.external.is_none(), "external array buffers are fixed-length");
        self.byte_length = byte_length;
    }

    pub fn max_byte_length(&self) -> Option<usize> {
        self.max_byte_length
    }

    pub fn is_fixed_length(&self) -> bool {
        self.max_byte_length.is_none()
    }

    /// The buffer's bytes. Panics if the buffer is detached.
    ///
    /// For an external buffer the slice aliases memory owned elsewhere (U1, design §6.4). Callers
    /// take the slice after their last callout (anything that can run JS, allocate, grow or detach)
    /// and drop it before the next one; the inventory of every caller is design §3.3.
    #[inline]
    pub fn data(&self) -> &[u8] {
        match self.data.as_ref() {
            Some(data) => data.as_slice(),
            None => self.external_data(),
        }
    }

    /// The buffer's bytes, mutable. Panics if the buffer is detached. See `data` for external
    /// buffers.
    #[inline]
    pub fn data_mut(&mut self) -> &mut [u8] {
        match self.data {
            Some(ref mut data) => data.as_mut_slice(),
            None => self.external_data_mut(),
        }
    }

    fn external_data(&self) -> &[u8] {
        let external = self.external.as_ref().expect("array buffer is detached");

        // SAFETY (U1, design §6.4): the buffer is attached (`external` is Some), so by the
        // invariant of `ExternalBacking` `ptr` points to `byte_length` readable bytes that stay
        // allocated while the buffer holds its owner, and nothing writes them while engine code
        // holds this slice: wasm runs only inside driver calls, which engine code enters holding no
        // slice (§3.3), and every detach or grow is a callout. `byte_length` is at most
        // MAX_ARRAY_BUFFER_SIZE, and `ptr` is non-null and aligned (to 8, so also for u8). The
        // slice's lifetime is that of `&self`.
        debug_assert!(external.ptr.cast::<u64>().is_aligned());
        unsafe { std::slice::from_raw_parts(external.ptr.as_ptr(), self.byte_length) }
    }

    fn external_data_mut(&mut self) -> &mut [u8] {
        let external = self.external.as_ref().expect("array buffer is detached");

        // SAFETY (U1, design §6.4): as in `external_data`, and `&mut self` gives exclusive access
        // to the buffer object for the slice's lifetime; no other attached buffer aliases the
        // same memory (one attached buffer per wasm memory, B1; the embedder's contract for
        // buffers it creates).
        debug_assert!(external.ptr.cast::<u64>().is_aligned());
        unsafe { std::slice::from_raw_parts_mut(external.ptr.as_ptr(), self.byte_length) }
    }

    /// A pointer to the buffer's first byte, read from the object at the moment of the call, or
    /// None if the buffer is detached. Works for heap and external buffers; the pointer is valid
    /// for `byte_length()` bytes until the next GC (heap buffers move) or callout.
    #[inline]
    pub fn raw_data_ptr(&self) -> Option<*mut u8> {
        if let Some(mut data) = self.data {
            return Some(data.as_mut_slice().as_mut_ptr());
        }

        self.external.as_ref().map(|external| external.ptr.as_ptr())
    }

    /// The heap data block, as a handle. None for detached and for external buffers.
    pub fn data_opt(&self) -> Option<Handle<ByteArray>> {
        self.data.map(|data| data.to_handle())
    }

    /// The buffer's heap data block, or None if the buffer is detached or external.
    #[inline]
    pub fn data_ptr(&self) -> Option<HeapPtr<ByteArray>> {
        self.data
    }

    pub fn set_data(&mut self, data: HeapPtr<ByteArray>) {
        debug_assert!(self.external.is_none(), "external array buffers are fixed-length");
        self.data = Some(data);
    }

    pub fn is_detached(&self) -> bool {
        self.data.is_none() && self.external.is_none()
    }

    /// Whether the buffer is attached and its bytes live outside the JS heap.
    pub fn is_external(&self) -> bool {
        self.external.is_some()
    }

    /// The detach key of an attached external buffer. `DetachKey::None` for heap buffers and for
    /// detached buffers.
    pub fn detach_key(&self) -> DetachKey {
        self.external
            .map_or(DetachKey::None, |external| external.key)
    }

    /// The owner an attached external buffer keeps alive, if any.
    pub fn external_owner(&self) -> Option<HeapPtr<ObjectValue>> {
        self.external.and_then(|external| external.owner)
    }

    /// DetachArrayBuffer (https://tc39.es/ecma262/#sec-detacharraybuffer) without a key check:
    /// detaches heap and external buffers alike. Callers that must honor the detach key use
    /// `detach_key` first.
    #[allow(dead_code)]
    pub fn detach(&mut self) {
        self.data = None;
        self.external = None;
        self.byte_length = 0;

        if self.max_byte_length.is_some() {
            self.max_byte_length = Some(0);
        }
    }

    /// Detach an external buffer (design §3.1): drop the backing and its owner, length 0. Used by
    /// the owner of the memory (`sync_buffers` after a grow, a JS `Memory.grow`, the poison
    /// transition) and by the embedder. Does nothing to a heap buffer.
    pub fn detach_external(&mut self) {
        if self.external.is_some() {
            self.external = None;
            self.byte_length = 0;
        }
    }
}

impl HeapItem for ArrayBufferObject {
    fn byte_size(array_buffer_object: HeapPtr<Self>) -> usize {
        array_buffer_object.object_byte_size()
    }

    fn visit_pointers(mut array_buffer_object: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        array_buffer_object.visit_object_pointers(visitor);
        visitor.visit_pointer_opt(&mut array_buffer_object.data);
        if let Some(external) = array_buffer_object.external.as_mut() {
            visitor.visit_pointer_opt(&mut external.owner);
        }
    }
}
