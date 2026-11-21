//! The %Segments% object (https://tc39.es/ecma402/#sec-segments-objects), 19.5: the object 19.3.3's
//! `segment` hands back, its `containing` method and its `@@iterator`, and the segment data object of
//! 19.7.1 that all three answers are made of.
//!
//! The clause is 19, not 18: in the draft this build follows, RelativeTimeFormat took 18 and
//! Segmenter moved to 19.
//!
//! What this object adds over 19.5.1's two slots is the point of the whole design. The spec answers
//! a question about a segment one position at a time, through FindBoundary (19.8.1), and FindBoundary
//! is written as a loop that starts from the beginning of the string - UTS #29 boundaries depend on
//! the text before them, so ICU4X 2.3 offers no way to resume a break iterator from a position, only
//! to start one. Asking it per `next()` is therefore quadratic over an iteration, which is what Boa
//! does and this build does not: 19.3.3 asks for every boundary of the string once, and stores the
//! answer here as a table. `containing` then binary-searches it and 19.6.2.1's `next` reads one entry
//! of it, so a full iteration is linear.
//!
//! The table is a `U32Array` and a `ByteArray`, the engine's GC-allocated plain-data blocks, because
//! a heap item may not own anything (`gc/heap_item.rs` refuses one that needs a destructor) and a
//! `Vec` field would be memory the collector cannot see or free.

use super::segment_iterator_object::SegmentIteratorObject;
use super::segmenter_object::{SegmenterGranularity, SegmenterObject};
use crate::extend_object;
use crate::runtime::intrinsics::rust_runtime::RuntimeFunction;
use crate::runtime::{
    Context, Handle, Realm, Value,
    abstract_operations::create_data_property_or_throw,
    alloc_error::AllocResult,
    collections::{ArrayInstance, array::ByteArray, array::U32Array},
    error::type_error,
    eval_result::EvalResult,
    gc::{HeapItem, HeapPtr, HeapVisitor},
    intrinsic_builder::IntrinsicBuilder,
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::{ObjectBuilder, ordinary_object_create},
    string_value::FlatString,
    type_utilities::to_integer_or_infinity,
};
use crate::{intrinsic_methods, runtime_fn};

extend_object! {
    /// Properties of %Segments% Objects
    /// (https://tc39.es/ecma402/#sec-segments-objects), 19.5.
    ///
    /// [[SegmentsSegmenter]] and [[SegmentsString]] are 19.5.1's two slots; the two arrays are the
    /// boundaries they imply, computed once at creation (see the module note).
    pub struct SegmentsObject {
        /// [[SegmentsSegmenter]] (19.5.1 step 3), which 19.5.2.1 step 3 and 19.5.2.2 step 3 read and
        /// whose [[SegmenterGranularity]] decides whether a segment data object has an `isWordLike`.
        segmenter: HeapPtr<SegmenterObject>,

        /// [[SegmentsString]] (19.5.1 step 4): the string the segments are pieces of, which 19.7.1
        /// step 7 reports back as each segment's `input`.
        string: HeapPtr<FlatString>,

        /// [[SegmenterGranularity]], copied out of the segmenter so that reading a segment needs no
        /// hop through an object that cannot be dropped. The slot is immutable after 19.1.1, so the
        /// copy and the original can never disagree.
        granularity: SegmenterGranularity,

        /// Every boundary of [[SegmentsString]] in code units, increasing, starting at 0 and ending
        /// at the string's length: FindBoundary answered for all of them at once. Segment `i` is the
        /// half-open range `boundaries[i]..boundaries[i + 1]`, so there is one more entry than there
        /// are segments.
        boundaries: HeapPtr<U32Array>,

        /// Each segment's 19.7.1 step 10 "word-like" answer, one byte each, present only for word
        /// granularity: for the other two the property must be absent from the segment data object
        /// rather than present and `false`.
        word_like: Option<HeapPtr<ByteArray>>,
    }
}

impl SegmentsObject {
    /// CreateSegmentsObject (https://tc39.es/ecma402/#sec-createsegmentsobject), 19.5.1, with the
    /// boundary computation of 19.3.3's FindBoundary questions folded in.
    pub fn new(
        cx: Context,
        segmenter: Handle<SegmenterObject>,
        string: Handle<FlatString>,
    ) -> AllocResult<Handle<Self>> {
        // Steps 1 and 2 are OrdinaryObjectCreate(%Segments.prototype%), which the builder does.
        let granularity = segmenter
            .granularity()
            .expect("19.3.3 step 2 required an initialized segmenter");

        // 19.8.1 for every position, once. The code units are copied out of the string first: the
        // segmentation and the allocations below must not hold a borrow of a GC block.
        let units: Vec<u16> = (*string).iter_code_units().collect();
        let segmentation = segmenter.segmenter(cx).segment(&units);

        let boundaries = U32Array::new_from_slice(cx, &segmentation.boundaries)?.to_handle();

        // The flags are only ever read for word granularity, and are only computed for it, which is
        // what makes their absence the answer to 19.7.1 step 9.
        let word_like = if granularity == SegmenterGranularity::Word {
            let flags: Vec<u8> = segmentation
                .word_like
                .iter()
                .map(|&is_word_like| u8::from(is_word_like))
                .collect();
            Some(ByteArray::new_from_slice(cx, &flags)?.to_handle())
        } else {
            None
        };

        let mut segments = ObjectBuilder::<Self>::new(cx)
            .intrinsic_proto(Intrinsic::SegmentsPrototype)
            .build()?;

        // Step 3 and step 4.
        segments.segmenter = *segmenter;
        segments.string = *string;
        segments.granularity = granularity;
        segments.boundaries = *boundaries;
        segments.word_like = word_like.map(|flags| *flags);

        Ok(segments.to_handle())
    }

    /// `? RequireInternalSlot(segments, [[SegmentsSegmenter]])` (19.5.2.1 step 2, 19.5.2.2 step 2).
    ///
    /// An object of this type always has the slot, because there is no way to make one without it:
    /// 19.5.1 sets both slots in the one operation that creates the object.
    pub fn require(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(cx, "Segments method called on incompatible receiver");
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(segments) => Ok(segments),
            None => type_error(cx, "Segments method called on incompatible receiver"),
        }
    }

    /// [[SegmentsString]]: the string whose pieces these are.
    pub fn string(&self) -> Handle<FlatString> {
        self.string.to_handle()
    }

    /// [[SegmentsSegmenter]].
    pub fn segmenter(&self) -> Handle<SegmenterObject> {
        self.segmenter.to_handle()
    }
}

impl Handle<SegmentsObject> {
    /// The boundary at table position `i`.
    fn boundary(self, i: usize) -> u32 {
        self.boundaries.as_slice()[i]
    }

    /// The table position of the segment that starts at the code unit index `start`, which is also
    /// where that segment's `isWordLike` flag is. Every start the callers pass is a boundary, because
    /// every one of them got it out of this same table.
    pub fn segment_index_starting_at(self, start: u32) -> usize {
        let boundaries = self.boundaries.as_slice();
        let index = boundaries.partition_point(|&boundary| boundary <= start) - 1;

        debug_assert_eq!(boundaries[index], start, "segments start at boundaries");

        index
    }

    /// CreateSegmentDataObject (https://tc39.es/ecma402/#sec-createsegmentdataobject), 19.7.1, for
    /// the segment at the given table position: 19.7.1's `segmenter` and `string` arguments are this
    /// object's slots, and its `startIndex` and `endIndex` are the two boundaries either side of it.
    pub fn segment_data_object(self, cx: Context, index: usize) -> EvalResult<Handle<Value>> {
        // Steps 1 and 2: the two indices, read out of the table before anything allocates.
        let start = self.boundary(index);
        let end = self.boundary(index + 1);

        // Step 3: `OrdinaryObjectCreate(%Object.prototype%)`.
        let segment_object = ordinary_object_create(cx)?;

        // Step 4. The string is re-read after that allocation rather than cached across it, as
        // everywhere else here: a collection moves the block a pointer was copied out of.
        let segment_string = self.string().substring(cx, start, end)?;
        create_data_property_or_throw(
            cx,
            segment_object,
            cx.names.segment(),
            segment_string.as_value(),
        )?;

        // Steps 5 and 6: the code unit index, which is what 6.2.9 counts.
        create_data_property_or_throw(cx, segment_object, cx.names.index(), cx.number(start))?;

        // Steps 7 and 8.
        create_data_property_or_throw(
            cx,
            segment_object,
            cx.names.input(),
            self.string().as_value(),
        )?;

        // Steps 9 and 10: grapheme and sentence segmentation has no word-likeness to answer for, and
        // the property is left absent rather than reported `false`.
        if self.granularity != SegmenterGranularity::Word {
            return Ok(segment_object.as_value());
        }

        // Steps 11 and 12.
        let is_word_like = self
            .word_like
            .is_some_and(|flags| flags.as_slice()[index] != 0);
        create_data_property_or_throw(
            cx,
            segment_object,
            cx.names.is_word_like(),
            cx.bool(is_word_like),
        )?;

        // Step 13.
        Ok(segment_object.as_value())
    }

    /// FindBoundary (https://tc39.es/ecma402/#sec-findboundary), 19.8.1, as the two questions 19.5.2.1
    /// steps 8 and 9 ask at once: the last boundary at or before `position`, and the first one after
    /// it. Both exist because the caller has checked that the position is inside the string, which is
    /// also what the table's 0 and its final entry guarantee. The position returned is the segment
    /// whose start is the "before" answer and whose end is the "after" one.
    pub fn segment_containing(self, position: u32) -> usize {
        self.boundaries
            .as_slice()
            .partition_point(|&boundary| boundary <= position)
            - 1
    }

    /// [[IteratedStringNextSegmentCodeUnitIndex]]'s partner in 19.6.2.1: the boundary that ends the
    /// segment starting at `start`, which step 8's FindBoundary with direction "after" asks for.
    pub fn boundary_after(self, index: usize) -> u32 {
        self.boundary(index + 1)
    }
}

/// The %Segments.prototype% object
/// (https://tc39.es/ecma402/#sec-segments-objects), 19.5.2.
///
/// The spec gives it `containing`, `@@iterator` and nothing else - no constructor (19.5.1's objects
/// are not made by a program) and, unlike every other prototype in this build, no `@@toStringTag`:
/// `Object.prototype.toString.call(segments)` reports "[object Object]" because %Segments.prototype%
/// has no tag of its own to report.
pub struct SegmentsPrototype;

impl SegmentsPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        intrinsic_methods!(cx, builder, {
            containing SegmentsPrototype_containing (1),
        });

        // 19.5.2.2: @@iterator, whose absence would make a Segments object non-iterable, and whose
        // presence is what `[...segmenter.segment(text)]` runs through.
        builder.method(cx.symbols.iterator(), RuntimeFunction::SegmentsPrototype_iterator, 0)?;

        builder.build()
    }

    runtime_fn! {
    /// %Segments.prototype%.containing ( https://tc39.es/ecma402/#sec-segments-objects ), 19.5.2.1.
    fn containing(cx, this_value, arguments) {
        // Steps 1 and 2.
        let segments = SegmentsObject::require(cx, this_value)?;

        // Steps 3 to 5: [[SegmentsSegmenter]], [[SegmentsString]] and its length in code units. The
        // segmenter is not read again below, because the granularity that decides `isWordLike` is
        // copied onto this object and the string's length is its own.
        let length = segments.string().len();

        // Step 6.
        let n = to_integer_or_infinity(cx, arguments.get(cx, 0))?;

        // Step 7: outside the string - including at its length, and for NaN, which
        // ToIntegerOrInfinity turns into 0 and which therefore answers as index 0 - there is no
        // segment to name, and the answer is `undefined` rather than an empty segment.
        if n < 0.0 || n >= f64::from(length) {
            return Ok(cx.undefined());
        }

        // Steps 8 to 10: the segment the two FindBoundary questions enclose, which is the segment at
        // the table position that search found.
        let index = segments.segment_containing(n as u32);
        segments.segment_data_object(cx, index)
    }}

    runtime_fn! {
    /// %Segments.prototype% [ @@iterator ] ( https://tc39.es/ecma402/#sec-segments-objects ), 19.5.2.2.
    fn iterator(cx, this_value, _) {
        // Steps 1 and 2.
        let segments = SegmentsObject::require(cx, this_value)?;

        // Steps 3 to 5: CreateSegmentIterator (19.6.1) over the same segmenter and string, with its
        // cursor at the start. The iterator reads the boundaries from this object rather than
        // re-deriving them, which is what keeps `next` O(1).
        let iterator = SegmentIteratorObject::new(cx, segments)?;

        // Step 6.
        Ok(iterator.as_value())
    }}
}

impl HeapItem for SegmentsObject {
    fn byte_size(segments: HeapPtr<Self>) -> usize {
        segments.object_byte_size()
    }

    fn visit_pointers(mut segments: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        segments.visit_object_pointers(visitor);
        visitor.visit_pointer(&mut segments.segmenter);
        visitor.visit_pointer(&mut segments.string);
        visitor.visit_pointer(&mut segments.boundaries);
        visitor.visit_pointer_opt(&mut segments.word_like);
    }
}
