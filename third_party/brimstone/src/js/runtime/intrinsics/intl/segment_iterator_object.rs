//! The Segment Iterator Objects (https://tc39.es/ecma402/#sec-segment-iterator-objects), 19.6: what
//! `%Segments.prototype%`'s `@@iterator` (19.5.2.2) makes and what a `for..of` over a Segments object
//! then drives.
//!
//! 19.6.1 gives an iterator three slots: the segmenter, the string and
//! [[IteratedStringNextSegmentCodeUnitIndex]]. The first two are exactly the two slots of the
//! %Segments% object the iterator was made from, and neither can change afterwards, so this object
//! keeps one pointer to that Segments object instead of two copies - which is also how it reaches the
//! boundary table, and so why 19.6.2.1's `next` costs one read rather than a fresh pass over the
//! string.

use super::segments_object::SegmentsObject;
use crate::extend_object;
use crate::runtime::{
    Context, Handle, Realm, Value,
    alloc_error::AllocResult,
    error::type_error,
    eval_result::EvalResult,
    gc::{HeapItem, HeapPtr, HeapVisitor},
    intrinsic_builder::IntrinsicBuilder,
    intrinsics::intrinsics::Intrinsic,
    iterator::create_iter_result_object,
    object_value::ObjectValue,
    ordinary_object::ObjectBuilder,
};
use crate::{intrinsic_methods, runtime_fn};

extend_object! {
    /// Properties of Segment Iterator Objects
    /// (https://tc39.es/ecma402/#sec-segment-iterator-objects), 19.6.
    pub struct SegmentIteratorObject {
        /// [[IteratingSegmenter]] and [[IteratedString]] (19.6.1 steps 2 and 3), together: the
        /// Segments object 19.5.2.2 made this iterator from, which holds both and the boundaries of
        /// the string they name.
        segments: HeapPtr<SegmentsObject>,

        /// [[IteratedStringNextSegmentCodeUnitIndex]] (19.6.1 step 4, 19.6.2.1 step 9): the code unit
        /// index the next segment starts at. It is always an entry of the boundary table, because
        /// every value it is set to is one.
        next_index: u32,
    }
}

impl SegmentIteratorObject {
    /// CreateSegmentIterator (https://tc39.es/ecma402/#sec-createsegmentiterator), 19.6.1, called
    /// only by 19.5.2.2 and so only for a Segments object that has already answered for its own
    /// string once.
    pub fn new(cx: Context, segments: Handle<SegmentsObject>) -> AllocResult<Handle<Self>> {
        // Step 1: `OrdinaryObjectCreate(%IntlSegmentIteratorPrototype%, « the three slots »)`.
        let mut iterator = ObjectBuilder::<Self>::new(cx)
            .intrinsic_proto(Intrinsic::SegmentIteratorPrototype)
            .build()?;

        // Steps 2 to 4: the segmenter and string travel together, and the cursor starts at the first
        // code unit of the string.
        iterator.segments = *segments;
        iterator.next_index = 0;

        Ok(iterator.to_handle())
    }

    /// `? RequireInternalSlot(iterator, [[IteratingSegmenter]])` (19.6.2.1 step 2). Every object of
    /// this type has the slot, since 19.6.1 is the only way to make one.
    pub fn require(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(cx, "Segment iterator method called on incompatible receiver");
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(iterator) => Ok(iterator),
            None => type_error(cx, "Segment iterator method called on incompatible receiver"),
        }
    }

    /// The Segments object this iterator is walking.
    pub fn segments(&self) -> Handle<SegmentsObject> {
        self.segments.to_handle()
    }
}

/// The %IntlSegmentIteratorPrototype% object
/// (https://tc39.es/ecma402/#sec-segment-iterator-objects), 19.6.2.
///
/// Its `[[Prototype]]` is `%Iterator.prototype%`, which has two consequences worth stating because
/// both are tested: the iterator inherits `@@iterator` rather than having one of its own, so
/// `segments[Symbol.iterator]() === segments[Symbol.iterator]()` is false while
/// `iterator[Symbol.iterator]() === iterator` is true; and it gains `drop`, `toArray` and the helpers
/// for free.
pub struct SegmentIteratorPrototype;

impl SegmentIteratorPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::IteratorPrototype)?;

        intrinsic_methods!(cx, builder, {
            next SegmentIteratorPrototype_next (0),
        });

        // 19.6.2.2: @@toStringTag, the String "Segmenter String Iterator", which is what makes
        // `Object.prototype.toString.call(iterator)` report "[object Segmenter String Iterator]".
        builder.to_string_tag_str("Segmenter String Iterator")?;

        builder.build()
    }

    runtime_fn! {
    /// %IntlSegmentIteratorPrototype%.next ( https://tc39.es/ecma402/#sec-segment-iterator-objects ), 19.6.2.1.
    fn next(cx, this_value, _) {
        // Steps 1 and 2.
        let mut iterator = SegmentIteratorObject::require(cx, this_value)?;

        // Steps 3 to 6: [[IteratingSegmenter]], [[IteratedString]] - through the Segments object that
        // holds both - the cursor, and the string's length in code units.
        let segments = iterator.segments();
        let start = iterator.next_index;
        let length = segments.string().len();

        // Step 7: CreateIteratorResultObject(undefined, true). The cursor only ever lands on a
        // boundary and the last boundary is the length, so it reaches that value exactly and the
        // empty string, whose one and only boundary is 0, is done before it starts.
        if start >= length {
            return Ok(create_iter_result_object(cx, cx.undefined(), true)?);
        }

        // Steps 8 and 9: FindBoundary (19.8.1) with direction "after", which is the next entry of the
        // table, and the cursor is moved to it. Both reads happen before step 10 allocates, and the
        // cursor is a plain number, so nothing here is invalidated by a collection.
        let index = segments.segment_index_starting_at(start);
        iterator.next_index = segments.boundary_after(index);

        // Step 10: CreateSegmentDataObject (19.7.1).
        let segment_data = segments.segment_data_object(cx, index)?;

        // Step 11: CreateIteratorResultObject(segmentData, false).
        Ok(create_iter_result_object(cx, segment_data, false)?)
    }}
}

impl HeapItem for SegmentIteratorObject {
    fn byte_size(iterator: HeapPtr<Self>) -> usize {
        iterator.object_byte_size()
    }

    fn visit_pointers(mut iterator: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        iterator.visit_object_pointers(visitor);
        visitor.visit_pointer(&mut iterator.segments);
    }
}
