//! The `Intl.ListFormat.prototype` object
//! (https://tc39.es/ecma402/#sec-properties-of-the-intl-listformat-prototype-object), 14.3:
//! `format`, `formatToParts` and `resolvedOptions`, and the tag that makes an instance report
//! "[object Intl.ListFormat]".
//!
//! The instance, the reading of its options and the formatting itself are
//! [`super::list_format_object`]; this file is only the methods a program calls.

use super::list_format_object::{ListFormatObject, part_object};
use super::number_format_prototype::{report_str, report_value};
use crate::runtime::{
    Context, Handle, Realm, alloc_error::AllocResult, array_object::create_array_from_list,
    error::type_error, intrinsic_builder::IntrinsicBuilder, intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue, ordinary_object::ordinary_object_create,
};
use crate::{intrinsic_methods, runtime_fn};

/// %Intl.ListFormat.prototype% (https://tc39.es/ecma402/#sec-properties-of-the-intl-listformat-prototype-object),
/// 14.3.
pub struct ListFormatPrototype;

impl ListFormatPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // 14.3: an ordinary object that is not an instance, so it carries no
        // [[InitializedListFormat]] and every method below refuses it.
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        intrinsic_methods!(cx, builder, {
            resolved_options ListFormatPrototype_resolved_options (0),
            format ListFormatPrototype_format (1),
            format_to_parts ListFormatPrototype_format_to_parts (1),
        });

        // 14.3.5: @@toStringTag, the String "Intl.ListFormat", non-writable and configurable, which
        // is what `builder.to_string_tag` installs.
        builder.to_string_tag(cx.names.intl_list_format())?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.ListFormat.prototype.resolvedOptions ( https://tc39.es/ecma402/#sec-intl.listformat.prototype.resolvedoptions ), 14.3.2.
    fn resolved_options(cx, this_value, _) {
        // Steps 1 and 2.
        let list_format = ListFormatObject::require_initialized(cx, this_value)?;

        let Some(resolved) = list_format.resolved() else {
            // The question `require_initialized` has already answered.
            return type_error(cx, "Intl.ListFormat method called on an uninitialized instance");
        };

        // [[Locale]] is a raw pointer inside the instance's own data, so it goes into a handle here,
        // before anything else allocates: a collection during step 3 would move the string and leave
        // the copy above pointing at the block it was copied out of.
        let locale = resolved.locale.to_handle(cx);

        // Step 3: `OrdinaryObjectCreate(%Object.prototype%)`.
        let options = ordinary_object_create(cx)?;

        // Steps 4 and 5, in the order Table 25 lists the slots. The three it names are every option
        // 14.1.1 resolves, so nothing here is left off: [[RelevantExtensionKeys]] and
        // [[ResolutionOptionDescriptors]] are both empty, which leaves no numbering system or other
        // key to report, and [[Templates]] is not an option.
        report_value(cx, options, cx.names.locale(), locale)?;
        report_str(cx, options, cx.names.type_(), resolved.kind().as_str())?;
        report_str(cx, options, cx.names.style(), resolved.style().as_str())?;

        // Step 6.
        Ok(options.as_value())
    }}

    runtime_fn! {
    /// Intl.ListFormat.prototype.format ( https://tc39.es/ecma402/#sec-intl.listformat.prototype.format ), 14.3.3.
    fn format(cx, this_value, arguments) {
        // Steps 1 and 2: `RequireInternalSlot(lf, [[InitializedListFormat]])`.
        let list_format = ListFormatObject::require_initialized(cx, this_value)?;

        // Step 3: `? StringListFromIterable(value)`, 14.5.5 - the operation the spec calls
        // CreateListFromArrayLike in 14.3.3 and has since renamed.
        let list = ListFormatObject::string_list_from_iterable(cx, arguments.get(cx, 0))?;

        // Step 4: `? FormatList(lf, list)`, 14.5.3.
        let text = list_format.format(cx, &list)?;

        Ok(cx.alloc_wtf8_string(&text)?.as_value())
    }}

    runtime_fn! {
    /// Intl.ListFormat.prototype.formatToParts ( https://tc39.es/ecma402/#sec-intl.listformat.prototype.formattoparts ), 14.3.4.
    fn format_to_parts(cx, this_value, arguments) {
        // Steps 1 and 2.
        let list_format = ListFormatObject::require_initialized(cx, this_value)?;

        // Step 3: `? StringListFromIterable(value)`, the same reading as 14.3.3 step 3 and so the same
        // TypeError for a place that is not a String.
        let list = ListFormatObject::string_list_from_iterable(cx, arguments.get(cx, 0))?;

        // Step 4: `? FormatListToParts(lf, list)`, 14.5.4.
        let parts = list_format.parts(cx, &list)?;

        // 14.5.4 steps 4.1 and 4.2: one `{ type, value }` record per part, in an array.
        let mut elements = Vec::with_capacity(parts.len());
        for part in &parts {
            elements.push(part_object(cx, part, cx.names.type_(), cx.names.value())?.into());
        }

        Ok(create_array_from_list(cx, &elements)?.into())
    }}
}
