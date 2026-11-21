//! The `Intl.Segmenter.prototype` object
//! (https://tc39.es/ecma402/#sec-properties-of-the-intl-segmenter-prototype-object), 19.3:
//! `resolvedOptions`, `segment` and the tag that makes an instance report "[object Intl.Segmenter]".
//!
//! The instance and the reading of its options are [`super::segmenter_object`]; the object `segment`
//! returns and everything a program then does with it are [`super::segments_object`].

use super::number_format_prototype::{report_str, report_value};
use super::segmenter_object::SegmenterObject;
use super::segments_object::SegmentsObject;
use crate::runtime::{
    Context, Handle, Realm, alloc_error::AllocResult, error::type_error,
    intrinsic_builder::IntrinsicBuilder, intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue, ordinary_object::ordinary_object_create, to_string,
};
use crate::{intrinsic_methods, runtime_fn};

/// %Intl.Segmenter.prototype%
/// (https://tc39.es/ecma402/#sec-properties-of-the-intl-segmenter-prototype-object), 19.3.
pub struct SegmenterPrototype;

impl SegmenterPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // 19.3: an ordinary object that is not an instance, so it carries no
        // [[InitializedSegmenter]] and both methods below refuse it.
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        intrinsic_methods!(cx, builder, {
            resolved_options SegmenterPrototype_resolved_options (0),
            segment SegmenterPrototype_segment (1),
        });

        // 19.3.4: @@toStringTag, the String "Intl.Segmenter", non-writable and configurable, which is
        // what `builder.to_string_tag` installs.
        builder.to_string_tag(cx.names.intl_segmenter())?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.Segmenter.prototype.resolvedOptions ( https://tc39.es/ecma402/#sec-intl.segmenter.prototype.resolvedoptions ), 19.3.2.
    fn resolved_options(cx, this_value, _) {
        // Steps 1 and 2.
        let segmenter = SegmenterObject::require_initialized(cx, this_value)?;

        let Some(resolved) = segmenter.resolved() else {
            // The question `require_initialized` has already answered.
            return type_error(cx, "Intl.Segmenter method called on an uninitialized instance");
        };

        // [[Locale]] is a Value inside the instance's own data, so it goes into a handle here, before
        // anything else allocates: a collection during step 3 would move the string and leave the
        // copy above pointing at the block it was copied out of.
        let locale = resolved.locale.to_handle(cx);

        // Step 3: `OrdinaryObjectCreate(%Object.prototype%)`.
        let options = ordinary_object_create(cx)?;

        // Steps 4 and 5, in the order Table 34 lists the slots. Both of them are always present:
        // [[RelevantExtensionKeys]] is empty (19.2.3), so there is no numbering-system-like key to
        // report, and [[SegmenterGranularity]] is one of three names with a default of "grapheme".
        report_value(cx, options, cx.names.locale(), locale)?;
        report_str(cx, options, cx.names.granularity(), resolved.settings.granularity.as_str())?;

        // Step 6.
        Ok(options.as_value())
    }}

    runtime_fn! {
    /// Intl.Segmenter.prototype.segment ( https://tc39.es/ecma402/#sec-intl.segmenter.prototype.segment ), 19.3.3.
    fn segment(cx, this_value, arguments) {
        // Steps 1 and 2: `RequireInternalSlot(segmenter, [[InitializedSegmenter]])`.
        let segmenter = SegmenterObject::require_initialized(cx, this_value)?;

        // Step 3: ? ToString(string). The result is flattened because the boundaries are read out of
        // the string's code units as one run, which a concat string does not have; flattening it is
        // what every other string operation in this engine that needs the whole text does.
        let string = to_string(cx, arguments.get(cx, 0))?.flatten_to_handle()?;

        // Steps 4 and 5: CreateSegmentsObject (19.5.1), which also answers FindBoundary (19.8.1) for
        // every position in the string at once and keeps the answer on the new object.
        let segments = SegmentsObject::new(cx, segmenter, string)?;

        // Step 6.
        Ok(segments.as_value())
    }}
}
