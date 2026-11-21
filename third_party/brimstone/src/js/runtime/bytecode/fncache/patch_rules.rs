// What a scope patch may do to the bytecode of a cached function on a hit. A patched scope instruction is written again from the new
// function's scope layout; it must come out at the same offset and with the same width as the instruction it replaces, or the
// function is refused, and the write must stay inside that instruction. `BytecodeFunctionGenerator::apply_scope_patches` asks these
// functions and holds no copy of the rules. Verus checks this very file (the method of LucinaOS `scripts/verify-shipped.sh`).
//
// Proved: `patch_end` answers with the end of the instruction only when the re-encoded instruction ends exactly there (so it is
// as wide as the old one, from the same offset) and that end is inside the bytecode, and refuses otherwise, whatever the numbers
// are (no overflow); `apply_patch` changes the bytes of `[offset, end)` to the new instruction's bytes and no other byte, and not
// the length. `op_matches` is the one rule on which operation may address which kind of slot.
//
// Not proved (trusted): that the writer encodes the instruction the patch's `op` names, with the register, index and depth the
// caller computed (the runtime verify mode compares every hit with a fresh generation).

/// Whether the patch's operation (0 load from scope, 1 store to scope, 2 load from module, 3 store to module) fits the slot's kind
/// (1 = a module slot, otherwise a scope slot).
pub fn op_matches(kind: u8, op: u8) -> bool //@ret
//@ ensures ret == ((kind == 1) == (op >= 2)),
{
    (kind == 1) == (op >= 2)
}

/// The end of the patched instruction, if the patch may be applied: the re-encoded instruction (written from `offset`) ended at
/// `produced_len`, which must be where an instruction of `len` bytes from `offset` ends, and that must be inside `bytecode_len`.
pub fn patch_end(offset: u32, len: u32, produced_len: usize, bytecode_len: usize) -> Option<usize> //@ret
//@ ensures match ret {
//@     Some(end) => end == offset as int + len as int && produced_len == end && end <= bytecode_len,
//@     None => produced_len as int != offset as int + len as int || offset as int + len as int > bytecode_len as int,
//@ },
{
    let end = offset as u64 + len as u64;
    if produced_len as u64 != end || end > bytecode_len as u64 {
        None
    } else {
        Some(end as usize)
    }
}

/// Copy the bytes of the re-encoded instruction over the old one: `[offset, end)` of `bytes` into the same range of `bytecode`.
pub fn apply_patch(bytecode: &mut Vec<u8>, bytes: &[u8], offset: usize, end: usize)
//@ requires offset <= end, end <= old(bytecode)@.len(), end <= bytes@.len(),
//@ ensures final(bytecode)@.len() == old(bytecode)@.len(),
//@     forall|i: int| 0 <= i < offset || end <= i < old(bytecode)@.len() ==> final(bytecode)@[i] == old(bytecode)@[i],
//@     forall|i: int| offset <= i < end ==> final(bytecode)@[i] == bytes@[i],
{
    let mut i = offset;
    while i < end
    //@ invariant offset <= i <= end, end <= bytecode@.len(), end <= bytes@.len(), bytecode@.len() == old(bytecode)@.len(),
    //@     forall|j: int| 0 <= j < offset || end <= j < old(bytecode)@.len() ==> bytecode@[j] == old(bytecode)@[j],
    //@     forall|j: int| offset <= j < i ==> bytecode@[j] == bytes@[j],
    //@ decreases end - i,
    {
        bytecode[i] = bytes[i];
        i += 1;
    }
}
