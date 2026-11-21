//! The `Intl.Locale.prototype` object (https://tc39.es/ecma402/#sec-properties-of-the-intl-locale-prototype-object),
//! 15.3: the twelve accessors that report the slots of 15.4, the three methods that restate the tag
//! (`toString`, `maximize`, `minimize`) and the eight Locale Info operations of 15.5 that answer what
//! CLDR says about it.
//!
//! The instance and the constructor are [`super::locale_object`]; the questions the `get…` methods
//! ask of the data are [`super::locale_data`]. What is left here is the reading of `this` and the
//! shape of what comes back, which is why every method below is three steps: require the slot, ask,
//! answer.
//!
//! None of the information methods is on the list 15.3 opens with as a property of every Locale, and
//! they are new enough that an engine may leave them out; 15.3.6 and 15.3.12 say so for
//! `firstDayOfWeek` and `numeric`. This build has all of them, because 15.2.2 gives it both "kf" and
//! "kn" and the data of `brimstone_intl_data::locale_info` answers the rest.

use super::locale_data::{
    TimeZones, calendars_of_locale, collations_of_locale, hour_cycles_of_locale,
    numbering_systems_of_locale, text_direction_of_locale, time_zones_of_locale,
    week_info_of_locale,
};
use super::locale_negotiation::{add_likely_subtags, remove_likely_subtags};
use super::locale_object::LocaleObject;
use crate::runtime::{
    Context, Handle, Realm, Value,
    abstract_operations::{construct, create_data_property_or_throw},
    alloc_error::AllocResult,
    array_object::{ArrayObject, create_array_from_list},
    error::range_error,
    eval_result::EvalResult,
    intrinsic_builder::IntrinsicBuilder,
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ordinary_object_create,
};
use crate::{intrinsic_getter_methods, intrinsic_methods, runtime_fn};

/// %Intl.Locale.prototype% (https://tc39.es/ecma402/#sec-properties-of-the-intl-locale-prototype-object),
/// 15.3: an ordinary object that is not an instance, so it has no [[InitializedLocale]] and every
/// method below refuses it.
pub struct LocalePrototype;

impl LocalePrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        // 15.3.2 and 15.3.3 to 15.3.14 and 15.3.23, the accessors, in the order the clause numbers
        // them. `caseFirst` and `numeric` are the two 15.3.4 and 15.3.12 make conditional on
        // 15.2.2's keys, which this build has (see [`LocaleObject`]).
        intrinsic_getter_methods!(cx, builder, {
            base_name LocalePrototype_base_name,
            calendar LocalePrototype_calendar,
            case_first LocalePrototype_case_first,
            collation LocalePrototype_collation,
            first_day_of_week LocalePrototype_first_day_of_week,
            hour_cycle LocalePrototype_hour_cycle,
            language LocalePrototype_language,
            numbering_system LocalePrototype_numbering_system,
            numeric LocalePrototype_numeric,
            region LocalePrototype_region,
            script LocalePrototype_script,
            variants LocalePrototype_variants,
        });

        // 15.3.9, 15.3.10, 15.3.15 and 15.3.16 to 15.3.22. Every one of the ten takes no argument, so
        // each `length` is 0.
        intrinsic_methods!(cx, builder, {
            maximize LocalePrototype_maximize (0),
            minimize LocalePrototype_minimize (0),
            to_string LocalePrototype_to_string (0),
            get_calendars LocalePrototype_get_calendars (0),
            get_collations LocalePrototype_get_collations (0),
            get_hour_cycles LocalePrototype_get_hour_cycles (0),
            get_numbering_systems LocalePrototype_get_numbering_systems (0),
            get_text_info LocalePrototype_get_text_info (0),
            get_time_zones LocalePrototype_get_time_zones (0),
            get_week_info LocalePrototype_get_week_info (0),
        });

        // 15.3.24: @@toStringTag, "Intl.Locale", non-writable and configurable, which is what
        // `builder.to_string_tag` installs.
        builder.to_string_tag(cx.names.intl_locale())?;

        builder.build()
    }

    runtime_fn! {
    /// get Intl.Locale.prototype.baseName ( https://tc39.es/ecma402/#sec-intl.locale.prototype.basename ), 15.3.2.
    fn base_name(cx, this_value, _) {
        // Steps 1 and 2.
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: GetLocaleBaseName(loc.[[Locale]]), 15.5.1.
        Ok(cx.alloc_string(&locale.base_name(cx)?)?.into())
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.calendar ( https://tc39.es/ecma402/#sec-intl.locale.prototype.calendar ), 15.3.3.
    fn calendar(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: loc.[[Calendar]], which 15.1.1 step 37 took from the `ca` keyword of the tag.
        text_or_undefined(cx, locale.slot(cx, "ca")?)
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.caseFirst ( https://tc39.es/ecma402/#sec-intl.locale.prototype.casefirst ), 15.3.4.
    fn case_first(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: loc.[[CaseFirst]] (15.1.1 step 41), the `kf` keyword as the tag spells it - so the
        // empty String for a `kf` that was written without a value, which is what
        // `new Intl.Locale("en-u-kf").caseFirst` answers.
        text_or_undefined(cx, locale.case_first(cx)?)
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.collation ( https://tc39.es/ecma402/#sec-intl.locale.prototype.collation ), 15.3.5.
    fn collation(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: loc.[[Collation]], the `co` keyword.
        text_or_undefined(cx, locale.slot(cx, "co")?)
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.firstDayOfWeek ( https://tc39.es/ecma402/#sec-intl.locale.prototype.firstdayofweek ), 15.3.6.
    fn first_day_of_week(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: loc.[[FirstDayOfWeek]] (15.1.1 step 39), which is the `fw` keyword and a String
        // rather than a number: 15.4 says so, and `getWeekInfo` below is where the number lives.
        text_or_undefined(cx, locale.slot(cx, "fw")?)
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.hourCycle ( https://tc39.es/ecma402/#sec-intl.locale.prototype.hourcycle ), 15.3.7.
    fn hour_cycle(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: loc.[[HourCycle]], the `hc` keyword.
        text_or_undefined(cx, locale.slot(cx, "hc")?)
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.language ( https://tc39.es/ecma402/#sec-intl.locale.prototype.language ), 15.3.8.
    fn language(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: GetLocaleLanguage(loc.[[Locale]]), 15.5.2.
        Ok(cx.alloc_string(&locale.language(cx)?)?.into())
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.numberingSystem ( https://tc39.es/ecma402/#sec-intl.locale.prototype.numberingsystem ), 15.3.11.
    fn numbering_system(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: loc.[[NumberingSystem]], the `nu` keyword.
        text_or_undefined(cx, locale.slot(cx, "nu")?)
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.numeric ( https://tc39.es/ecma402/#sec-intl.locale.prototype.numeric ), 15.3.12.
    fn numeric(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: loc.[[Numeric]] (15.1.1 step 42), a Boolean.
        Ok(cx.bool(locale.numeric(cx)?))
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.region ( https://tc39.es/ecma402/#sec-intl.locale.prototype.region ), 15.3.13.
    fn region(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: GetLocaleRegion(loc.[[Locale]]), 15.5.4.
        text_or_undefined(cx, locale.region(cx)?)
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.script ( https://tc39.es/ecma402/#sec-intl.locale.prototype.script ), 15.3.14.
    fn script(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: GetLocaleScript(loc.[[Locale]]), 15.5.3.
        text_or_undefined(cx, locale.script(cx)?)
    }}

    runtime_fn! {
    /// get Intl.Locale.prototype.variants ( https://tc39.es/ecma402/#sec-intl.locale.prototype.variants ), 15.3.23.
    fn variants(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: GetLocaleVariants(loc.[[Locale]]), 15.5.5: the whole run of them, hyphen separated,
        // in the order the tag wrote them.
        text_or_undefined(cx, locale.variants(cx)?)
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.maximize ( https://tc39.es/ecma402/#sec-intl.locale.prototype.maximize ), 15.3.9.
    fn maximize(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: Add Likely Subtags (https://unicode.org/reports/tr35/#Likely_Subtags), which signals
        // an error for a tag whose subtags are in none of the tables - and then the answer is the tag
        // itself, which is what step 3's second sentence asks for.
        let tag = locale.tag(cx)?;
        let maximal = add_likely_subtags(&tag).unwrap_or(tag);

        // Step 4: ! Construct(%Intl.Locale%, « maximal »), which is the constructor of 15.1.1 running
        // on a tag this engine wrote and on no options at all.
        construct_locale(cx, &maximal)
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.minimize ( https://tc39.es/ecma402/#sec-intl.locale.prototype.minimize ), 15.3.10.
    fn minimize(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: Remove Likely Subtags, which is the same tables read the other way. Note that the
        // clause applies it to the tag the maximizer would produce, not to a minimized base name:
        // `minimize` of "en-Latn-GB" is "en-GB", because the removal's own search for the shortest
        // tag that expands to it settles on that one.
        let tag = locale.tag(cx)?;
        let minimal = remove_likely_subtags(&tag).unwrap_or(tag);

        // Step 4.
        construct_locale(cx, &minimal)
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.toString ( https://tc39.es/ecma402/#sec-intl.locale.prototype.tostring ), 15.3.15.
    fn to_string(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: loc.[[Locale]], which 15.1.1 step 36 wrote as the record's [[locale]]: the whole
        // canonicalized tag, extension and private use sequence included.
        let tag = locale
            .locale()
            .expect("the slot require_initialized checked");

        Ok(tag.to_handle(cx))
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.getCalendars ( https://tc39.es/ecma402/#sec-intl.locale.prototype.getcalendars ), 15.3.16.
    fn get_calendars(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: CalendarsOfLocale (15.5.9), which is the calendar the tag names or the one the
        // locale's region prefers.
        let list = calendars_of_locale(&locale.tag(cx)?);

        Ok(array_of_strings(cx, list.iter().map(String::as_str))?.as_value())
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.getCollations ( https://tc39.es/ecma402/#sec-intl.locale.prototype.getcollations ), 15.3.17.
    fn get_collations(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: CollationsOfLocale (15.5.10).
        let list = collations_of_locale(&locale.tag(cx)?);

        Ok(array_of_strings(cx, list.iter().map(String::as_str))?.as_value())
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.getHourCycles ( https://tc39.es/ecma402/#sec-intl.locale.prototype.gethourcycles ), 15.3.18.
    fn get_hour_cycles(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: HourCyclesOfLocale (15.5.11).
        let list = hour_cycles_of_locale(&locale.tag(cx)?);

        Ok(array_of_strings(cx, list.iter().map(String::as_str))?.as_value())
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.getNumberingSystems ( https://tc39.es/ecma402/#sec-intl.locale.prototype.getnumberingsystems ), 15.3.19.
    fn get_numbering_systems(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: NumberingSystemsOfLocale (15.5.12).
        let list = numbering_systems_of_locale(&locale.tag(cx)?);

        Ok(array_of_strings(cx, list.iter().map(String::as_str))?.as_value())
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.getTimeZones ( https://tc39.es/ecma402/#sec-intl.locale.prototype.gettimezones ), 15.3.20.
    fn get_time_zones(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        match time_zones_of_locale(&locale.tag(cx)?) {
            // 15.5.13 steps 1-2: a tag that names no region has no list of zones, and the operation
            // says so by returning undefined.
            TimeZones::NoRegion => Ok(cx.undefined()),

            // Step 3 and 15.3.20's CreateArrayFromList.
            TimeZones::Known(zones) => Ok(array_of_strings(cx, zones.iter().copied())?.as_value()),

            // Step 3 is implementation-defined, and this build generated the zones of three regions.
            // An empty list would be the clause's own answer for a region that uses no time zone,
            // which is a claim about Germany rather than about this engine, so the method says what
            // it cannot do instead.
            TimeZones::UnknownRegion(region) => range_error(
                cx,
                &format!(
                    "Intl.Locale.getTimeZones has no time zone data for the region '{region}'"
                ),
            ),
        }
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.getTextInfo ( https://tc39.es/ecma402/#sec-intl.locale.prototype.gettextinfo ), 15.3.21.
    fn get_text_info(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3: OrdinaryObjectCreate(%Object.prototype%).
        let info = ordinary_object_create(cx)?;

        // Steps 4 and 5: TextDirectionOfLocale (15.5.14), which is the direction of the tag's script
        // and undefined when the tables do not give the script one. The property exists either way:
        // step 5 creates it unconditionally. Both arms are handles, so the write of step 5 cannot
        // collect the String out from under itself.
        let direction = match text_direction_of_locale(&locale.tag(cx)?) {
            Some(direction) => cx.alloc_string(direction)?.into(),
            None => cx.undefined(),
        };
        create_data_property_or_throw(cx, info, cx.names.direction(), direction)?;

        // Step 6.
        Ok(info.as_value())
    }}

    runtime_fn! {
    /// Intl.Locale.prototype.getWeekInfo ( https://tc39.es/ecma402/#sec-intl.locale.prototype.getweekinfo ), 15.3.22.
    fn get_week_info(cx, this_value, _) {
        let locale = LocaleObject::require_initialized(cx, this_value)?;

        // Step 3.
        let info = ordinary_object_create(cx)?;

        // Step 4: WeekInfoOfLocale (15.5.17), the record Table 27 defines.
        let week = week_info_of_locale(&locale.tag(cx)?);

        // Step 5: [[FirstDay]] as an integral Number, ISO 8601's: Monday is 1 and Sunday is 7.
        let first_day = Value::number(f64::from(week.first_day)).to_handle(cx);
        create_data_property_or_throw(cx, info, cx.names.first_day(), first_day)?;

        // Step 6: [[Weekend]] as CreateArrayFromList of the same kind of number, in week order.
        let mut weekend = Vec::with_capacity(week.weekend.len());
        for day in &week.weekend {
            weekend.push(Value::number(f64::from(*day)).to_handle(cx));
        }
        let weekend = create_array_from_list(cx, &weekend)?;
        create_data_property_or_throw(cx, info, cx.names.weekend(), weekend.as_value())?;

        // Step 7.
        Ok(info.as_value())
    }}
}

/// A slot that is "either undefined or a String value" (15.4), as the value 15.3's accessors return.
fn text_or_undefined(mut cx: Context, text: Option<String>) -> EvalResult<Handle<Value>> {
    match text {
        Some(text) => Ok(cx.alloc_string(&text)?.as_value()),
        None => Ok(cx.undefined()),
    }
}

/// CreateArrayFromList of the strings one of the operations of 15.5 answered with.
fn array_of_strings<'a, I: IntoIterator<Item = &'a str>>(
    mut cx: Context,
    list: I,
) -> EvalResult<Handle<ArrayObject>> {
    let mut elements: Vec<Handle<Value>> = Vec::new();

    for text in list {
        elements.push(cx.alloc_string(text)?.as_value());
    }

    Ok(create_array_from_list(cx, &elements)?)
}

/// 15.3.9 step 4 and 15.3.10 step 4: `! Construct(%Intl.Locale%, « tag »)`. It is a call of the
/// constructor rather than a copy of the instance because the result is a *new* Locale with its own
/// [[Locale]] - and because a subclass's `maximize` returns a plain `Intl.Locale`, which is what
/// naming %Intl.Locale% rather than the receiver in those steps means.
fn construct_locale(mut cx: Context, tag: &str) -> EvalResult<Handle<Value>> {
    let constructor = cx
        .initial_realm()
        .get_intrinsic(Intrinsic::LocaleConstructor);
    let argument = cx.alloc_string(tag)?.as_value();

    // The `!` of the step: 15.1.1 cannot throw on a tag this engine produced and no options, so a
    // throw here is a bug in this file rather than an error a program can cause. It is still the
    // exception that goes back, since that is what a caller can see.
    let locale = construct(cx, constructor.as_object(), &[argument], None)?;

    Ok(locale.as_value())
}
