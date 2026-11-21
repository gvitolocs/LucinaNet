/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// What the store buffer of `engine/gc.rs` decides: after a post-write barrier wrote `next` into a `Heap<T>` slot, is the slot
// recorded or removed; after a GC updated a slot, does it stay. `record_cell_slot`, `record_value_slot` and `update_store_buffer`
// call these two functions and hold no copy of the rule. Verus checks this very file (the method of
// `scripts/verify-shipped.sh`): `//@` lines hold the proof, and the file is checked together with `boundary/value_bits.rs`,
// whose `kind` decides what a heap pointer looks like.
//
// The buffer is a set of slot addresses; a moving GC visits exactly that set. What must hold for the GC never to miss a pointer or
// touch freed memory is a property of every sequence of operations, so it is proved on a model of the buffer: `mem` maps each live
// slot to its content, `rec` is the recorded set, and the four operations are what the glue does:
//   create    a `Heap` comes to life holding its initial value (null; undefined for a value), no barrier;
//   write     the barrier runs with the new content: recorded if it is a heap pointer, removed if not;
//   drop      `Heap`'s `Drop` writes the initial value through the barrier, then the memory is gone;
//   gc        every recorded slot gets the forwarded address of its object (null, or undefined for a value, if it died), and a slot
//             that no longer holds a pointer leaves the set.
//
// Proved: from the empty buffer, after any sequence of operations, `rec` is exactly the set of live slots whose content is a heap
// pointer. So no live slot with a pointer is missed by a GC, a slot written with null or a non-pointer, or dropped, is not in the
// set, and no freed slot is ever written by a later GC. After a GC every recorded slot holds its forwarded address and
// every slot that holds a pointer was one of them.
//
// Not proved (trusted): that the HashSet's insert, remove and retain do what the model's set operations do, that every write to
// a `Heap` field goes through its barrier (SpiderMonkey's `Heap<T>` does; a raw write through `get_unsafe` does not), and that
// the updater gives a pointer its object's new address. The model keeps no addresses of objects, only whether a content is a pointer.

use crate::boundary::value_bits;

//@ /// Whether the bits in a slot are a heap pointer the GC must follow: a cell slot holds a pointer if it is not null, a value slot
//@ /// if its bits are `value_bits::kind` `Pointer` (not zero, top 16 bits zero).
//@ pub open spec fn ptr_content(is_value: bool, c: u64) -> bool {
//@     if is_value { c != 0 && value_bits::tag_of(c) == 0 } else { c != 0 }
//@ }

//@ /// What a new or dropped `Heap` holds: null, and for a value undefined.
//@ pub open spec fn init_content(is_value: bool) -> u64 { if is_value { 0x8006u64 << 48u64 } else { 0 } }

//@ pub proof fn init_is_not_a_pointer(is_value: bool)
//@     ensures !ptr_content(is_value, init_content(is_value)),
//@ {
//@     assert((0x8006u64 << 48u64) >> 48u64 == 0x8006u64) by (bit_vector);
//@ }

/// After a barrier wrote `content` into a cell slot (a `JSObject*`, `JSString*`, `BigInt*` or `JSScript*` field), or after a GC
/// updated it: the slot is in the buffer exactly when it is not null.
#[inline]
pub fn cell_recorded(content: usize) -> bool //@ret
//@ ensures ret == ptr_content(false, content as u64),
{
    content != 0
}

/// The same for a `JS::Value` slot: the slot is in the buffer exactly when the bits are a heap pointer (a double, an int, a
/// boolean, undefined, null and the zero word never are).
#[inline]
pub fn value_recorded(bits: u64) -> bool //@ret
//@ ensures ret == ptr_content(true, bits),
{
    bits != 0
        && match value_bits::kind(bits) {
            value_bits::Kind::Pointer => true,
            _ => false,
        }
}

//@ /// The model of one buffer, for cells (`is_value` false) or for values.
//@ pub struct Buffer {
//@     pub mem: Map<int, u64>,
//@     pub rec: Set<int>,
//@ }

//@ pub open spec fn empty_buffer() -> Buffer { Buffer { mem: Map::empty(), rec: Set::empty() } }

//@ /// The recorded set is exactly the live slots that hold a pointer.
//@ pub open spec fn inv(b: Buffer, is_value: bool) -> bool {
//@     forall|s: int| #![trigger b.rec.contains(s)] b.rec.contains(s) <==> (b.mem.dom().contains(s) && ptr_content(is_value, b.mem[s]))
//@ }

//@ /// A `Heap` comes to life at `s` holding its initial value.
//@ pub open spec fn create(b: Buffer, is_value: bool, s: int) -> Buffer {
//@     Buffer { mem: b.mem.insert(s, init_content(is_value)), rec: b.rec }
//@ }

//@ /// The barrier wrote `v` into the live slot `s`.
//@ pub open spec fn write(b: Buffer, is_value: bool, s: int, v: u64) -> Buffer {
//@     Buffer { mem: b.mem.insert(s, v), rec: if ptr_content(is_value, v) { b.rec.insert(s) } else { b.rec.remove(s) } }
//@ }

//@ /// `Drop`: the initial value goes through the barrier, then the slot is gone.
//@ pub open spec fn drop_slot(b: Buffer, is_value: bool, s: int) -> Buffer {
//@     let w = write(b, is_value, s, init_content(is_value));
//@     Buffer { mem: w.mem.remove(s), rec: w.rec }
//@ }

//@ /// What the updater does to the content `c` of a recorded slot: the forwarded address, or null / undefined if its object died.
//@ pub open spec fn forwarded(f: Map<u64, u64>, is_value: bool, c: u64) -> u64 {
//@     if f.dom().contains(c) { f[c] } else { init_content(is_value) }
//@ }

//@ /// The contents after a GC: recorded slots are updated, no other slot is touched.
//@ pub open spec fn gc_mem(b: Buffer, is_value: bool, f: Map<u64, u64>) -> Map<int, u64> {
//@     Map::new(
//@         b.mem.dom(),
//@         |s: int| if b.rec.contains(s) { forwarded(f, is_value, b.mem[s]) } else { b.mem[s] },
//@     )
//@ }

//@ /// A GC: every recorded slot is updated, and the ones that no longer hold a pointer leave the set.
//@ pub open spec fn gc(b: Buffer, is_value: bool, f: Map<u64, u64>) -> Buffer {
//@     let mem = gc_mem(b, is_value, f);
//@     Buffer { mem, rec: b.rec.filter(|s: int| ptr_content(is_value, mem[s])) }
//@ }

//@ pub proof fn inv_empty(is_value: bool)
//@     ensures inv(empty_buffer(), is_value),
//@ {
//@ }

//@ pub proof fn inv_create(b: Buffer, is_value: bool, s: int)
//@     requires inv(b, is_value), !b.mem.dom().contains(s),
//@     ensures inv(create(b, is_value, s), is_value),
//@ {
//@     init_is_not_a_pointer(is_value);
//@ }

//@ /// A write of null or any non-pointer takes the slot out; a write of a pointer puts it in.
//@ pub proof fn inv_write(b: Buffer, is_value: bool, s: int, v: u64)
//@     requires inv(b, is_value), b.mem.dom().contains(s),
//@     ensures inv(write(b, is_value, s, v), is_value),
//@         !ptr_content(is_value, v) ==> !write(b, is_value, s, v).rec.contains(s),
//@         ptr_content(is_value, v) ==> write(b, is_value, s, v).rec.contains(s),
//@ {
//@ }

//@ /// A dropped slot is neither recorded nor live, so no later GC writes it.
//@ pub proof fn inv_drop(b: Buffer, is_value: bool, s: int)
//@     requires inv(b, is_value), b.mem.dom().contains(s),
//@     ensures inv(drop_slot(b, is_value, s), is_value),
//@         !drop_slot(b, is_value, s).rec.contains(s), !drop_slot(b, is_value, s).mem.dom().contains(s),
//@ {
//@     init_is_not_a_pointer(is_value);
//@ }

//@ /// After a GC the invariant holds again; every slot that held a pointer was recorded and now holds its forwarded address; a slot
//@ /// that was not recorded is untouched; no slot is created or freed.
//@ pub proof fn inv_gc(b: Buffer, is_value: bool, f: Map<u64, u64>)
//@     requires inv(b, is_value),
//@     ensures inv(gc(b, is_value, f), is_value),
//@         gc(b, is_value, f).mem.dom() =~= b.mem.dom(),
//@         forall|s: int| #![trigger b.mem.dom().contains(s)]
//@             b.mem.dom().contains(s) && ptr_content(is_value, b.mem[s]) ==> gc(b, is_value, f).mem[s] == forwarded(f, is_value, b.mem[s]),
//@         forall|s: int| #![trigger b.mem.dom().contains(s)]
//@             b.mem.dom().contains(s) && !ptr_content(is_value, b.mem[s]) ==> gc(b, is_value, f).mem[s] == b.mem[s],
//@         forall|s: int| #![trigger gc(b, is_value, f).rec.contains(s)]
//@             gc(b, is_value, f).rec.contains(s) ==> gc(b, is_value, f).mem.dom().contains(s) && b.rec.contains(s),
//@         forall|s: int| #![trigger b.rec.contains(s)]
//@             b.rec.contains(s) && !ptr_content(is_value, gc(b, is_value, f).mem[s]) ==> !gc(b, is_value, f).rec.contains(s),
//@         forall|s: int| #![trigger b.rec.contains(s)]
//@             b.rec.contains(s) && !f.dom().contains(b.mem[s])
//@                 ==> gc(b, is_value, f).mem[s] == init_content(is_value) && !gc(b, is_value, f).rec.contains(s),
//@         forall|s: int| #![trigger b.rec.contains(s)]
//@             b.rec.contains(s) && f.dom().contains(b.mem[s]) ==> gc(b, is_value, f).mem[s] == f[b.mem[s]],
//@ {
//@     init_is_not_a_pointer(is_value);
//@ }

//@ pub enum Op {
//@     Create(int),
//@     Write(int, u64),
//@     Drop(int),
//@     Gc(Map<u64, u64>),
//@ }

//@ /// One operation; one that does not apply (a write to a slot that is not live) does nothing.
//@ pub open spec fn step(b: Buffer, is_value: bool, op: Op) -> Buffer {
//@     match op {
//@         Op::Create(s) => if b.mem.dom().contains(s) { b } else { create(b, is_value, s) },
//@         Op::Write(s, v) => if b.mem.dom().contains(s) { write(b, is_value, s, v) } else { b },
//@         Op::Drop(s) => if b.mem.dom().contains(s) { drop_slot(b, is_value, s) } else { b },
//@         Op::Gc(f) => gc(b, is_value, f),
//@     }
//@ }

//@ pub open spec fn run(ops: Seq<Op>, is_value: bool) -> Buffer
//@ decreases ops.len(),
//@ {
//@     if ops.len() == 0 { empty_buffer() } else { step(run(ops.drop_last(), is_value), is_value, ops.last()) }
//@ }

//@ /// For cells and for values, after any sequence of creates, writes, drops and GCs: the recorded set is exactly the live slots
//@ /// that hold a pointer.
//@ pub proof fn inv_run(ops: Seq<Op>, is_value: bool)
//@     ensures inv(run(ops, is_value), is_value),
//@     decreases ops.len(),
//@ {
//@     if ops.len() == 0 {
//@         inv_empty(is_value);
//@     } else {
//@         let prev = ops.drop_last();
//@         inv_run(prev, is_value);
//@         let b = run(prev, is_value);
//@         match ops.last() {
//@             Op::Create(s) => { if !b.mem.dom().contains(s) { inv_create(b, is_value, s); } },
//@             Op::Write(s, v) => { if b.mem.dom().contains(s) { inv_write(b, is_value, s, v); } },
//@             Op::Drop(s) => { if b.mem.dom().contains(s) { inv_drop(b, is_value, s); } },
//@             Op::Gc(f) => { inv_gc(b, is_value, f); },
//@         }
//@     }
//@ }
