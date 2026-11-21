#!/usr/bin/env bash
# Shows that a shipped file of src/js/runtime/bytecode/fncache is what keeps Verus on its rules: each `try` plants one bug in a copy
# of the file, and Verus must refuse every mutant.
#   verification/fncache/mutants.sh <name> <path of the shipped file>        (needs `verus` on PATH, or VERUS=...)
# Normally started by verification/fncache/run.sh <name>, which copies the file and this script into a directory of their own and runs
# them on the proof host (`verus-remote`), or on the build host (FALLBACK=1). Names: disk_rules, patch_rules, prov_rules, wire, partition.
# MUTANTS_STRICT=1: exit 1 if any mutant was refused without a verification run (a compile error, not a disproof).
# No argument: proof disk_rules, its shipped file read from the repo root, which is how lucina-ci mutants-precheck runs this script;
# then only disk_rules mutants are checked, for the other four names pass the name (each is its own run of the script).
set -uo pipefail
verus=${VERUS:-verus}
name=${1:-disk_rules}
rules=${2:-src/js/runtime/bytecode/fncache/$name.rs}
work=$(mktemp -d "${TMPDIR:-/tmp}/fncache-mutants.XXXXXX")
trap 'rm -rf "${work:?}"' EXIT

# What Verus reads from the shipped file: the mechanical steps of LucinaOS scripts/verify-shipped.sh.
make_verus_input() {
    local body
    body=$(sed -E -e '/\/\/@omit$/d' -e 's/-> (.*[^ ]) \/\/@ret$/-> (ret: \1)/' -e 's/^( *)\/\/@ ?/\1\/*proof*\/ /' "$1")
    printf '#![allow(unused)]\nuse vstd::prelude::*;\nverus! {\n%s\n}\nfn main() {}\n' "$body"
}

# Undoing those steps must give the shipped code back, so that what Verus checks is the code that is compiled.
shipped_code=$(sed -E -e '/^ *\/\/@/d' -e '/\/\/@omit$/d' -e 's/ \/\/@ret$//' "$rules")
read_back=$(make_verus_input "$rules" | sed -e '1,3d' | head -n -2 | grep -vF '/*proof*/' | sed -E 's/-> \(ret: (.*)\)$/-> \1/')
if [ "$shipped_code" != "$read_back" ]; then
    echo "WHAT VERUS READS IS NOT THE SHIPPED CODE"
    diff <(echo "$shipped_code") <(echo "$read_back") | head -20
    exit 1
fi

# The unchanged file must verify, or nothing below means anything.
make_verus_input "$rules" > "$work/base.rs"
if ! "$verus" --triggers-mode silent "$work/base.rs" > "$work/base.out" 2>&1; then
    echo "BASE FAILS"
    cat "$work/base.out"
    exit 1
fi
echo "baseline: verifies"

fail=0
refused=0
weak=0
try() {
    local from=$1 to=$2
    local text; text=$(<"$rules")
    case "$text" in
        *"$from"*) ;;
        *) echo "NOT FOUND: $from"; fail=1; return ;;
    esac
    local mut; mut=${text/"$from"/"$to"}
    if [ "$mut" = "$text" ]; then echo "NOT CHANGED: $from"; fail=1; return; fi
    printf '%s\n' "$mut" > "$work/mutant-src.rs"
    make_verus_input "$work/mutant-src.rs" > "$work/mutant.rs"
    if "$verus" --triggers-mode silent "$work/mutant.rs" > "$work/verus.out" 2>&1; then
        echo "NOT REFUSED: $from -> $to"
        cat "$work/verus.out"
        fail=1
    else
        if grep -qE 'verification results|simplifies to|not satisfied|assertion failed|possible arithmetic|may not terminate|cannot show|recommendation not met' "$work/verus.out" && ! grep -q 'error\[E' "$work/verus.out"; then
            refused=$((refused + 1))
            echo "refused: $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
        else
            echo "refused (NO VERIFICATION RUN: compile or other error): $from -> $to ($(grep -c "^error" "$work/verus.out") errors)"
            weak=$((weak + 1))
        fi
    fi
}

mutants_disk_rules() {
    # The bound: a record larger than it is never stored; room is made for the new record's bytes too.
    try "    size <= max" "    size < max"
    try "while self.total > self.max || extra > self.max - self.total" "while self.total > self.max"
    try "while self.total > self.max || extra > self.max - self.total" "while self.total >= self.max || extra > self.max - self.total"
    try "let evicted = self.evict_for(size);" "let evicted = self.evict_for(0);"
    try "let r = self.evict_for(0);" "let r = self.evict_for(1);"
    # Least recently used first, and the count kept in step.
    try "let (key, size) = self.entries.remove(0);" "let (key, size) = self.entries.remove(self.entries.len() - 1);"
    try $'            self.total -= size;\n            out.push(key);' $'            out.push(key);'
    try "                self.total -= old_size;" "                let _ = old_size;"
    try "        self.total += size;" "        self.total += 1;"
    try "                lru.total += size;" "                lru.total += 1;"
    try "if size > u64::MAX - lru.total {" "if size > u64::MAX {"
    try "        dropped.append(&mut evicted);" "        evicted.clear();"
    # A lookup: the record goes to the back, nothing else changes.
    try $'                self.entries.push(entry);\n' ''
    try "let entry = self.entries.remove(i);" "let entry = self.entries.remove(0);"
    try "            if self.entries[i].0 == key {" "            if self.entries[i].0 != key {"
    try "        let mut i = 0;
        while i < self.entries.len()" "        let mut i = 1;
        while i < self.entries.len()"
}

mutants_patch_rules() {
    # Which operation may address which kind of slot.
    try "(kind == 1) == (op >= 2)" "(kind == 1) != (op >= 2)"
    try "(kind == 1) == (op >= 2)" "(kind == 1) == (op >= 3)"
    try "(kind == 1) == (op >= 2)" "(kind == 1) == (op >= 1)"
    # The instruction must end where the old one did (same width from the same offset), inside the bytecode.
    try "if produced_len as u64 != end || end > bytecode_len as u64 {" "if produced_len as u64 > end || end > bytecode_len as u64 {"
    try "if produced_len as u64 != end || end > bytecode_len as u64 {" "if produced_len as u64 < end || end > bytecode_len as u64 {"
    try "if produced_len as u64 != end || end > bytecode_len as u64 {" "if produced_len as u64 != end {"
    try "if produced_len as u64 != end || end > bytecode_len as u64 {" "if produced_len as u64 != end || end >= bytecode_len as u64 {"
    try "if produced_len as u64 != end || end > bytecode_len as u64 {" "if produced_len as u64 != end || end > bytecode_len as u64 + 1 {"
    try "let end = offset as u64 + len as u64;" "let end = offset as u64 + len as u64 + 1;"
    try "let end = offset as u64 + len as u64;" "let end = len as u64;"
    try "Some(end as usize)" "Some(end as usize + 1)"
    # The copy stays inside [offset, end).
    try "    let mut i = offset;" "    let mut i = 0;"
    try "    let mut i = offset;" "    let mut i = offset + 1;"
    try "    while i < end
    //@ invariant offset" "    while i <= end
    //@ invariant offset"
    try "        bytecode[i] = bytes[i];" "        bytecode[i] = bytes[i - 1];"
    try "        bytecode[i] = bytes[i];
        i += 1;" "        bytecode[i] = bytes[i];
        i += 2;"
}

mutants_prov_rules() {
    # Which provenances are source texts, and when a merge of two is a conflict.
    try "Prov::Node(_) | Prov::Quasi(..) | Prov::ScopeBinding(..) => true," "Prov::Node(_) | Prov::Quasi(..) => true,"
    try "Prov::Node(_) | Prov::Quasi(..) | Prov::ScopeBinding(..) => true," "Prov::Node(_) | Prov::Quasi(..) | Prov::ScopeBinding(..) | Prov::PrivateName(..) => true,"
    try "!same && !(existing.is_source_text() && new.is_source_text())" "!same && !(existing.is_source_text() || new.is_source_text())"
    try "!same && !(existing.is_source_text() && new.is_source_text())" "!same"
    try "!same && !(existing.is_source_text() && new.is_source_text())" "!same && !existing.is_source_text()"
    try "(Prov::NestedFunction(a), Prov::NestedFunction(b)) => a == b," "(Prov::NestedFunction(a), Prov::NestedFunction(b)) => true,"
    try "(Prov::ClassNames(a), Prov::ClassNames(b)) => a == b," "(Prov::ClassNames(a), Prov::ClassNames(b)) => true,"
    try "(Prov::ScopeNames(a), Prov::ScopeNames(b)) => a == b," "(Prov::ScopeNames(a), Prov::ScopeNames(b)) => true,"
    try "(Prov::PrivateName(a, s), Prov::PrivateName(b, t)) => a == b && s == t," "(Prov::PrivateName(a, s), Prov::PrivateName(b, t)) => a == b,"
    try "(Prov::Intrinsic, Prov::Intrinsic) => true," "(Prov::Intrinsic, Prov::Intrinsic) => false,"
    try "        _ => false,
    };
    !same" "        _ => true,
    };
    !same"
    # Where the text of a provenance is read.
    try "Prov::Intrinsic => TextSource::Stored," "Prov::Intrinsic => TextSource::NoText,"
    try "Prov::Quasi(t, i) => TextSource::Quasi(t, i)," "Prov::Quasi(t, i) => TextSource::Quasi(i, t),"
    try "        _ => TextSource::NoText," "        Prov::ScopeBinding(s, b) => TextSource::Quasi(s, b),
        _ => TextSource::NoText,"
    try "if len == 0 { None } else { Some(1) }" "Some(1)"
    try "if len == 0 { None } else { Some(1) }" "if len == 0 { None } else { Some(2) }"
    try "        Some(0)
    }" "        Some(1)
    }"
    # The model of reading the new AST, and of the builder.
    try "Some(env.node[o].skip(1))" "Some(env.node[o].skip(0))"
    try "Prov::Intrinsic => Some(stored)," "Prov::Intrinsic => Some(Seq::empty()),"
    try "Some(env.quasi[(t, i)])" "Some(env.quasi[(i, t)])"
    try "Prov::PrivateName(o, s) => TextSource::Private(o, s)," "Prov::PrivateName(o, s) => TextSource::Private(o, true),"
    try "conflict: r.conflict || !compat(r.all[idx][0], p)" "conflict: !compat(r.all[idx][0], p)"
    try "conflict: r.conflict || !compat(r.all[idx][0], p)" "conflict: r.conflict"
    try "Rec { all: r.all.insert(idx, r.all[idx].push(p)), conflict" "Rec { all: r.all, conflict"
    try "pub open spec fn compat(p: Prov, q: Prov) -> bool { p == q || (is_src(p) && is_src(q)) }" "pub open spec fn compat(p: Prov, q: Prov) -> bool { p == q || is_src(p) || is_src(q) }"
}

mutants_wire() {
    # The writer: every byte of every number.
    try "        self.buf.push((v >> 8) as u8);
        //@ assert(self.buf@ =~= old(self).buf@ + enc16(v));" "        self.buf.push((v >> 7) as u8);
        //@ assert(self.buf@ =~= old(self).buf@ + enc16(v));"
    try "        self.buf.push((v >> 24) as u8);
        //@ assert(self.buf@ =~= old(self).buf@ + enc32(v));" "        self.buf.push((v >> 16) as u8);
        //@ assert(self.buf@ =~= old(self).buf@ + enc32(v));"
    try "        self.buf.push((v >> 56) as u8);" "        self.buf.push((v >> 48) as u8);"
    try "        self.u8(if v { 1 } else { 0 });" "        self.u8(if v { 0 } else { 1 });"
    try "        self.usize(v.len());" "        self.usize(v.len() + 1);"
    try "            self.buf.push(v[i]);" "            self.buf.push(v[0]);"
    # The reader: bounds, position, and every byte of every number.
    try "        if n > self.buf.len() - self.pos {" "        if n >= self.buf.len() - self.pos {"
    try "        if n > self.buf.len() - self.pos {" "        if n > self.buf.len() {"
    try "        self.pos += n;
        Some(start)" "        self.pos += n + 1;
        Some(start)"
    try "        self.pos += n;
        Some(start)" "        Some(start)"
    try "                Some((self.buf[s] as u16) | ((self.buf[s + 1] as u16) << 8))" "                Some((self.buf[s] as u16) | ((self.buf[s + 1] as u16) << 7))"
    try "                        | ((self.buf[s + 3] as u32) << 24)," "                        | ((self.buf[s + 3] as u32) << 16),"
    try "                        | ((self.buf[s + 7] as u64) << 56)," "                        | ((self.buf[s + 6] as u64) << 56),"
    try "            Some(v) => Some(v as i32)," "            Some(v) => Some((v as i32) >> 1),"
    try "            Some(v) => Some(v as usize)," "            Some(v) => Some(v as usize >> 1),"
    try "            Some(v) => Some(v != 0)," "            Some(v) => Some(v == 0),"
    try "            Some(start) => Some((start, start + len))," "            Some(start) => Some((start, start + len + 1)),"
    try "        self.pos == self.buf.len()
    }" "        self.pos <= self.buf.len()
    }"
    # The header: the magic, the version, the build id and the payload length are all enforced, in what is written too.
    try "    if buf.len() < HEADER_LEN {" "    if buf.len() <= HEADER_LEN {"
    try "        if buf[i] != magic[i] {" "        if false {"
    try "    if found_version != version {" "    if false {"
    try "    if found_version != version {" "    if found_version == version {"
    try "    if found_build != build {" "    if false {"
    try "    if buf.len() != HEADER_LEN + payload_len as usize {" "    if false {"
    try "    if buf.len() != HEADER_LEN + payload_len as usize {" "    if buf.len() < HEADER_LEN + payload_len as usize {"
    try "    w.u64((key >> 64) as u64);" "    w.u64((key >> 63) as u64);"
    try "    w.u64(key as u64);" "    w.u64(0);"
    try "    w.u64(partition as u64);" "    w.u64(key as u64);"
    try "    w.u64((partition >> 64) as u64);" "    w.u64((partition >> 32) as u64);"
    try "    let key = (key_lo as u128) | ((key_hi as u128) << 64);" "    let key = (key_lo as u128) | ((key_hi as u128) << 63);"
    try "    let partition = (partition_lo as u128) | ((partition_hi as u128) << 64);" "    let partition = (key_lo as u128) | ((partition_hi as u128) << 64);"
    try "    Some((key, partition))" "    Some((partition, key))"
    try "    w.u32(version);" "    w.u32(version ^ 1);"
    try "    w.u64(build);" "    w.u64(0);"
    try "    w.u32(payload_len);" "    w.u32(0);"
    try "    let mut r = Reader { buf, pos: 4 };" "    let mut r = Reader { buf, pos: 5 };"
}

mutants_partition() {
    # The decision needs the key and the partition, both equal.
    try "    entry_key == key && entry_partition == partition
}" "    entry_key == key
}"
    try "    entry_key == key && entry_partition == partition
}" "    entry_partition == partition
}"
    try "    entry_key == key && entry_partition == partition
}" "    entry_key == key || entry_partition == partition
}"
    try "    entry_key == key && entry_partition == partition
}" "    entry_key == key && entry_partition != partition
}"
    try "    entry_key == key && entry_partition == partition
}" "    entry_key == key && entry_partition == key
}"
    try "    entry_key == key && entry_partition == partition
}" "    entry_key == partition && entry_partition == key
}"
    try "    entry_key == key && entry_partition == partition
}" "    true
}"
    try "    entry_key == key && entry_partition == partition
}" "    (entry_key == key && entry_partition == partition) || entry_partition == 0
}"
    # The model of the table lookup.
    try "if table.dom().contains(key) && hit(key, table[key], key, partition)" "if table.dom().contains(key)"
    try "pub open spec fn hit(entry_key: u128, entry_partition: u128, key: u128, partition: u128) -> bool {
//@     entry_key == key && entry_partition == partition" "pub open spec fn hit(entry_key: u128, entry_partition: u128, key: u128, partition: u128) -> bool {
//@     entry_key == key || entry_partition == partition"
}

case "$name" in
    disk_rules) mutants_disk_rules ;;
    patch_rules) mutants_patch_rules ;;
    prov_rules) mutants_prov_rules ;;
    wire) mutants_wire ;;
    partition) mutants_partition ;;
    *) echo "unknown proof $name"; exit 2 ;;
esac
echo "mutants: $refused refused by verification, $weak refused without a verification run"
if [ "${MUTANTS_STRICT:-0}" = "1" ] && [ "$weak" -gt 0 ]; then exit 1; fi
exit "$fail"
