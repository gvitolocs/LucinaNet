// proof text: ensures spells the branch; two mutants search it; sites: src/js/runtime/gc/accounting.rs:157
#[allow(clippy::implicit_saturating_sub)]
// proof text: `p = p * 2;` is in three mutant search texts; sites: src/js/runtime/gc/accounting.rs:190
#[allow(clippy::assign_op_pattern)]
pub mod accounting;
mod garbage_collector;
mod handle;
mod heap;
mod heap_item;
mod heap_serializer;
mod heap_trait_object;
mod heap_visitor;
mod pointer;
// proof text: one mutant; sites: src/js/runtime/gc/sizing.rs:155
#[allow(clippy::implicit_saturating_sub)]
// proof text: assign op in proved code; CI rule records no separate reason; sites: src/js/runtime/gc/sizing.rs:171
#[allow(clippy::assign_op_pattern)]
mod sizing;
pub(crate) mod verify;
mod weak_rules;

pub use garbage_collector::{GarbageCollector, GcType};
pub use handle::{
    Escapable, Handle, HandleContents, HandleScope, HandleScopeGuard, ToHandleContents,
};
pub use heap::{HEAP_ALIGNMENT, Heap, HeapInfo};
pub use heap_item::{
    AnyHeapItem, HeapItem, HeapItemKind, HeapUnaligned, IsHeapItem, WithHeapItemKind,
};
#[allow(unused)]
pub use heap_serializer::{HeapRootsDeserializer, HeapSerializer};
pub use heap_visitor::HeapVisitor;
pub use pointer::HeapPtr;
