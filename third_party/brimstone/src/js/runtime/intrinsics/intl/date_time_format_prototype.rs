//! The `Intl.DateTimeFormat.prototype` object (https://tc39.es/ecma402/#sec-properties-of-the-intl-datetimeformat-prototype-object),
//! 11.3: `resolvedOptions`, `formatRange`, `formatRangeToParts` and `formatToParts`, the `format`
//! getter, and the tag that makes an instance report "[object Intl.DateTimeFormat]".
//!
//! The instance itself, the reading of its options and the rendering are
//! [`super::date_time_format_object`]; this file is only the methods a program calls and the function
//! `format` hands back.

use super::date_time_format_core::{
    DateTimePart, Formattable, TemporalKind, TemporalSource, Width,
};
use super::date_time_format_object::{DateTimeFormatObject, ResolvedDateTimeFormat};
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
        intrinsics::{
            intrinsics::Intrinsic,
            rust_runtime::RuntimeFunction,
            temporal::{
                instant_object::InstantObject, plain_date_object::PlainDateObject,
                plain_date_time_object::PlainDateTimeObject,
                plain_month_day_object::PlainMonthDayObject, plain_time_object::PlainTimeObject,
                plain_year_month_object::PlainYearMonthObject,
                zoned_date_time_object::ZonedDateTimeObject,
            },
        },
        object_value::ObjectValue,
        ordinary_object::ordinary_object_create,
        type_utilities::to_number,
    },
    runtime_fn,
};

/// %Intl.DateTimeFormat.prototype% (https://tc39.es/ecma402/#sec-properties-of-the-intl-datetimeformat-prototype-object),
/// 11.3.
pub struct DateTimeFormatPrototype;

impl DateTimeFormatPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        // 11.3.2 to 11.3.6, installed in the order 11.3 lists them because that is the order
        // `Object.getOwnPropertyNames` reports properties in: the method, then the accessor, then the
        // three that format.
        builder.method(
            cx.names.resolved_options(),
            RuntimeFunction::DateTimeFormatPrototype_resolved_options,
            0,
        )?;

        // 11.3.3.
        builder.getter(cx.names.format(), RuntimeFunction::DateTimeFormatPrototype_get_format)?;

        intrinsic_methods!(cx, builder, {
            format_range DateTimeFormatPrototype_format_range (2),
            format_range_to_parts DateTimeFormatPrototype_format_range_to_parts (2),
            format_to_parts DateTimeFormatPrototype_format_to_parts (1),
        });

        // 11.3.7: @@toStringTag, the String "Intl.DateTimeFormat", non-writable and configurable,
        // which is what `builder.to_string_tag` installs.
        builder.to_string_tag(cx.names.intl_date_time_format())?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.DateTimeFormat.prototype.resolvedOptions ( https://tc39.es/ecma402/#sec-intl.datetimeformat.prototype.resolvedoptions ), 11.3.2.
    fn resolved_options(cx, this_value, _) {
        // Steps 1 to 3: UnwrapDateTimeFormat (11.5.13), then the check of
        // [[InitializedDateTimeFormat]] on what it gave back.
        let date_time_format = DateTimeFormatObject::unwrap_initialized(cx, this_value)?;

        let Some(resolved) = date_time_format.resolved() else {
            // The question `unwrap_initialized` has already answered.
            return type_error(
                cx,
                "Intl.DateTimeFormat method called on an uninitialized instance",
            );
        };

        // The four String slots of 11.4 are raw pointers inside the instance's own data, so they go
        // into handles here, before anything else allocates: a collection during step 4 or 5 would
        // move each string and leave the copy above pointing at the block it was copied out of.
        let locale = resolved.locale.to_handle(cx);
        let calendar = resolved.calendar.to_handle(cx);
        let numbering_system = resolved.numbering_system.to_handle(cx);
        let time_zone = resolved.time_zone_name.to_handle(cx);

        // Step 4: `OrdinaryObjectCreate(%Object.prototype%)`.
        let options = ordinary_object_create(cx)?;

        report_resolved_options(
            cx,
            options,
            &resolved,
            locale,
            calendar,
            numbering_system,
            time_zone,
        )?;

        // Step 6.
        Ok(options.as_value())
    }}

    runtime_fn! {
    /// Intl.DateTimeFormat.prototype.formatRange ( https://tc39.es/ecma402/#sec-intl.datetimeformat.prototype.formatrange ), 11.3.4.
    fn format_range(cx, this_value, arguments) {
        // Steps 1 and 2: `RequireInternalSlot(dtf, [[InitializedDateTimeFormat]])`. 11.3.4 to 11.3.6
        // are the methods of 11.3 that do not start with UnwrapDateTimeFormat, so an object that only
        // carries an instance under %Intl%.[[FallbackSymbol]] - the `this` of the legacy call mode -
        // is refused here rather than formatted through the instance hidden on it.
        let date_time_format = DateTimeFormatObject::require_initialized(cx, this_value)?;

        // Step 3.
        let start = arguments.get(cx, 0);
        let end = arguments.get(cx, 1);

        if start.is_undefined() || end.is_undefined() {
            return type_error(
                cx,
                "Intl.DateTimeFormat formatRange requires both a start and an end date",
            );
        }

        // Steps 4 and 5: `ToDateTimeFormattable` of 15.6.11, twice in this order, so that the
        // `valueOf` of one of the two arguments that has one runs before the other is read and neither
        // is read at all for a Temporal object. Step 6 is 11.5.9's `FormatDateTimeRange`, whose 11.5.8
        // is where the two answers are compared and the TimeClip and range checks of 15.6.21 happen.
        let x = date_time_formattable(cx, start)?;
        let y = date_time_formattable(cx, end)?;

        let text = date_time_format.format_range(cx, &x, &y)?;

        Ok(cx.alloc_string(&text)?.as_value())
    }}

    runtime_fn! {
    /// Intl.DateTimeFormat.prototype.formatRangeToParts ( https://tc39.es/ecma402/#sec-intl.datetimeformat.prototype.formatrangetoparts ), 11.3.5.
    fn format_range_to_parts(cx, this_value, arguments) {
        // Steps 1 and 2, and steps 3 to 5 for the two ends: the same require of 11.3.4 rather than
        // the unwrap, since this too formats through the instance it is called on.
        let date_time_format = DateTimeFormatObject::require_initialized(cx, this_value)?;

        let start = arguments.get(cx, 0);
        let end = arguments.get(cx, 1);

        if start.is_undefined() || end.is_undefined() {
            return type_error(
                cx,
                "Intl.DateTimeFormat formatRangeToParts requires both a start and an end date",
            );
        }

        // Steps 4 and 5, then 11.5.10's `FormatDateTimeRangeToParts`, which is the partition 11.5.8
        // gives: the two answers are converted in this order, and compared and clipped inside the call
        // below.
        let x = date_time_formattable(cx, start)?;
        let y = date_time_formattable(cx, end)?;

        let parts = date_time_format.format_range_parts(cx, &x, &y)?;

        // 11.5.10 steps 2 to 4: one `{ type, value }` record per part of the partition, the glue of
        // the range pattern being a `literal` like any other piece of text between two fields.
        let mut elements = Vec::with_capacity(parts.len());
        for part in &parts {
            elements.push(date_time_part_to_object(cx, part)?.into());
        }

        Ok(create_array_from_list(cx, &elements)?.into())
    }}

    runtime_fn! {
    /// Intl.DateTimeFormat.prototype.formatToParts ( https://tc39.es/ecma402/#sec-intl.datetimeformat.prototype.formattoparts ), 11.3.6.
    fn format_to_parts(cx, this_value, arguments) {
        // Steps 1 and 2. See `format_range` above for why this is the require rather than the unwrap.
        let date_time_format = DateTimeFormatObject::require_initialized(cx, this_value)?;

        // Steps 3 to 5.
        let x = date_time_formattable(cx, arguments.get(cx, 0))?;

        // Step 6: `FormatDateTimeToParts(dtf, x)`, 11.5.7, which is the partition 11.5.6 gives.
        let parts = date_time_format.format(cx, &x)?;

        // 11.5.7 steps 2 to 4: one `{ type, value }` record per part, in an array.
        let mut elements = Vec::with_capacity(parts.len());
        for part in &parts {
            elements.push(date_time_part_to_object(cx, part)?.into());
        }

        Ok(create_array_from_list(cx, &elements)?.into())
    }}

    runtime_fn! {
    /// get Intl.DateTimeFormat.prototype.format ( https://tc39.es/ecma402/#sec-intl.datetimeformat.prototype.format ), 11.3.3.
    fn get_format(cx, this_value, _) {
        // Steps 1 to 3: UnwrapDateTimeFormat and the check of [[InitializedDateTimeFormat]].
        let mut date_time_format = DateTimeFormatObject::unwrap_initialized(cx, this_value)?;

        // Steps 4 and 5, for an instance that has been asked before: [[BoundFormat]] is the one
        // function the getter made, which is what makes `dtf.format === dtf.format` hold.
        let bound_format = date_time_format.bound_format();
        if !bound_format.is_undefined() {
            return Ok(bound_format.to_handle(cx));
        }

        // Step 4.1: the abstract closure. The builtin functions of this engine carry no closure
        // environment, so "captures dtf" is built the way `Proxy.revocable`'s revoker and NumberFormat's
        // own `format` are built: the instance hangs off the function under a private symbol and the
        // function reads it back. That is also why the function below ignores its `this` value, which
        // is what lets `dates.map(dtf.format)` format in `dtf`'s locale and zone.
        //
        // Step 4.2: `CreateBuiltinFunction(closure, 1, "", « »)` - the name is the empty string.
        let realm = cx.current_realm();
        let mut format_function = BuiltinFunction::create(
            cx,
            RuntimeFunction::date_time_format_prototype_format,
            1,
            cx.names.empty_string(),
            realm,
            None,
        )?;
        format_function.private_element_set(
            cx,
            cx.symbols.bound_date_time_format().cast(),
            date_time_format.into(),
        )?;

        // Step 4.2: `Set(dtf.[[BoundFormat]], boundFormat)`, and step 5 returns it.
        date_time_format.set_bound_format(*format_function.as_value());

        Ok(format_function.as_value())
    }}
}

runtime_fn! {
/// 11.3.3 step 4.1's closure, which is what `Intl.DateTimeFormat.prototype.format` hands out. Its
/// `this` value is ignored on purpose: the instance is the one the getter captured.
fn format(cx, _, arguments) {
    // The capture, read back the way a revoker reads the proxy it revokes.
    let captured = cx
        .current_function()
        .private_element_find(cx, cx.symbols.bound_date_time_format().cast());

    let Some(captured) = captured else {
        return type_error(cx, "Intl.DateTimeFormat format function without its captured instance");
    };

    let date_time_format = captured.value().cast::<DateTimeFormatObject>();

    let x = date_time_formattable(cx, arguments.get(cx, 0))?;

    // Step 4.1.2: `Return FormatDateTime(dtf, x)`, 11.5.6.
    let text = date_time_format.format_to_string(cx, &x)?;

    Ok(cx.alloc_string(&text)?.as_value())
}}

/// 15.6.11's `ToDateTimeFormattable`, which is the one branch the four entry points of 11.3 take
/// together, and the one 20.4.4.7's `Date.prototype.toLocaleString` would take if it had a Temporal
/// argument to give: a Temporal object is handed down as the source it is and never coerced, because
/// clause 15 forbids `valueOf` on it, and anything else - an ordinary object with a `calendar`
/// property of its own included - is the number that 15.6.21's handler then clips.
///
/// The undefined of a missing argument is the current instant, as 11.3.3 step 4.1.1 and 11.3.6 step 4
/// say. `%Date.now%` is read from the engine's clock rather than called through the `Date`
/// constructor, so that a program which has replaced `Date.now` cannot move the instant a bare
/// `format()` renders: the intrinsic is the built-in function, and 21.4.4.1 is TimeClip of this same
/// reading.
fn date_time_formattable(cx: Context, value: Handle<Value>) -> EvalResult<Formattable> {
    if value.is_object() {
        if let Some(source) = temporal_source(value.as_object()) {
            return Ok(Formattable::Temporal(source));
        }

        if value.as_object().is::<ZonedDateTimeObject>() {
            // 15.6.11, which answers an object for which 15.6.12 is true and calls nothing on it. The
            // TypeError is 15.6.22's last step, and it belongs to the dispatch rather than to this
            // coercion: 15.6.8 converts both of its arguments before it reads either of them.
            return Ok(Formattable::ZonedDateTime);
        }
    }

    if value.is_undefined() {
        return Ok(Formattable::TimeValue(cx.current_unix_time_millis() as f64));
    }

    Ok(Formattable::TimeValue(to_number(cx, value)?.as_number()))
}

/// 15.6.12's `IsTemporalObject`, 15.6.13's `SameTemporalType` answer and 15.6.14's read, in one: the
/// dispatch of 15.6.22 in the order it tests for the seven types, and `None` for anything that is not
/// one of them. The test is the same `is::<>` chain that `IsPartialTemporalObject` of
/// `crate::runtime::intrinsics::temporal::utils` is written with.
///
/// What comes back is a copy of two plain values and nothing else. The count of nanoseconds is an
/// `i128`, and the calendar identifier is the borrowed `&'static str` of `temporal_rs`'s own table of
/// them rather than a borrow of the object, so the object is out of reach of every allocation the
/// rendering makes after this and is not seen by the collector while they happen.
///
/// `Temporal.PlainTime` and `Temporal.Instant` have no calendar to compare and are given `iso8601`,
/// which is the identifier [`check_temporal_calendar`] accepts without looking at the instance.
fn temporal_source(object: Handle<ObjectValue>) -> Option<TemporalSource> {
    if let Some(plain_date) = object.as_opt::<PlainDateObject>() {
        return Some(TemporalSource {
            kind: TemporalKind::Date,
            epoch_nanoseconds: plain_date.date().epoch_ns_for_utc().as_i128(),
            calendar: plain_date.date().calendar().identifier(),
        });
    }

    if let Some(plain_date_time) = object.as_opt::<PlainDateTimeObject>() {
        return Some(TemporalSource {
            kind: TemporalKind::DateTime,
            epoch_nanoseconds: plain_date_time.date_time().epoch_ns_for_utc().as_i128(),
            calendar: plain_date_time.date_time().calendar().identifier(),
        });
    }

    if let Some(plain_month_day) = object.as_opt::<PlainMonthDayObject>() {
        return Some(TemporalSource {
            kind: TemporalKind::MonthDay,
            epoch_nanoseconds: plain_month_day.month_day().epoch_ns_for_utc().as_i128(),
            calendar: plain_month_day.month_day().calendar().identifier(),
        });
    }

    if let Some(plain_time) = object.as_opt::<PlainTimeObject>() {
        return Some(TemporalSource {
            kind: TemporalKind::Time,
            epoch_nanoseconds: plain_time.time().epoch_ns_for_utc().as_i128(),
            calendar: "iso8601",
        });
    }

    if let Some(plain_year_month) = object.as_opt::<PlainYearMonthObject>() {
        return Some(TemporalSource {
            kind: TemporalKind::YearMonth,
            epoch_nanoseconds: plain_year_month.year_month().epoch_ns_for_utc().as_i128(),
            calendar: plain_year_month.year_month().calendar().identifier(),
        });
    }

    if let Some(instant) = object.as_opt::<InstantObject>() {
        return Some(TemporalSource {
            kind: TemporalKind::Instant,
            epoch_nanoseconds: instant.instant().epoch_nanoseconds().as_i128(),
            calendar: "iso8601",
        });
    }

    None
}

/// The `{ type, value }` record of 11.5.7 step 4.2, for one part of the partition of a single date,
/// and the `{ type, value, source }` record of 11.5.10 step 4 for one part of the partition of a
/// range - the two being the same object with the Table 17 row that only a range has.
fn date_time_part_to_object(
    mut cx: Context,
    part: &DateTimePart,
) -> EvalResult<Handle<ObjectValue>> {
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

    if let Some(source) = part.source {
        create_data_property_or_throw(
            cx,
            record,
            cx.names.source(),
            cx.alloc_string(source.as_str())?.as_value(),
        )?;
    }

    Ok(record)
}

/// 11.3.2 step 5: Table 15, in table order, from the slots of an initialized instance.
///
/// The rows that have an internal slot are the first five and the last two; the component rows between
/// them read `dtf.[[DateTimeFormat]]`, which is why they are skipped whole when a style shorthand was
/// asked for. A value this build does not have is left off the object rather than reported as
/// `undefined`, which is what step 5.4's "if value is not undefined" means.
///
/// The four String slots come in as handles rather than being read off `resolved` here: the record is
/// a copy taken out of the block, so the pointers in it are only current until the next allocation,
/// and the first of those is the object this function is writing into.
fn report_resolved_options(
    cx: Context,
    options: Handle<ObjectValue>,
    resolved: &ResolvedDateTimeFormat,
    locale: Handle<Value>,
    calendar: Handle<Value>,
    numbering_system: Handle<Value>,
    time_zone: Handle<Value>,
) -> EvalResult<()> {
    report_value(cx, options, cx.names.locale(), locale)?;
    report_value(cx, options, cx.names.calendar(), calendar)?;
    report_value(cx, options, cx.names.numbering_system(), numbering_system)?;
    report_value(cx, options, cx.names.time_zone(), time_zone)?;

    // [[HourCycle]] and the "hour12" row that converts it. The conversion is the one the note beside
    // Table 15 gives for web compatibility: h11 and h12 are true, h23 and h24 are false, and neither
    // row is there at all for a format with no hour in it.
    if let Some(hour_cycle) = resolved.hour_cycle {
        let name = hour_cycle.as_str();

        report_str(cx, options, cx.names.hour_cycle(), name)?;
        report_value(cx, options, cx.names.hour12(), cx.bool(name == "h11" || name == "h12"))?;
    }

    // The rows 11.3.2 step 5.2.2 reads off `[[DateTimeFormat]]`, which is the matched pattern: their
    // width is the one that pattern renders with rather than the one the program asked for, so
    // `{ hour: "numeric" }` reports "2-digit" in a locale whose pattern counts the hour with a zero.
    if resolved.date_style.is_none() && resolved.time_style.is_none() {
        let widths = &resolved.widths;

        report_width(cx, options, cx.names.weekday(), widths.weekday)?;
        report_width(cx, options, cx.names.era(), widths.era)?;
        report_width(cx, options, cx.names.year(), widths.year)?;
        report_width(cx, options, cx.names.month(), widths.month)?;
        report_width(cx, options, cx.names.day(), widths.day)?;
        report_width(cx, options, cx.names.day_period(), widths.day_period)?;
        report_width(cx, options, cx.names.hour(), widths.hour)?;
        report_width(cx, options, cx.names.minute(), widths.minute)?;
        report_width(cx, options, cx.names.second(), widths.second)?;

        // The one component row whose "Conversion" cell is not empty: the digits count is reported as
        // the number it was read as, not as the string a program would have written.
        if let Some(digits) = resolved.fractional_second_digits {
            report_number(cx, options, cx.names.fractional_second_digits(), digits)?;
        }

        // The last of the component rows, and the one of them whose values are this build's
        // `TimeZoneNameStyle` names rather than the four widths of Table 16.
        if let Some(style) = widths.time_zone_name {
            report_str(cx, options, cx.names.time_zone_name(), style.as_str())?;
        }
    }

    if let Some(style) = resolved.date_style {
        report_str(cx, options, cx.names.date_style(), style.as_str())?;
    }

    if let Some(style) = resolved.time_style {
        report_str(cx, options, cx.names.time_style(), style.as_str())?;
    }

    Ok(())
}

/// A row of Table 15 whose value is one of this build's names.
fn report_str(
    mut cx: Context,
    options: Handle<ObjectValue>,
    name: Handle<PropertyKey>,
    value: &str,
) -> EvalResult<()> {
    let value = cx.alloc_string(value)?;

    create_data_property_or_throw(cx, options, name, value.as_value())
}

/// A row of Table 15 whose value is a Number, which is `fractionalSecondDigits` alone.
fn report_number(
    cx: Context,
    options: Handle<ObjectValue>,
    name: Handle<PropertyKey>,
    value: impl Into<f64>,
) -> EvalResult<()> {
    create_data_property_or_throw(cx, options, name, Value::number(value.into()).to_handle(cx))
}

/// A row of Table 15 whose value is neither, which here is `locale`, `hour12` and the absent `widths`
/// this operation declines to write.
fn report_value(
    cx: Context,
    options: Handle<ObjectValue>,
    name: Handle<PropertyKey>,
    value: Handle<Value>,
) -> EvalResult<()> {
    create_data_property_or_throw(cx, options, name, value)
}

/// A component row of Table 15, which is written only when the format has that component.
fn report_width(
    cx: Context,
    options: Handle<ObjectValue>,
    name: Handle<PropertyKey>,
    width: Option<Width>,
) -> EvalResult<()> {
    match width {
        Some(width) => report_str(cx, options, name, width.as_str()),
        None => Ok(()),
    }
}
