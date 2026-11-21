//! The `Intl.RelativeTimeFormat.prototype` object
//! (https://tc39.es/ecma402/#sec-properties-of-intl-relativetimeformat-prototype-object), 18.3:
//! `format`, `formatToParts` and `resolvedOptions`, and the tag that makes an instance report
//! "[object Intl.RelativeTimeFormat]".
//!
//! The instance, the reading of its options and the partition itself are
//! [`super::relative_time_format_object`]; this file is only the methods a program calls.

use super::number_format_prototype::{report_str, report_value};
use super::relative_time_format_object::{RelativeTimeFormatObject, RelativeTimePart};
use crate::{
    intrinsic_methods,
    runtime::{
        Context, Handle, PropertyKey, Realm, Value,
        abstract_operations::create_data_property_or_throw, alloc_error::AllocResult,
        array_object::create_array_from_list, error::type_error, eval_result::EvalResult,
        intrinsic_builder::IntrinsicBuilder, intrinsics::intrinsics::Intrinsic,
        object_value::ObjectValue, ordinary_object::ordinary_object_create, to_string,
        type_utilities::to_number,
    },
    runtime_fn,
};

/// %Intl.RelativeTimeFormat.prototype% (https://tc39.es/ecma402/#sec-properties-of-intl-relativetimeformat-prototype-object),
/// 18.3.
pub struct RelativeTimeFormatPrototype;

impl RelativeTimeFormatPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // 18.3: an ordinary object that is not an instance, so it carries no
        // [[InitializedRelativeTimeFormat]] and every method below refuses it.
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        intrinsic_methods!(cx, builder, {
            resolved_options RelativeTimeFormatPrototype_resolved_options (0),
            format RelativeTimeFormatPrototype_format (2),
            format_to_parts RelativeTimeFormatPrototype_format_to_parts (2),
        });

        // 18.3.5: @@toStringTag, the String "Intl.RelativeTimeFormat", non-writable and
        // configurable, which is what `builder.to_string_tag` installs.
        builder.to_string_tag(cx.names.intl_relative_time_format())?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.RelativeTimeFormat.prototype.resolvedOptions ( https://tc39.es/ecma402/#sec-intl-relativetimeformat-prototype-resolvedoptions ), 18.3.2.
    fn resolved_options(cx, this_value, _) {
        // Steps 1 and 2.
        let relative_time_format = RelativeTimeFormatObject::require_initialized(cx, this_value)?;

        let Some(resolved) = relative_time_format.resolved() else {
            // The question `require_initialized` has already answered.
            return type_error(
                cx,
                "Intl.RelativeTimeFormat method called on an uninitialized instance",
            );
        };

        // [[Locale]] is a raw pointer inside the instance's own data, so it goes into a handle
        // here, before anything else allocates: a collection during step 3 would move the string
        // and leave the copy above pointing at the block it was copied out of.
        let locale = resolved.locale.to_handle(cx);

        // Step 3: `OrdinaryObjectCreate(%Object.prototype%)`.
        let options = ordinary_object_create(cx)?;

        // Steps 4 and 5, in the order Table 33 lists the slots. [[NumberingSystem]] is the
        // constant the single "nu" value this build supports forces, which is why it is reported
        // without having been stored.
        report_value(cx, options, cx.names.locale(), locale)?;
        report_str(cx, options, cx.names.style(), resolved.style().as_str())?;
        report_str(cx, options, cx.names.numeric(), resolved.numeric().as_str())?;
        report_str(cx, options, cx.names.numbering_system(), "latn")?;

        // Step 5.
        Ok(options.as_value())
    }}

    runtime_fn! {
    /// Intl.RelativeTimeFormat.prototype.format ( https://tc39.es/ecma402/#sec-intl-relativetimeformat-prototype-format ), 18.3.3.
    fn format(cx, this_value, arguments) {
        // Steps 1 and 2: `RequireInternalSlot(rtf, [[InitializedRelativeTimeFormat]])`.
        let relative_time_format = RelativeTimeFormatObject::require_initialized(cx, this_value)?;

        // Step 3: `value = ? ToNumber(value)`.
        let value = to_number(cx, arguments.get(cx, 0))?.as_number();

        // Step 4: `unit = ? ToString(unit)`. A unit is matched by name, so the lossy conversion
        // only turns a request that no name of 18.5.1 would have matched anyway into one that is
        // refused.
        let unit = to_string_argument(cx, arguments.get(cx, 1))?;

        // Step 5: ? FormatRelativeTime(rtf, value, unit), 18.5.4 - which is 18.5.2's parts with
        // their text concatenated.
        let parts = relative_time_format.partition(cx, value, &unit)?;

        let mut text = String::new();
        for part in &parts {
            text.push_str(&part.value);
        }

        Ok(cx.alloc_string(&text)?.as_value())
    }}

    runtime_fn! {
    /// Intl.RelativeTimeFormat.prototype.formatToParts ( https://tc39.es/ecma402/#sec-intl-relativetimeformat-prototype-formattoparts ), 18.3.4.
    fn format_to_parts(cx, this_value, arguments) {
        // Steps 1 and 2.
        let relative_time_format = RelativeTimeFormatObject::require_initialized(cx, this_value)?;

        // Steps 3 and 4: the same reads as 18.3.3, and so the same RangeError for a value with no
        // finite meaning and the same one for a unit that is no singular relative time unit.
        let value = to_number(cx, arguments.get(cx, 0))?.as_number();
        let unit = to_string_argument(cx, arguments.get(cx, 1))?;

        // Step 5: ? FormatRelativeTimeToParts(rtf, value, unit), 18.5.5 - which is 18.5.2's parts
        // made into objects.
        let parts = relative_time_format.partition(cx, value, &unit)?;

        // 18.5.5 steps 4.1 to 4.5: one record per part, in an array, with `unit` only where the
        // record carries one.
        let mut elements = Vec::with_capacity(parts.len());
        for part in &parts {
            elements.push(
                part_object(cx, part, cx.names.type_(), cx.names.value(), cx.names.unit())?.into(),
            );
        }

        Ok(create_array_from_list(cx, &elements)?.into())
    }}
}

/// 18.3.3 step 4 and 18.3.4 step 4, as far as a unit has to be read as text: a unit name is
/// matched against the eight names of 18.5.1, all of them ASCII, so the lossy conversion only
/// turns requests that were already not names into one that is refused.
fn to_string_argument(cx: Context, value: Handle<Value>) -> EvalResult<String> {
    let text = to_string(cx, value)?.to_wtf8_string()?;

    Ok(String::from_utf8_lossy(text.as_bytes()).into_owned())
}

/// 18.5.5 steps 4.1 to 4.4: one `{ type, value, unit? }` object per part.
fn part_object(
    mut cx: Context,
    part: &RelativeTimePart,
    kind: Handle<PropertyKey>,
    value: Handle<PropertyKey>,
    unit: Handle<PropertyKey>,
) -> EvalResult<Handle<ObjectValue>> {
    let record = ordinary_object_create(cx)?;

    create_data_property_or_throw(
        cx,
        record,
        kind,
        cx.alloc_string(part.kind.as_str())?.as_value(),
    )?;
    create_data_property_or_throw(cx, record, value, cx.alloc_string(&part.value)?.as_value())?;

    if let Some(unit_name) = part.unit {
        create_data_property_or_throw(
            cx,
            record,
            unit,
            cx.alloc_string(unit_name.as_str())?.as_value(),
        )?;
    }

    Ok(record)
}
