// Provenance of the constants of a cached function: where each constant table entry's value comes from, how it is read again from
// a structurally equal function's AST on a hit, and when two entries of the generator's constant table that were merged make the
// function uncacheable. `constant_table_builder.rs` (`record_prov`) and `template.rs` (`resolve_text`) call these functions and
// hold no copy of the rules. Verus checks this very file (the method of LucinaOS `scripts/verify-shipped.sh`): `//@` lines hold
// the proof.
//
// The structural key fixes which source texts of a function are equal, not what they are. So an instantiated entry is read from the
// new AST by the coordinates in its provenance, and a generator-intrinsic value is copied. Proved here, on a model of the AST
// (`Env`: the text at each node ordinal and each template quasi) and of the builder (`Rec`: every provenance recorded for every
// constant index, with the conflict flag):
//  1. `source_of` names for each provenance the AST coordinates it is read from, and nothing else; two source-derived provenances
//     with the same coordinates are the same provenance, so an entry is mapped to exactly the new function's value at its own
//     coordinates (`resolve_node`, `resolve_quasi`, `resolve_private`); an intrinsic entry resolves to its stored text whatever the
//     new function is (`resolve_intrinsic_copied`).
//  2. On the references of a function, instantiating is a bijection: if the key's guarantee holds (two references have equal text
//     in the new function iff they had in the old one, `key_fixes_equality`), then two entries map to the same new value iff
//     their old values were equal (`remap_bijection`), intrinsic entries included.
//  3. `conflicts` refuses a merge of two provenances unless they are the same or both source texts. After any sequence of
//     `record` calls without a conflict, all provenances recorded for one constant index are pairwise the same or both source texts
//     (`record_keeps_inv`, which also covers that the code compares only with the first), and then they all resolve to the same
//     new value (`no_double_remap`): nothing that the instantiation could remap two ways passes.
//
// Not proved (trusted): that the lookups of `resolve_text` read the AST at the coordinates `source_of` names (they are one match
// arm each), that the key guarantee `key_fixes_equality` holds (that is `key.rs`), and the Wtf-8 well-formedness checks, which only
// ever refuse.

/// Where a constant table entry's value comes from, so that it can be rebuilt from a structurally equal function's AST when the
/// function cache hits.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum Prov {
    /// Generator-intrinsic value, stored verbatim in the template.
    Intrinsic,
    /// Value derived from the AST node with this ordinal in the canonical walk of the function.
    Node(u32),
    /// Cooked text of quasi `u32` of the template literal node with ordinal `u32`.
    Quasi(u32, u32),
    /// Name of the private identifier node with this ordinal, with the leading '#' stripped if the bool is set.
    PrivateName(u32, bool),
    /// Name of the binding with this index in the scope with this ordinal.
    ScopeBinding(u32, u32),
    /// A nested function: index into the pending functions queue at enqueue time.
    NestedFunction(u32),
    /// A ScopeNames heap item for the scope with this ordinal.
    ScopeNames(u32),
    /// A ClassNames heap item for the class node with this ordinal. The method list and home object locations are recorded
    /// separately, keyed by constant index.
    ClassNames(u32),
}

/// Where `resolve_text` reads the text of a provenance.
#[derive(Clone, Copy)]
#[derive(Debug, PartialEq, Eq)] //@omit
pub enum TextSource {
    /// The text of the node with this ordinal.
    Node(u32),
    /// The cooked text of this quasi of this template literal.
    Quasi(u32, u32),
    /// The name of this private identifier, '#' stripped if the bool is set.
    Private(u32, bool),
    /// The text stored in the template.
    Stored,
    /// No text: the provenance is not a string (or is one `resolve_text` does not rebuild).
    NoText,
}

//@ pub open spec fn is_src(p: Prov) -> bool { p is Node || p is Quasi || p is ScopeBinding }
//@ /// Two provenances that may share a constant index: the same, or both source texts (whose equality the key fixes).
//@ pub open spec fn compat(p: Prov, q: Prov) -> bool { p == q || (is_src(p) && is_src(q)) }

impl Prov {
    /// Whether this provenance is a source text (identifier, string or template text) whose equality with other source texts is
    /// fixed by the structural key.
    pub fn is_source_text(&self) -> bool //@ret
    //@ ensures ret == is_src(*self),
    {
        match self {
            Prov::Node(_) | Prov::Quasi(..) | Prov::ScopeBinding(..) => true,
            _ => false,
        }
    }
}

/// Two entries deduplicated into one constant index, the second with provenance `new`: is the function uncacheable? Only the
/// first provenance recorded for the index is kept, so the merge must be harmless against it.
pub fn conflicts(existing: Prov, new: Prov) -> bool //@ret
//@ ensures ret == !compat(existing, new),
{
    let same = match (existing, new) {
        (Prov::Intrinsic, Prov::Intrinsic) => true,
        (Prov::Node(a), Prov::Node(b)) => a == b,
        (Prov::Quasi(a, b), Prov::Quasi(c, d)) => a == c && b == d,
        (Prov::PrivateName(a, s), Prov::PrivateName(b, t)) => a == b && s == t,
        (Prov::ScopeBinding(a, b), Prov::ScopeBinding(c, d)) => a == c && b == d,
        (Prov::NestedFunction(a), Prov::NestedFunction(b)) => a == b,
        (Prov::ScopeNames(a), Prov::ScopeNames(b)) => a == b,
        (Prov::ClassNames(a), Prov::ClassNames(b)) => a == b,
        _ => false,
    };
    !same && !(existing.is_source_text() && new.is_source_text())
}

/// Where the text of a provenance is read. Only these four kinds are rebuilt from the AST or the template.
pub fn source_of(prov: Prov) -> TextSource //@ret
//@ ensures match prov {
//@     Prov::Node(o) => ret == TextSource::Node(o),
//@     Prov::Quasi(t, i) => ret == TextSource::Quasi(t, i),
//@     Prov::PrivateName(o, s) => ret == TextSource::Private(o, s),
//@     Prov::Intrinsic => ret == TextSource::Stored,
//@     _ => ret == TextSource::NoText,
//@ },
{
    match prov {
        Prov::Node(o) => TextSource::Node(o),
        Prov::Quasi(t, i) => TextSource::Quasi(t, i),
        Prov::PrivateName(o, strip) => TextSource::Private(o, strip),
        Prov::Intrinsic => TextSource::Stored,
        _ => TextSource::NoText,
    }
}

/// Where the text of a private name starts in the name's bytes (`len` of them): after the '#' when it is stripped, which an empty
/// name cannot be.
pub fn private_name_start(len: usize, strip: bool) -> Option<usize> //@ret
//@ ensures match ret {
//@     Some(start) => start == (if strip { 1int } else { 0int }) && start <= len,
//@     None => strip && len == 0,
//@ },
{
    if strip {
        if len == 0 { None } else { Some(1) }
    } else {
        Some(0)
    }
}

//@ /// The AST of a function as the model sees it: the text at each node ordinal that has one, and of each template quasi.
//@ pub struct Env {
//@     pub node: Map<u32, Seq<u8>>,
//@     pub quasi: Map<(u32, u32), Seq<u8>>,
//@ }

//@ /// The text `resolve_text` builds: read at the coordinates of `source_of`, `stored` for an intrinsic provenance.
//@ pub open spec fn resolve(env: Env, p: Prov, stored: Seq<u8>) -> Option<Seq<u8>> {
//@     match p {
//@         Prov::Node(o) => if env.node.dom().contains(o) { Some(env.node[o]) } else { None },
//@         Prov::Quasi(t, i) => if env.quasi.dom().contains((t, i)) { Some(env.quasi[(t, i)]) } else { None },
//@         Prov::PrivateName(o, strip) => if env.node.dom().contains(o) {
//@             if strip { if env.node[o].len() == 0 { None } else { Some(env.node[o].skip(1)) } } else { Some(env.node[o]) }
//@         } else { None },
//@         Prov::Intrinsic => Some(stored),
//@         _ => None,
//@     }
//@ }

//@ /// Reading again from the new function's AST gives the text at the entry's own coordinates.
//@ pub proof fn resolve_node(env: Env, o: u32, stored: Seq<u8>)
//@     requires env.node.dom().contains(o),
//@     ensures resolve(env, Prov::Node(o), stored) == Some(env.node[o]),
//@ {
//@ }

//@ pub proof fn resolve_quasi(env: Env, t: u32, i: u32, stored: Seq<u8>)
//@     requires env.quasi.dom().contains((t, i)),
//@     ensures resolve(env, Prov::Quasi(t, i), stored) == Some(env.quasi[(t, i)]),
//@ {
//@ }

//@ pub proof fn resolve_private(env: Env, o: u32, strip: bool, stored: Seq<u8>)
//@     requires env.node.dom().contains(o), !strip || env.node[o].len() > 0,
//@     ensures resolve(env, Prov::PrivateName(o, strip), stored)
//@         == Some(if strip { env.node[o].skip(1) } else { env.node[o] }),
//@ {
//@ }

//@ /// An intrinsic entry is copied: whatever the new function is, it gets the stored text.
//@ pub proof fn resolve_intrinsic_copied(env1: Env, env2: Env, stored: Seq<u8>)
//@     ensures resolve(env1, Prov::Intrinsic, stored) == resolve(env2, Prov::Intrinsic, stored),
//@         resolve(env1, Prov::Intrinsic, stored) == Some(stored),
//@ {
//@ }

//@ /// A provenance with a text is read at coordinates only it has.
//@ pub proof fn coordinates_name_the_provenance(p: Prov, q: Prov)
//@     requires p is Node || p is Quasi || p is PrivateName, q is Node || q is Quasi || q is PrivateName,
//@         coords(p) == coords(q),
//@     ensures p == q,
//@ {
//@ }
//@ /// The coordinates of a source-derived provenance, as `source_of` gives them.
//@ pub open spec fn coords(p: Prov) -> TextSource {
//@     match p {
//@         Prov::Node(o) => TextSource::Node(o),
//@         Prov::Quasi(t, i) => TextSource::Quasi(t, i),
//@         Prov::PrivateName(o, s) => TextSource::Private(o, s),
//@         Prov::Intrinsic => TextSource::Stored,
//@         _ => TextSource::NoText,
//@     }
//@ }

//@ /// The guarantee of the structural key that the remap relies on, and all it gives: for the source texts among the references of
//@ /// the function, two of them have equal text in the new function exactly when they had in the old one. It says nothing about a
//@ /// source text against an intrinsic one: that is why a merge of the two is a conflict.
//@ pub open spec fn key_fixes_equality(old: Env, new: Env, stored: Seq<u8>, refs: Set<Prov>) -> bool {
//@     forall|p: Prov, q: Prov| #![trigger refs.contains(p), refs.contains(q)]
//@         refs.contains(p) && refs.contains(q) && is_src(p) && is_src(q)
//@             ==> (resolve(old, p, stored) == resolve(old, q, stored)) == (resolve(new, p, stored) == resolve(new, q, stored))
//@ }

//@ /// On the source texts of a function the remap is a bijection: two entries map to the same new value iff their old values were
//@ /// equal. The table has no two entries of equal old value, so the new table has none either (it matches what generation would
//@ /// have merged), and entries that were one stay one. Intrinsic entries are the same text in both functions.
//@ pub proof fn remap_bijection(old: Env, new: Env, stored: Seq<u8>, refs: Set<Prov>, p: Prov, q: Prov)
//@     requires key_fixes_equality(old, new, stored, refs), refs.contains(p), refs.contains(q),
//@         (is_src(p) && is_src(q)) || (p == Prov::Intrinsic && q == Prov::Intrinsic),
//@     ensures (resolve(old, p, stored) == resolve(old, q, stored)) <==> (resolve(new, p, stored) == resolve(new, q, stored)),
//@ {
//@ }

//@ /// Every provenance recorded for every constant index, in order, and whether two of them clashed.
//@ pub struct Rec {
//@     pub all: Map<int, Seq<Prov>>,
//@     pub conflict: bool,
//@ }

//@ pub open spec fn empty_rec() -> Rec { Rec { all: Map::empty(), conflict: false } }

//@ /// The code of `record_prov`: the first provenance of an index is kept; later ones are compared with it only.
//@ pub open spec fn record(r: Rec, idx: int, p: Prov) -> Rec {
//@     if r.all.dom().contains(idx) {
//@         Rec { all: r.all.insert(idx, r.all[idx].push(p)), conflict: r.conflict || !compat(r.all[idx][0], p) }
//@     } else {
//@         Rec { all: r.all.insert(idx, seq![p]), conflict: r.conflict }
//@     }
//@ }

//@ /// Without a conflict, everything recorded for an index is compatible with the first.
//@ pub open spec fn rec_inv(r: Rec) -> bool {
//@     forall|idx: int| #![trigger r.all.dom().contains(idx)] r.all.dom().contains(idx) ==> r.all[idx].len() > 0
//@ }
//@ pub open spec fn clean(r: Rec) -> bool {
//@     !r.conflict ==> forall|idx: int, k: int| #![trigger r.all[idx][k]] r.all.dom().contains(idx) && 0 <= k < r.all[idx].len() ==> compat(r.all[idx][0], r.all[idx][k])
//@ }

//@ pub proof fn record_keeps_inv(r: Rec, idx: int, p: Prov)
//@     requires rec_inv(r), clean(r),
//@     ensures rec_inv(record(r, idx, p)), clean(record(r, idx, p)),
//@         record(r, idx, p).all.dom().contains(idx), record(r, idx, p).all[idx].last() == p,
//@         forall|i: int| i != idx && r.all.dom().contains(i) ==> record(r, idx, p).all[i] == r.all[i],
//@         r.conflict ==> record(r, idx, p).conflict,
//@         !record(r, idx, p).conflict ==> !r.conflict,
//@ {
//@     let r2 = record(r, idx, p);
//@     if r.all.dom().contains(idx) {
//@         assert(r2.all[idx] == r.all[idx].push(p));
//@         assert forall|i: int, k: int| r2.all.dom().contains(i) && 0 <= k < r2.all[i].len() && !r2.conflict implies compat(r2.all[i][0], r2.all[i][k]) by {
//@             if i == idx {
//@                 if k == r.all[idx].len() as int { assert(compat(r.all[idx][0], p)); }
//@                 else { assert(r2.all[idx][k] == r.all[idx][k]); }
//@             } else {
//@                 assert(r2.all[i] == r.all[i]);
//@             }
//@         }
//@     } else {
//@         assert forall|i: int, k: int| r2.all.dom().contains(i) && 0 <= k < r2.all[i].len() && !r2.conflict implies compat(r2.all[i][0], r2.all[i][k]) by {
//@             if i == idx { assert(k == 0); } else { assert(r2.all[i] == r.all[i]); }
//@         }
//@     }
//@ }

//@ pub proof fn empty_rec_ok()
//@     ensures rec_inv(empty_rec()), clean(empty_rec()),
//@ {
//@ }

//@ /// compat is what makes two provenances of one index resolve alike: with the same first provenance, any two recorded ones are
//@ /// the same or both source texts, so, by the key's guarantee, if they had equal text in the old function (they were merged because
//@ /// their values were equal) they have equal text in the new one.
//@ pub proof fn no_double_remap(old: Env, new: Env, stored: Seq<u8>, refs: Set<Prov>, r: Rec, idx: int, k: int, l: int)
//@     requires rec_inv(r), clean(r), !r.conflict, r.all.dom().contains(idx), 0 <= k < r.all[idx].len(), 0 <= l < r.all[idx].len(),
//@         key_fixes_equality(old, new, stored, refs), refs.contains(r.all[idx][k]), refs.contains(r.all[idx][l]),
//@         resolve(old, r.all[idx][k], stored) == resolve(old, r.all[idx][l], stored),
//@     ensures resolve(new, r.all[idx][k], stored) == resolve(new, r.all[idx][l], stored),
//@         compat(r.all[idx][k], r.all[idx][l]),
//@ {
//@     let p = r.all[idx][k];
//@     let q = r.all[idx][l];
//@     let first = r.all[idx][0];
//@     assert(compat(first, p));
//@     assert(compat(first, q));
//@     if is_src(first) {
//@         assert(is_src(p) && is_src(q));
//@         remap_bijection(old, new, stored, refs, p, q);
//@     } else {
//@         assert(p == first && q == first);
//@     }
//@ }
