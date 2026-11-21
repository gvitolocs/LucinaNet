// The bounds our side of the glue computes from numbers Servo or a page hands it: reserved slots of a class, a native's
// argument vector, a typed array view over its buffer, a copy of a string's chars. The engine and `jsapi` functions call these
// and index only with what they return. Verus checks this very file (`scripts/verify-shipped.sh`).
//
// Proved: every index or range returned is inside the object or buffer it is for, and the arithmetic cannot overflow for any
// input. Not proved: that the counts passed in are the real sizes (they come from the engine's own objects).

/// `JSCLASS_RESERVED_SLOTS_SHIFT` and `_MASK`, from mozjs_sys's jsapi.rs.
pub const RESERVED_SLOTS_SHIFT: u32 = 8;
pub const RESERVED_SLOTS_MASK: u32 = 255;
/// `JSCLASS_IS_GLOBAL`; a global's class counts SpiderMonkey's own global slots in its reserved slots.
pub const CLASS_IS_GLOBAL: u32 = 0x2_0000;

/// `JSCLASS_RESERVED_SLOTS(clasp)`: how many reserved slots objects of this class get.
pub fn reserved_slot_count(class_flags: u32) -> u32 //@ret
//@ ensures ret <= 255,
{
    //@ assert((class_flags >> 8u32) & 255u32 <= 255u32) by (bit_vector);
    (class_flags >> RESERVED_SLOTS_SHIFT) & RESERVED_SLOTS_MASK
}

/// The slot to read for `JS_GetReservedSlot(obj, index)`, or `None` (SpiderMonkey asserts; we refuse).
pub fn reserved_slot(count: u32, index: u32) -> Option<usize> //@ret
//@ ensures match ret { Some(i) => i < count && i == index, None => index >= count },
{
    if index < count { Some(index as usize) } else { None }
}

/// The length of the `vp` array a native gets: callee, this, then the arguments, padded with `undefined` up to the function's
/// declared `nargs` as SpiderMonkey does (`CallArgs` code may read `vp[2 + i]` for `i < nargs` without checking `argc`).
/// `None` if it would not fit in memory we are willing to give one call (2^20 values).
pub fn vp_len(argc: u32, nargs: u32) -> Option<usize> //@ret
//@ ensures match ret {
//@     Some(n) => n == 2 + (if argc > nargs { argc } else { nargs }) && n <= 0x10_0002,
//@     None => (if argc > nargs { argc } else { nargs }) > 0x10_0000,
//@ },
{
    let n = if argc > nargs { argc } else { nargs };
    if n > 0x10_0000 {
        return None;
    }
    Some(2 + n as usize)
}

/// The index in `vp` of argument `i`, if `vp` (of length `len`, from `vp_len`) has it.
pub fn arg_slot(len: usize, i: u32) -> Option<usize> //@ret
//@ requires len >= 2,
//@ ensures match ret { Some(k) => k == 2 + i && k < len, None => 2 + i >= len },
{
    if (i as usize) < len - 2 { Some(2 + i as usize) } else { None }
}

/// The bytes a typed array or DataView reads: `length` elements of `element_size` bytes from `byte_offset`, inside a buffer of
/// `buffer_len` bytes. `None` for anything that does not fit, including overflow; `Some((start, end))` with
/// `start <= end <= buffer_len` otherwise.
pub fn view_range(buffer_len: usize, byte_offset: usize, length: usize, element_size: usize) -> Option<(usize, usize)> //@ret
//@ requires element_size >= 1, element_size <= 8,
//@ ensures match ret {
//@     Some((s, e)) => s == byte_offset && e == byte_offset + length * element_size && e <= buffer_len,
//@     None => byte_offset + length * element_size > buffer_len,
//@ },
{
    if byte_offset > buffer_len {
        return None;
    }
    let room = buffer_len - byte_offset;
    // `length * element_size <= room` without multiplying first.
    if length > room / element_size {
        //@ assert(length * element_size > room) by (nonlinear_arith)
        //@     requires length > room / element_size, element_size >= 1;
        return None;
    }
    //@ assert(length * element_size <= room) by (nonlinear_arith)
    //@     requires length <= room / element_size, element_size >= 1;
    Some((byte_offset, byte_offset + length * element_size))
}

/// `JS_CopyStringChars(cx, dest, str)`: the number of chars to copy from a string of `str_len` into `dest_len` slots. SpiderMonkey
/// requires `dest_len >= str_len`; here a shorter `dest` gets as much as fits, never more.
pub fn copy_count(str_len: usize, dest_len: usize) -> usize //@ret
//@ ensures ret <= str_len, ret <= dest_len, ret == str_len || ret == dest_len,
{
    if str_len < dest_len { str_len } else { dest_len }
}

/// `JS_GetStringCharAt(cx, str, index)` and friends: whether `index` is a char of a string of `len`.
pub fn char_index(len: usize, index: usize) -> Option<usize> //@ret
//@ ensures match ret { Some(i) => i == index && i < len, None => index >= len },
{
    if index < len { Some(index) } else { None }
}

/// The bytes of an object with `slots` reserved slots after a header of `header` bytes, 8 bytes each. `None` on overflow
/// or above the engine's 4 GiB heap.
pub fn object_bytes(header: usize, slots: u32) -> Option<usize> //@ret
//@ ensures match ret { Some(n) => n == header + 8 * slots && n <= 0x1_0000_0000, None => header + 8 * slots > 0x1_0000_0000 },
{
    let slot_bytes = slots as u64 * 8;
    if header as u64 > 0x1_0000_0000 || slot_bytes > 0x1_0000_0000 - header as u64 {
        return None;
    }
    Some(header + slot_bytes as usize)
}
