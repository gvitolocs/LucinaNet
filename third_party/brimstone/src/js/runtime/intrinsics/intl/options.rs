//! The option-reading abstract operations of ECMA-402 section 9.2, which every `Intl` constructor
//! starts from.

use crate::runtime::{
    Context, EvalResult, Handle, PropertyKey, Value,
    error::{range_error, type_error},
    get,
    object_value::ObjectValue,
    ordinary_object::ordinary_object_create_without_proto,
    to_string,
    type_utilities::{to_boolean, to_number, to_object},
};

/// The `type` argument of GetOption (9.2.11 steps 3 and 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionType {
    Boolean,
    String,
}

/// CoerceOptionsToObject (https://tc39.es/ecma402/#sec-coerceoptionstoobject), 9.2.10.
pub fn coerce_options_to_object(
    cx: Context,
    options: Handle<Value>,
) -> EvalResult<Handle<ObjectValue>> {
    // Step 1: an absent `options` is read as an object with no prototype, so that every option read
    // below looks the same way.
    if options.is_undefined() {
        return Ok(ordinary_object_create_without_proto(cx)?.to_handle());
    }

    // Step 2: ? ToObject(options). This coercion is exactly why the spec's own note discourages
    // this operation in favour of GetOptionsObject: a number or a boolean quietly becomes a wrapper
    // object instead of being reported.
    to_object(cx, options)
}

/// GetOptionsObject (https://tc39.es/ecma402/#sec-getoptionsobject), the stricter sibling that new
/// functionality is meant to use instead of `CoerceOptionsToObject`.
pub fn get_options_object(cx: Context, options: Handle<Value>) -> EvalResult<Handle<ObjectValue>> {
    // Step 1.
    if options.is_undefined() {
        return Ok(ordinary_object_create_without_proto(cx)?.to_handle());
    }

    // Step 2.
    if options.is_object() {
        return Ok(options.as_object());
    }

    // Step 3.
    type_error(cx, "Intl options argument must be an object or undefined")
}

/// GetOption (https://tc39.es/ecma402/#sec-getoption), 9.2.11. `values` is the spec's list of
/// allowed values, empty for "no restriction", and `default` is `None` for the spec's `required`.
pub fn get_option(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    option_type: OptionType,
    values: &[&str],
    default: Option<Handle<Value>>,
) -> EvalResult<Handle<Value>> {
    // Step 1: Let value be ? Get(options, propertyKey).
    let mut value = get(cx, options, property)?;

    if value.is_undefined() {
        // Step 2.1: no `default` means the option is required.
        let Some(default) = default else {
            return type_error(cx, "Intl option is missing and has no default");
        };

        // Step 2.2.
        return Ok(default);
    }

    match option_type {
        // Step 3.
        OptionType::Boolean => value = cx.bool(to_boolean(*value)),
        // Step 4: the assert here is that the type is string.
        OptionType::String => value = to_string(cx, value)?.into(),
    }

    if !values.is_empty() {
        // Step 5: the converted value has to appear in the allowed list. Steps 3 and 4 guarantee a
        // boolean or a string, and a list is only ever passed for the latter.
        let string = value.as_string().flatten()?;
        if !values.iter().any(|allowed| string.eq_str(allowed)) {
            let allowed = values.join(", ");
            return range_error(cx, &format!("Intl option value must be one of: {allowed}"));
        }
    }

    // Step 6.
    Ok(value)
}

/// GetOption (9.2.11) for a `string` option whose allowed values are known here, answered with one
/// of the entries of `values` instead of a copy of the string the program wrote: step 5 compares the
/// converted value against that list, and step 2 answers with `default`, so one of the entries is
/// what the operation always returns.
pub fn get_name_option<'a>(
    mut cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    values: &'a [&'a str],
    default: &'a str,
) -> EvalResult<&'a str> {
    let value = get_option(
        cx,
        options,
        property,
        OptionType::String,
        values,
        // The default is answered with a copy rather than `alloc_static_string`, because a caller
        // takes it from the same list it passes as `values`, and that list is not `'static`.
        Some(cx.alloc_string(default)?.into()),
    )?;

    let text = value.as_string().flatten()?;

    // The value is either one of `values`, which step 5 made sure of, or `default`, which every
    // caller here takes from the same list.
    Ok(values
        .iter()
        .find(|name| text.eq_str(name))
        .copied()
        .expect("the resolved option value is one of the allowed names"))
}

/// GetOption (9.2.11) over the names of one of `Intl.NumberFormat`'s option enums, and the variant
/// the resolved name means. `from_name` is the enum's own mapping from its `NAMES` list, so the two
/// arguments that name the values are one table rather than two that can drift apart.
pub fn get_enum_option<E>(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    values: &'static [&'static str],
    default: &'static str,
    from_name: fn(&str) -> Option<E>,
) -> EvalResult<E> {
    let name = get_name_option(cx, options, property, values, default)?;

    Ok(from_name(name).expect("the option value is one of the enum's own names"))
}

/// GetOption (https://tc39.es/ecma402/#sec-getoption), 9.2.11 for a `string` option that is
/// `required` and whose allowed values are known here, which is 12.1.1 step 8's `type` and nothing
/// else in this namespace: the last argument of 9.2.11 is `required` rather than a default, so a bag
/// with no `type` gets step 2.1's TypeError. The answer is one of the entries of `values` for the
/// same reason `get_name_option`'s is, that step 5 compares the converted value against that list.
pub fn get_required_name_option<'a>(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    values: &'a [&'a str],
) -> EvalResult<&'a str> {
    let value = get_option(
        cx,
        options,
        property,
        OptionType::String,
        values,
        // `None` is the spec's `required`.
        None,
    )?;

    let text = value.as_string().flatten()?;

    Ok(values
        .iter()
        .find(|name| text.eq_str(name))
        .expect("step 5 refused everything but the values it was given"))
}

/// GetBooleanOrStringNumberFormatOption
/// (https://tc39.es/ecma402/#sec-getbooleanorstringnumberformatoption), 9.2.12. The result is
/// either a Boolean or a String, so it comes back as a value rather than as one concrete type;
/// `Intl.NumberFormat` step 16.1.1.21 is the caller that knows which of the two it got.
pub fn get_boolean_or_string_number_format_option(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    string_values: &[&str],
    fallback: Handle<Value>,
) -> EvalResult<Handle<Value>> {
    // Step 1: let value be ? Get(options, propertyKey).
    let value = get(cx, options, property)?;

    // Step 2.
    if value.is_undefined() {
        return Ok(fallback);
    }

    // Step 3: only a `true` Boolean is returned as a Boolean here; every other truthy value falls
    // through to the string conversion.
    if value.is_bool() && value.as_bool() {
        return Ok(cx.bool(true));
    }

    // Step 4: anything that ToBoolean reports false for, not just the `false` Boolean.
    if !to_boolean(*value) {
        return Ok(cx.bool(false));
    }

    // Step 5.
    let value: Handle<Value> = to_string(cx, value)?.into();

    // Step 6: the comparison is against the already converted string, so a number such as `1` is
    // rejected for spelling "1" rather than for not being a string.
    let text = value.as_string().flatten()?;
    if !string_values.iter().any(|allowed| text.eq_str(allowed)) {
        let allowed = string_values.join(", ");
        return range_error(cx, &format!("Intl option must be a boolean or one of: {allowed}"));
    }

    // Step 7.
    Ok(value)
}

/// DefaultNumberOption (https://tc39.es/ecma402/#sec-defaultnumberoption), 9.2.13. The `Option` in
/// the return type is the spec's "an integer or undefined", since `fallback` may itself be
/// undefined.
pub fn default_number_option(
    cx: Context,
    value: Handle<Value>,
    minimum: f64,
    maximum: f64,
    fallback: Option<f64>,
) -> EvalResult<Option<f64>> {
    // Step 1: only an absent option reaches the fallback.
    if value.is_undefined() {
        return Ok(fallback);
    }

    // Step 2.
    let number = to_number(cx, value)?.as_number();

    // Step 3: not finite, or outside the range, is an error rather than a clamp.
    if !number.is_finite() || number < minimum || number > maximum {
        return range_error(
            cx,
            &format!("Intl numeric option must be a finite number between {minimum} and {maximum}"),
        );
    }

    // Step 4.
    Ok(Some(number.floor()))
}

/// GetNumberOption (https://tc39.es/ecma402/#sec-getnumberoption), 9.2.14.
pub fn get_number_option(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    minimum: f64,
    maximum: f64,
    fallback: Option<f64>,
) -> EvalResult<Option<f64>> {
    // Step 1.
    let value = get(cx, options, property)?;

    // Step 2.
    default_number_option(cx, value, minimum, maximum, fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        ContextBuilder,
        embed::{evaluate_script, with_realm},
        type_utilities::same_value_non_numeric_non_allocating,
    };

    /// Run `body` inside a fresh context, with its realm entered the way an embedder would.
    fn with_context(body: impl FnOnce(Context)) {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            body(cx);
        });
        cx.drop();
    }

    /// Evaluate an expression that produces the object the operation is handed.
    fn options(cx: Context, source: &str) -> Handle<ObjectValue> {
        evaluate_script(cx, cx.initial_realm(), source, "options.js", 1)
            .unwrap()
            .as_object()
    }

    fn key(mut cx: Context, name: &str) -> Handle<PropertyKey> {
        let property = cx.alloc_string(name).unwrap();
        PropertyKey::string_handle(cx, property).unwrap()
    }

    fn text(mut cx: Context, value: &str) -> Handle<Value> {
        cx.alloc_string(value).unwrap().as_value()
    }

    fn is_text(value: Handle<Value>, expected: &str) -> bool {
        value.is_string() && value.as_string().flatten().unwrap().eq_str(expected)
    }

    #[test]
    fn absent_options_become_a_prototype_less_object() {
        with_context(|cx| {
            for object in [
                coerce_options_to_object(cx, cx.undefined()).unwrap(),
                get_options_object(cx, cx.undefined()).unwrap(),
            ] {
                // Step 1 of each: OrdinaryObjectCreate(null).
                assert!(object.get_prototype_of(cx).unwrap().is_none());
                assert!(object.own_property_keys(cx).unwrap().is_empty());
            }
        });
    }

    #[test]
    fn coerce_options_to_object_wraps_primitives() {
        with_context(|cx| {
            // Step 2 is ToObject, so a number becomes a Number instance with the Number prototype
            // behind it, rather than an error.
            let object = coerce_options_to_object(cx, cx.smi(5)).unwrap();
            assert!(object.get_prototype_of(cx).unwrap().is_some());

            // A string is wrapped the same way, and `null` is the one primitive that throws.
            assert!(coerce_options_to_object(cx, text(cx, "lookup")).is_ok());
            assert!(coerce_options_to_object(cx, cx.null()).is_err());
        });
    }

    #[test]
    fn get_options_object_refuses_primitives() {
        with_context(|cx| {
            let given = options(cx, "({ localeMatcher: 'lookup' })");

            // Step 2 hands back the very object it was given.
            let taken = get_options_object(cx, given.as_value()).unwrap();
            assert!(same_value_non_numeric_non_allocating(*taken.as_value(), *given.as_value()));

            // Everything else but undefined is a TypeError, including `null`.
            for rejected in [cx.smi(1), text(cx, "lookup"), cx.bool(true), cx.null()] {
                assert!(get_options_object(cx, rejected).is_err());
            }
        });
    }

    #[test]
    fn get_option_falls_back_to_its_default() {
        with_context(|cx| {
            let object = options(cx, "({})");

            let value = get_option(
                cx,
                object,
                key(cx, "localeMatcher"),
                OptionType::String,
                &["lookup", "best fit"],
                Some(text(cx, "best fit")),
            )
            .unwrap();
            assert!(is_text(value, "best fit"));

            // Step 2.1: `required` in place of a default makes an absent option a TypeError.
            assert!(
                get_option(cx, object, key(cx, "localeMatcher"), OptionType::String, &[], None)
                    .is_err()
            );
        });
    }

    #[test]
    fn get_option_converts_and_checks_strings() {
        with_context(|cx| {
            let read = |source: &str, values: &[&str]| {
                get_option(
                    cx,
                    options(cx, source),
                    key(cx, "localeMatcher"),
                    OptionType::String,
                    values,
                    Some(cx.undefined()),
                )
            };

            assert!(is_text(read("({ localeMatcher: 'lookup' })", &["lookup"]).unwrap(), "lookup"));

            // Step 4 converts before step 5 compares, so a number reads as its own string form.
            assert!(is_text(read("({ localeMatcher: 7 })", &[]).unwrap(), "7"));

            // Step 5: an unlisted value is a RangeError, and it is checked after conversion.
            assert!(read("({ localeMatcher: 'closest' })", &["lookup", "best fit"]).is_err());
        });
    }

    #[test]
    fn get_option_converts_booleans() {
        with_context(|cx| {
            for (source, expected) in [("({ numeric: 'yes' })", true), ("({ numeric: '' })", false)]
            {
                let value = get_option(
                    cx,
                    options(cx, source),
                    key(cx, "numeric"),
                    OptionType::Boolean,
                    &[],
                    Some(cx.undefined()),
                )
                .unwrap();
                assert_eq!(value.as_bool(), expected);
            }
        });
    }

    #[test]
    fn boolean_or_string_option_keeps_both_shapes() {
        with_context(|cx| {
            let read = |source: &str| {
                get_boolean_or_string_number_format_option(
                    cx,
                    options(cx, source),
                    key(cx, "useGrouping"),
                    &["min2", "auto", "always", "true", "false"],
                    cx.undefined(),
                )
            };

            // Step 2: an absent option is the fallback, which is why the caller passes the value the
            // style and notation imply rather than a fixed default.
            assert!(read("({})").unwrap().is_undefined());

            // Steps 3-4: `true` and everything that ToBoolean calls false stay Booleans.
            assert_eq!(read("({ useGrouping: true })").unwrap().as_bool(), true);
            assert_eq!(read("({ useGrouping: false })").unwrap().as_bool(), false);
            assert_eq!(read("({ useGrouping: 0 })").unwrap().as_bool(), false);

            // Steps 5-7: a truthy non-true value is stringified and then checked.
            assert!(is_text(read("({ useGrouping: 'min2' })").unwrap(), "min2"));
            assert!(is_text(read("({ useGrouping: 'always' })").unwrap(), "always"));

            // The strings "true" and "false" are allowed values in their own right, which 16.1.1
            // step 22 notes is only there for historical reasons.
            assert!(is_text(read("({ useGrouping: 'true' })").unwrap(), "true"));

            // Step 6: anything else is a RangeError, including the numbers that spell a value the
            // wrong way.
            assert!(read("({ useGrouping: 'sometimes' })").is_err());
            assert!(read("({ useGrouping: 7 })").is_err());
        });
    }

    #[test]
    fn default_number_option_floors_and_bounds() {
        with_context(|cx| {
            let read = |value: Handle<Value>| default_number_option(cx, value, 0.0, 9.0, Some(3.0));

            assert_eq!(read(cx.smi(4)).unwrap(), Some(4.0));
            assert_eq!(read(text(cx, "3.5")).unwrap(), Some(3.0));

            // Step 1: an absent option is the fallback.
            assert_eq!(read(cx.undefined()).unwrap(), Some(3.0));

            // Step 3: NaN and the infinities are not finite, and a value outside the range is an
            // error rather than a clamp.
            assert!(read(cx.nan()).is_err());
            assert!(read(cx.number(f64::INFINITY)).is_err());
            assert!(read(cx.smi(-1)).is_err());
            assert!(read(cx.smi(10)).is_err());
        });
    }

    #[test]
    fn get_number_option_reads_a_property_and_applies_the_fallback() {
        with_context(|cx| {
            let read = |source: &str, name: &str, fallback: Option<f64>| {
                get_number_option(cx, options(cx, source), key(cx, name), 0.0, 9.0, fallback)
            };

            assert_eq!(
                read("({ maximumFractionDigits: '3.5' })", "maximumFractionDigits", Some(3.0))
                    .unwrap(),
                Some(3.0)
            );

            assert_eq!(read("({})", "maximumFractionDigits", Some(3.0)).unwrap(), Some(3.0));

            // With no fallback an absent option stays undefined.
            assert_eq!(read("({})", "maximumFractionDigits", None).unwrap(), None);

            // Out of range is the RangeError of 9.2.13 step 3, not a clamp.
            assert!(
                read("({ maximumFractionDigits: 100 })", "maximumFractionDigits", Some(3.0))
                    .is_err()
            );
        });
    }
}
