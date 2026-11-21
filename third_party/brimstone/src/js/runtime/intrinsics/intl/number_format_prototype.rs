//! The `Intl.NumberFormat.prototype` object (https://tc39.es/ecma402/#sec-properties-of-the-intl-numberformat-prototype-object),
//! 16.3: `resolvedOptions` and `formatToParts`, the `format` getter, and the tag that makes an
//! instance report "[object Intl.NumberFormat]".
//!
//! The instance itself, the reading of its options and the rendering are
//! [`super::number_format_object`]; this file is only the methods a program calls and the function
//! `format` hands back.

use super::number_format_core::{Notation, NumberPart};
use super::number_format_object::NumberFormatObject;
use crate::{
    intrinsic_methods,
    runtime::{
        Context, Handle, PropertyKey, Realm, Value,
        abstract_operations::create_data_property_or_throw,
        alloc_error::AllocResult,
        array_object::create_array_from_list,
        builtin_function::BuiltinFunction,
        error::type_error,
        eval_result::EvalResult,
        intrinsic_builder::IntrinsicBuilder,
        intrinsics::{intrinsics::Intrinsic, rust_runtime::RuntimeFunction},
        object_value::ObjectValue,
        ordinary_object::ordinary_object_create,
    },
    runtime_fn,
};

/// %Intl.NumberFormat.prototype% (https://tc39.es/ecma402/#sec-properties-of-the-intl-numberformat-prototype-object),
/// 16.3.
pub struct NumberFormatPrototype;

impl NumberFormatPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        // 16.3.3. The getter is installed before the methods because 16.3 lists them in that order
        // and `Object.getOwnPropertyNames` reports the order they were defined in.
        builder.getter(cx.names.format(), RuntimeFunction::NumberFormatPrototype_get_format)?;

        intrinsic_methods!(cx, builder, {
            resolved_options NumberFormatPrototype_resolved_options (0),
            format_to_parts NumberFormatPrototype_format_to_parts (1),
        });

        // 16.3.7: @@toStringTag, the String "Intl.NumberFormat", non-writable and configurable,
        // which is what `builder.to_string_tag` installs.
        builder.to_string_tag(cx.names.intl_number_format())?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.NumberFormat.prototype.resolvedOptions ( https://tc39.es/ecma402/#sec-intl.numberformat.prototype.resolvedoptions ), 16.3.2.
    fn resolved_options(cx, this_value, _) {
        // Steps 1 to 3: UnwrapNumberFormat (16.5.9), then the check of [[InitializedNumberFormat]]
        // on what it gave back.
        let number_format = NumberFormatObject::unwrap_initialized(cx, this_value)?;

        let Some(resolved) = number_format.resolved() else {
            // The question `unwrap_initialized` has already answered.
            return type_error(cx, "Intl.NumberFormat method called on an uninitialized instance");
        };

        // [[Locale]] is a raw pointer inside the instance's own data, so it goes into a handle here,
        // before anything else allocates: a collection during step 4 would move the string and leave
        // the copy above pointing at the block it was copied out of.
        let locale = resolved.locale.to_handle(cx);

        // Step 4: `OrdinaryObjectCreate(%Object.prototype%)`.
        let options = ordinary_object_create(cx)?;

        // Step 5, in the order Table 28 lists the slots. A slot this build does not have - a
        // currency, a unit, `compactDisplay`, and either digit pair the instance did not resolve - is
        // left off rather than reported as `undefined`, which is what "if the slot is not undefined"
        // means.
        report_value(cx, options, cx.names.locale(), locale)?;
        report_str(cx, options, cx.names.numbering_system(), "latn")?;
        report_str(cx, options, cx.names.style(), resolved.slots.style.as_str())?;
        // [[Currency]], [[CurrencyDisplay]] and [[CurrencySign]] are the three slots 16.1.3 step 12
        // writes only for style "currency", so an instance of another style has none of them to
        // report even though step 6 and step 7 read and checked the two options for it.
        if let Some(currency) = resolved.slots.currency {
            report_str(cx, options, cx.names.currency(), currency.as_str())?;
            report_str(
                cx,
                options,
                cx.names.currency_display(),
                resolved.slots.currency_display.as_str(),
            )?;
            report_str(
                cx,
                options,
                cx.names.currency_sign(),
                resolved.slots.currency_sign.as_str(),
            )?;
        }
        report_number(
            cx,
            options,
            cx.names.minimum_integer_digits(),
            resolved.slots.minimum_integer_digits,
        )?;
        if resolved.slots.reports_fraction_digits {
            report_number(
                cx,
                options,
                cx.names.minimum_fraction_digits(),
                resolved.slots.minimum_fraction_digits,
            )?;
            report_number(
                cx,
                options,
                cx.names.maximum_fraction_digits(),
                resolved.slots.maximum_fraction_digits,
            )?;
        }
        if resolved.slots.reports_significant_digits {
            report_number(
                cx,
                options,
                cx.names.minimum_significant_digits(),
                resolved.slots.minimum_significant_digits,
            )?;
            report_number(
                cx,
                options,
                cx.names.maximum_significant_digits(),
                resolved.slots.maximum_significant_digits,
            )?;
        }
        // 16.1.1 step 24 turned the Boolean `true` into "always" and left `false` the Boolean, which
        // is what [`super::number_format_core::UseGrouping::resolved_name`] reports.
        match resolved.slots.use_grouping.resolved_name() {
            Some(name) => report_str(cx, options, cx.names.use_grouping(), name)?,
            None => report_value(cx, options, cx.names.use_grouping(), cx.bool(false))?,
        }
        // [[Notation]] and [[NumberingSystem]] are not slots an instance stores: 16.1.1 step 11 and
        // 9.2.7 step 12 allow exactly one value of each here, so both are the constant above.
        report_str(cx, options, cx.names.notation(), resolved.slots.notation.as_str())?;

        // Table 28 reports `compactDisplay` for the notation that reads it and for no other, which
        // is 16.1.1 step 19.1 writing the slot only there.
        if resolved.slots.notation == Notation::Compact {
            report_str(
                cx,
                options,
                cx.names.compact_display(),
                resolved.slots.compact_display.as_str(),
            )?;
        }

        report_str(cx, options, cx.names.sign_display(), resolved.slots.sign_display.as_str())?;
        report_number(
            cx,
            options,
            cx.names.rounding_increment(),
            resolved.slots.rounding_increment.as_u32(),
        )?;
        report_str(cx, options, cx.names.rounding_mode(), resolved.slots.rounding_mode.as_str())?;
        report_str(
            cx,
            options,
            cx.names.rounding_priority(),
            resolved.slots.rounding_priority.as_str(),
        )?;
        report_str(
            cx,
            options,
            cx.names.trailing_zero_display(),
            resolved.slots.trailing_zero_display.as_str(),
        )?;

        // Step 6.
        Ok(options.as_value())
    }}

    runtime_fn! {
    /// Intl.NumberFormat.prototype.formatToParts ( https://tc39.es/ecma402/#sec-intl.numberformat.prototype.formattoparts ), 16.3.6.
    fn format_to_parts(cx, this_value, arguments) {
        // Step 1: `RequireInternalSlot(numberFormat, [[InitializedNumberFormat]])`. 16.3.6 is the one
        // method of 16.3 that does not start with UnwrapNumberFormat, so an object that only carries
        // an instance under %Intl%.[[FallbackSymbol]] - the `this` of the legacy call mode - is
        // refused here rather than formatted through the instance hidden on it.
        let number_format = NumberFormatObject::require_initialized(cx, this_value)?;

        // Step 2 is the question `require_initialized` just asked, and step 3 is
        // `? ToIntlMathematicalValue(value)`, 16.5.15.
        let x = NumberFormatObject::to_intl_mathematical_value(cx, arguments.get(cx, 0))?;

        // Step 4: `FormatNumericToParts(numberFormat, x)`, 16.5.6.
        let parts = number_format.format_to_parts(cx, &x)?;

        // 16.5.6 steps 2 to 4.2: one `{ type, value }` record per part, in an array.
        let mut elements = Vec::with_capacity(parts.len());
        for part in &parts {
            elements.push(number_part_to_object(cx, part)?.into());
        }

        Ok(create_array_from_list(cx, &elements)?.into())
    }}

    runtime_fn! {
    /// get Intl.NumberFormat.prototype.format ( https://tc39.es/ecma402/#sec-intl.numberformat.prototype.format ), 16.3.3.
    fn get_format(cx, this_value, _) {
        // Steps 1 to 3: UnwrapNumberFormat and the check of [[InitializedNumberFormat]].
        let mut number_format = NumberFormatObject::unwrap_initialized(cx, this_value)?;

        // Steps 4 and 6, for an instance that has been asked before: [[BoundFormat]] is the one
        // function the getter made, which is what makes `nf.format === nf.format` hold.
        let bound_format = number_format.bound_format();
        if !bound_format.is_undefined() {
            return Ok(bound_format.to_handle(cx));
        }

        // Step 4.1: the function of the closure. The builtin functions of this engine carry no
        // closure environment, so "captures nf" is built the way `Proxy.revocable`'s revoker and a
        // bound function are built: the instance hangs off the function under a private symbol and
        // the function reads it back. That is also why the function below ignores its `this` value,
        // which is what lets `[1, 2].map(nf.format)` format in `nf`'s locale.
        let realm = cx.current_realm();
        let mut format_function = BuiltinFunction::create(
            cx,
            RuntimeFunction::number_format_prototype_format,
            1,
            cx.names.empty_string(),
            realm,
            None,
        )?;
        format_function.private_element_set(
            cx,
            cx.symbols.bound_number_format().cast(),
            number_format.into(),
        )?;

        // Step 4.2: `Set(numberFormat.[[BoundFormat]], boundFormat)`, and step 6 returns it.
        number_format.set_bound_format(*format_function.as_value());

        Ok(format_function.as_value())
    }}
}

runtime_fn! {
/// 16.3.3 step 4.1's closure, which is what `Intl.NumberFormat.prototype.format` hands out. Its
/// `this` value is ignored on purpose: the instance is the one the getter captured.
fn format(cx, _, arguments) {
    // The capture, read back the way a revoker reads the proxy it revokes.
    let captured = cx
        .current_function()
        .private_element_find(cx, cx.symbols.bound_number_format().cast());

    let Some(captured) = captured else {
        return type_error(cx, "Intl.NumberFormat format function without its captured instance");
    };

    let number_format = captured.value().cast::<NumberFormatObject>();

    // Step 4.1.1: `? ToIntlMathematicalValue(value)`.
    let x = NumberFormatObject::to_intl_mathematical_value(cx, arguments.get(cx, 0))?;

    // Step 4.1.2: `Return FormatNumeric(numberFormat, x)`, 16.5.5.
    let text = number_format.format(cx, &x)?;

    Ok(cx.alloc_string(&text)?.as_value())
}}

/// The `{ type, value }` record of 16.5.6 step 4.2.
fn number_part_to_object(mut cx: Context, part: &NumberPart) -> EvalResult<Handle<ObjectValue>> {
    let record = ordinary_object_create(cx)?;

    create_data_property_or_throw(
        cx,
        record,
        cx.names.type_(),
        cx.alloc_string(part.kind.name())?.as_value(),
    )?;
    create_data_property_or_throw(
        cx,
        record,
        cx.names.value(),
        cx.alloc_string(&part.value)?.as_value(),
    )?;

    Ok(record)
}

/// 16.3.2 step 5, for a slot whose value is a String.
pub(crate) fn report_str(
    mut cx: Context,
    options: Handle<ObjectValue>,
    name: Handle<PropertyKey>,
    value: &str,
) -> EvalResult<()> {
    let value = cx.alloc_string(value)?;

    create_data_property_or_throw(cx, options, name, value.as_value())
}

/// 16.3.2 step 5, for a slot whose value is a Number.
pub(crate) fn report_number(
    cx: Context,
    options: Handle<ObjectValue>,
    name: Handle<PropertyKey>,
    value: impl Into<f64>,
) -> EvalResult<()> {
    create_data_property_or_throw(cx, options, name, Value::number(value.into()).to_handle(cx))
}

/// 16.3.2 step 5, for a slot whose value is neither, which here is only the `false` of
/// [[UseGrouping]].
pub(crate) fn report_value(
    cx: Context,
    options: Handle<ObjectValue>,
    name: Handle<PropertyKey>,
    value: Handle<Value>,
) -> EvalResult<()> {
    create_data_property_or_throw(cx, options, name, value)
}
