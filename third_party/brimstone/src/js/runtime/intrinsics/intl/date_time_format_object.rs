//! The Intl.DateTimeFormat instance and the reading of the options that fill it.
//!
//! DateTimeFormat is split over three files the way ECMA-402 splits it: this one holds the instance
//! and the operations that work from it (11.1.2's `CreateDateTimeFormat`, 11.5.13's unwrap, 11.5.6's
//! `FormatDateTime` and 11.5.8's `PartitionDateTimeRangePattern`),
//! [`super::date_time_format_constructor`] the constructor and `supportedLocalesOf` (11.1.1, 11.2.2),
//! and [`super::date_time_format_prototype`] the methods a program calls (11.3).
//!
//! What an instance holds is the list of 11.4's internal slots: the locale and the calendar and the
//! zone it resolved to, and the resolved options themselves. The `icu_datetime` formatter those
//! options name is not stored on it - it comes from the per-context cache of
//! [`super::cache::LocaleCache`], which is also why every instance of one locale and one field set
//! shares one loaded pattern.
//!
//! The order the options are read in is the one the specification's own steps give, because a getter
//! on the options bag can be seen to run in it. The table of what the reads mean is the shape of
//! Boa's `core/engine/src/builtins/intl/date_time_format/options.rs` (boa-dev/boa, MIT OR
//! Unlicense), which the design review names as the worked example to adapt; its `ToDateTimeOptions`
//! and its hardcoded UTC default time zone are not taken, as the review says.

use std::rc::Rc;

use super::cache::DataLocale;
use super::date_time_format_core::{
    AskedWidths, Components, DateTimePart, DateTimeRenderer, FieldSetError, FieldSetKey,
    FormatZone, Formattable, HourCycle, NarrowWidths, Style, TemporalKind, TemporalSource,
    TimePrecisionKey, TimeZoneNameStyle, Width, host_zone, hour_cycle_of_locale,
    temporal_components,
};
use super::intl_object::canonicalize_locale_list_argument;
use super::locale_negotiation::{
    Matcher, RelevantExtensionKey, ResolvedLocale, resolve_locale_with_keys,
};
use super::options::{coerce_options_to_object, get_name_option, get_number_option};
use crate::extend_object;
use crate::runtime::{
    Context, Handle, HeapPtr, PropertyKey, Value,
    abstract_operations::ordinary_has_instance,
    error::{range_error, type_error},
    eval_result::EvalResult,
    gc::{HeapItem, HeapVisitor},
    get,
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ObjectBuilder,
    to_string,
    type_utilities::to_boolean,
};

/// Which of the two groups 11.1.2's `required` says a caller needs, and so which of them the defaults
/// of its step 31.5 may not leave out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Required {
    Date,
    Time,
    Any,
}

/// Which of them 11.1.2 step 31.6 fills in for a bag that named no field: the constructor asks for
/// both, and `Date.prototype`'s three methods each ask for one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Defaults {
    Date,
    Time,
    All,
}

extend_object! {
    /// Properties of Intl.DateTimeFormat Instances
    /// (https://tc39.es/ecma402/#sec-properties-of-intl-datetimeformat-instances), 11.4.
    ///
    /// The record is written once, at the end of construction, so that no method can see an instance
    /// half read, and its absence is this engine's spelling of `[[InitializedDateTimeFormat]]`.
    pub struct DateTimeFormatObject {
        resolved: Option<ResolvedDateTimeFormat>,
        /// [[BoundFormat]] (11.3.3 steps 4 to 6): undefined until the `format` getter builds the
        /// function, and that same function for every read after it.
        bound_format: Value,
    }
}

/// The slots 11.1.2's steps 8 to 33 fill, carried with the option bag so that one `Option` on the
/// instance says whether it is initialized at all.
///
/// Every field is `Copy`: an Intl instance lives in the GC heap, which holds nothing that can drop,
/// so the three String slots of 11.4 are JS string values rather than Rust ones, and the zone is its
/// canonical id with its offset filled in again per moment by [`zone_at`].
#[derive(Clone, Copy)]
pub struct ResolvedDateTimeFormat {
    /// [[Locale]] (step 8), as the String value 11.3.2 reports.
    pub locale: Value,
    /// The locale whose data backs that tag: 9.2.7 step 5's answer, and the key of the renderer this
    /// format uses.
    pub data_locale: DataLocale,
    /// [[Calendar]] and [[NumberingSystem]] (steps 10 and 11), which 9.2.7 resolves over two of the
    /// three keys of 11.2.3's [[RelevantExtensionKeys]].
    pub calendar: Value,
    pub numbering_system: Value,
    /// [[TimeZone]] (step 20): the zone of 11.5.3's canonicalization, with the offset of the moment
    /// filled in at each rendering by [`zone_at`], and its canonical name as the String 11.3.2
    /// reports. See [`super::date_time_format_core::FormatZone`].
    pub time_zone: FormatZone,
    pub time_zone_name: Value,
    /// The component rows of 11.3.2's Table 15, which a style shorthand leaves empty.
    pub widths: ResolvedWidths,
    /// [[HourCycle]] (step 33), which is only there when the format has an hour to count.
    pub hour_cycle: Option<HourCycle>,
    /// [[DateStyle]] and [[TimeStyle]] (steps 27 and 28).
    pub date_style: Option<Style>,
    pub time_style: Option<Style>,
    /// The `fractionalSecondDigits` of Table 16, reported as the number it was read as.
    pub fractional_second_digits: Option<u8>,
    /// [[DateTimeFormat]] (step 32), reduced to the key of the renderer that answers for it.
    pub key: FieldSetKey,
    /// The components of Table 16 as step 24 read them out of the bag, before step 31.6 filled any
    /// defaults in and before the two shorthands replaced them whole. 15.6.1's `GetDateTimeFormat`
    /// derives the format of each Temporal type from *this* rather than from [[DateTimeFormat]],
    /// because the `defaults` and `inherit` it is called with differ per type.
    components: Components,
}

/// The component rows of 11.3.2's Table 15, in the order the table has them.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolvedWidths {
    pub weekday: Option<Width>,
    pub era: Option<Width>,
    pub year: Option<Width>,
    pub month: Option<Width>,
    pub day: Option<Width>,
    pub day_period: Option<Width>,
    pub hour: Option<Width>,
    pub minute: Option<Width>,
    pub second: Option<Width>,
    pub time_zone_name: Option<TimeZoneNameStyle>,
}

/// The nine string components of Table 16 in the order step 24 reads them, which is also the order of
/// the rows of Table 15 that `resolvedOptions` walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Component {
    Weekday,
    Era,
    Year,
    Month,
    Day,
    DayPeriod,
    Hour,
    Minute,
    Second,
}

impl Component {
    const ALL: [Component; 9] = [
        Component::Weekday,
        Component::Era,
        Component::Year,
        Component::Month,
        Component::Day,
        Component::DayPeriod,
        Component::Hour,
        Component::Minute,
        Component::Second,
    ];

    /// The property the option is read from.
    fn name(self, cx: Context) -> Handle<PropertyKey> {
        match self {
            Component::Weekday => cx.names.weekday(),
            Component::Era => cx.names.era(),
            Component::Year => cx.names.year(),
            Component::Month => cx.names.month(),
            Component::Day => cx.names.day(),
            Component::DayPeriod => cx.names.day_period(),
            Component::Hour => cx.names.hour(),
            Component::Minute => cx.names.minute(),
            Component::Second => cx.names.second(),
        }
    }

    /// The Values column of the row of Table 16, which is what `GetOption` answers with and throws
    /// the RangeError outside of.
    fn values(self) -> &'static [&'static str] {
        match self {
            // The fields of a name: `narrow`, `short` and `long`, in the order the table lists them.
            Component::Weekday | Component::Era | Component::DayPeriod => {
                &["narrow", "short", "long"]
            }
            // The fields of a number: the two digit widths.
            Component::Year
            | Component::Day
            | Component::Hour
            | Component::Minute
            | Component::Second => &["2-digit", "numeric"],
            // The month is both, and is the only field whose row lists five values.
            Component::Month => &["2-digit", "numeric", "narrow", "short", "long"],
        }
    }

    fn set(self, components: &mut Components, width: Width) {
        match self {
            Component::Weekday => components.weekday = Some(width),
            Component::Era => components.era = Some(width),
            Component::Year => components.year = Some(width),
            Component::Month => components.month = Some(width),
            Component::Day => components.day = Some(width),
            Component::DayPeriod => components.day_period = Some(width),
            Component::Hour => components.hour = Some(width),
            Component::Minute => components.minute = Some(width),
            Component::Second => components.second = Some(width),
        }
    }

    /// The width 11.3.2 reports for this component: what the rendering of the matched pattern shows
    /// (the `2-digit` hour of a Danish `{ hour: "numeric" }`, which is the answer the specification
    /// reads out of `[[DateTimeFormat]]` rather than out of the bag) or, where the rendering carries
    /// no digits to count, what was asked for.
    fn reported(
        self,
        widths: &super::date_time_format_core::RenderedWidths,
        asked: Option<Width>,
    ) -> Option<Width> {
        let rendered = match self {
            Component::Year => widths.year,
            Component::Month => widths.month,
            Component::Day => widths.day,
            Component::Hour => widths.hour,
            Component::Minute => widths.minute,
            Component::Second => widths.second,
            // The named fields are not probed: telling `Feb` from `February` is a question for the
            // name tables rather than for the rendering, so these keep the width of the bag.
            _ => None,
        };

        asked.map(|width| rendered.unwrap_or(width))
    }
}

impl DateTimeFormatObject {
    /// 11.1.2 steps 1 and 2: `OrdinaryCreateFromConstructor(newTarget,
    /// "%Intl.DateTimeFormat.prototype%", ‹ the slots of 11.4 ›)`, every one of them empty. The
    /// options are read afterwards, by [`Self::initialize`].
    pub fn new(cx: Context, new_target: Handle<ObjectValue>) -> EvalResult<HeapPtr<Self>> {
        // Step 2, through `ObjectBuilder`, which reads `newTarget.prototype` and falls back to the
        // intrinsic the spec names: OrdinaryCreateFromConstructor steps 1 and 2.
        let mut date_time_format = ObjectBuilder::<Self>::new(cx)
            .constructor_proto(new_target, Intrinsic::DateTimeFormatPrototype)?
            .build()?;

        // The slots are empty as step 2 leaves them, and stay so until `initialize` writes the whole
        // record in one go. `[[BoundFormat]]` starts as undefined rather than absent, which is what
        // 11.3.3 step 1 tests.
        date_time_format.resolved = None;
        date_time_format.bound_format = Value::undefined();

        Ok(date_time_format)
    }

    /// 11.5.13 UnwrapDateTimeFormat (https://tc39.es/ecma402/#sec-unwrapdatetimeformat), which is
    /// the first step of 11.3.3 to 11.3.6.
    pub fn unwrap(cx: Context, date_time_format: Handle<Value>) -> EvalResult<Handle<Value>> {
        // Step 1.
        if !date_time_format.is_object() {
            return type_error(cx, "Intl.DateTimeFormat method called on incompatible receiver");
        }

        let object = date_time_format.as_object();

        // Step 2: a value that is a DateTimeFormat by its prototype chain but was never initialized
        // is the `this` of the legacy call mode of 4.3 Note 1, and the instance 11.1.1 left under
        // %Intl% is reached through [[FallbackSymbol]].
        let initialized = object
            .as_opt::<Self>()
            .is_some_and(|instance| instance.resolved.is_some());

        if !initialized
            && ordinary_has_instance(
                cx,
                cx.get_intrinsic(Intrinsic::DateTimeFormatConstructor)
                    .into(),
                date_time_format,
            )?
        {
            // Step 2.1.
            return get(cx, object, cx.symbols.intl_fallback_symbol());
        }

        // Step 3.
        Ok(date_time_format)
    }

    /// The unwrap followed by the `[[InitializedDateTimeFormat]]` test of 11.3.2 step 3 and 11.3.3
    /// step 3, which is what every method that reads the instance performs on its result.
    pub fn unwrap_initialized(
        cx: Context,
        date_time_format: Handle<Value>,
    ) -> EvalResult<Handle<Self>> {
        let date_time_format = Self::unwrap(cx, date_time_format)?;

        match date_time_format.as_opt::<Self>() {
            Some(instance) if instance.resolved.is_some() => Ok(instance),
            // The step after the unwrap, for a value that is not a DateTimeFormat at all, which the
            // unwrap's step 1 already refused, or is one that was never initialized.
            _ => type_error(cx, "Intl.DateTimeFormat method called on an uninitialized instance"),
        }
    }

    /// The `RequireInternalSlot(this, [[InitializedDateTimeFormat]])` of 11.3.4 step 2, 11.3.5 step 2
    /// and 11.3.6 step 2, on the value itself: those three are the methods of 11.3 that do not begin
    /// with UnwrapDateTimeFormat, so an object that only carries an instance under
    /// %Intl%. [[FallbackSymbol]] is not a receiver they accept.
    pub fn require_initialized(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(cx, "Intl.DateTimeFormat method called on incompatible receiver");
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(instance) if instance.resolved.is_some() => Ok(instance),
            _ => type_error(cx, "Intl.DateTimeFormat method called on an uninitialized instance"),
        }
    }

    /// The resolved options of an initialized instance, which is what 11.3.2 walks row by row and
    /// what the rendering operations read.
    ///
    /// The record comes out by value rather than as a borrow of the block on purpose: every reader
    /// of it formats, and formatting allocates. A `&ResolvedDateTimeFormat` taken before the first of
    /// those allocations is a reference into the copy the collector has already moved away from.
    pub fn resolved(&self) -> Option<ResolvedDateTimeFormat> {
        self.resolved
    }

    /// [[BoundFormat]], 11.3.3 steps 1 and 6.
    pub fn bound_format(&self) -> Value {
        self.bound_format
    }

    /// 11.3.3 step 5: `Set(dateTimeFormat.[[BoundFormat]], boundFormat)`.
    pub fn set_bound_format(&mut self, bound_format: Value) {
        self.bound_format = bound_format;
    }

    /// 11.5.6 FormatDateTime (https://tc39.es/ecma402/#sec-formatdatetime) for this instance, which
    /// is what 11.3.6's `formatToParts` returns and 11.3.3's `format` joins into a string, of either
    /// of 15.6.11's two answers.
    ///
    /// A time value is the 11.5.6 this engine has always run. A Temporal source is 15.6.5's
    /// `PartitionDateTimePattern`: the same partition, rendered from the civil fields the argument
    /// itself names - through the slot of 11.1.2 step 31 for its type, at the offset of 15.6.4's
    /// `[[IsPlain]]` - rather than from a number of milliseconds.
    pub fn format(&self, cx: Context, x: &Formattable) -> EvalResult<Vec<DateTimePart>> {
        let Some(resolved) = self.resolved() else {
            return type_error(
                cx,
                "Intl.DateTimeFormat method called on an uninitialized instance",
            );
        };

        let (renderer, moment) = self.moment(cx, &resolved, x)?;

        Ok(match moment {
            Rendered::TimeValue(milliseconds, zone) => renderer.format(milliseconds, &zone),
            Rendered::Nanoseconds(nanoseconds, zone) => {
                renderer.format_nanoseconds(nanoseconds, &zone)
            }
        })
    }

    /// 11.5.6 FormatDateTime again, this time as the String that 11.3.3's `format` and 20.4.1 to
    /// 20.4.3 of `Date.prototype` return: the concatenation of the values of the partition, with no
    /// record made for either.
    pub fn format_to_string(&self, cx: Context, x: &Formattable) -> EvalResult<String> {
        let Some(resolved) = self.resolved() else {
            return type_error(
                cx,
                "Intl.DateTimeFormat method called on an uninitialized instance",
            );
        };

        let (renderer, moment) = self.moment(cx, &resolved, x)?;

        Ok(match moment {
            Rendered::TimeValue(milliseconds, zone) => {
                renderer.format_to_string(milliseconds, &zone)
            }
            Rendered::Nanoseconds(nanoseconds, zone) => {
                renderer.format_nanoseconds_to_string(nanoseconds, &zone)
            }
        })
    }

    /// The format and the moment one argument of 15.3 renders with, which is 11.5.6's steps 1 to 5 of
    /// a time value and 15.6.14's dispatch plus 15.6.4's `ToLocalTime` of a Temporal source.
    fn moment(
        &self,
        cx: Context,
        resolved: &ResolvedDateTimeFormat,
        x: &Formattable,
    ) -> EvalResult<(Rc<DateTimeRenderer>, Rendered)> {
        match *x {
            Formattable::TimeValue(number) => {
                // Steps 1 to 3: the time value, clipped, and the `Invalid Date` that is a RangeError here.
                let moment = clip_time_value(cx, number)?;

                // Steps 4 and 5: the moment made civil in the zone the format resolved to, whose offset is
                // the one that applies at this moment rather than the one construction happened to look up.
                let renderer = self.renderer(cx, resolved)?;
                let zone = zone_at(cx, resolved, &renderer, moment)?;

                Ok((renderer, Rendered::TimeValue(moment, zone)))
            }
            Formattable::Temporal(source) => {
                // 15.6.15 to 15.6.19, in the order each of them has its steps in: the calendar first,
                // because the RangeError of a calendar this format has no patterns for comes before the
                // TypeError of a slot that was never made, and the slot then names the renderer.
                check_temporal_calendar(cx, resolved, &source)?;

                let renderer = self.temporal_renderer(cx, resolved, source.kind)?;
                let zone = temporal_zone_at(cx, resolved, &renderer, &source)?;

                Ok((renderer, Rendered::Nanoseconds(source.epoch_nanoseconds, zone)))
            }
            // 15.6.22's last step, which is the end of the dispatch rather than of 15.6.11: a zoned
            // date time has no handler, its zone being neither the `+00:00` of 15.6.4's `[[IsPlain]]`
            // nor the `[[TimeZone]]` the format resolved to.
            Formattable::ZonedDateTime => type_error(cx, ZONED_DATETIME_FAILURE),
        }
    }

    /// 11.5.9 FormatDateTimeRange (https://tc39.es/ecma402/#sec-formatdatetimerange), which is the
    /// concatenation of 11.5.8's parts and so renders through the range pattern CLDR chose for the
    /// difference between the two ends.
    pub fn format_range(
        &self,
        cx: Context,
        x: &Formattable,
        y: &Formattable,
    ) -> EvalResult<String> {
        match self.range_ends(cx, x, y)? {
            Ends::TimeValues(renderer, start, start_zone, end, end_zone) => {
                match renderer.format_range(start, &start_zone, end, &end_zone) {
                    Some(text) => Ok(text),
                    None => range_error(cx, RANGE_FAILURE),
                }
            }
            Ends::Nanoseconds(renderer, start, start_zone, end, end_zone) => {
                match renderer.format_range_nanoseconds(start, &start_zone, end, &end_zone) {
                    Some(text) => Ok(text),
                    None => range_error(cx, RANGE_FAILURE),
                }
            }
        }
    }

    /// 11.5.8 PartitionDateTimeRangePattern (https://tc39.es/ecma402/#sec-partitiondatetimerangepattern),
    /// which is what `formatRangeToParts` of 11.3.5 answers with: the same partition 11.5.9 joins,
    /// kept as the records of 11.3.5's array.
    pub fn format_range_parts(
        &self,
        cx: Context,
        x: &Formattable,
        y: &Formattable,
    ) -> EvalResult<Vec<DateTimePart>> {
        match self.range_ends(cx, x, y)? {
            Ends::TimeValues(renderer, start, start_zone, end, end_zone) => {
                match renderer.format_range_parts(start, &start_zone, end, &end_zone) {
                    Some(parts) => Ok(parts),
                    None => range_error(cx, RANGE_FAILURE),
                }
            }
            Ends::Nanoseconds(renderer, start, start_zone, end, end_zone) => {
                match renderer.format_range_nanoseconds_to_parts(start, &start_zone, end, &end_zone)
                {
                    Some(parts) => Ok(parts),
                    None => range_error(cx, RANGE_FAILURE),
                }
            }
        }
    }

    /// The shared steps of 11.5.9 and 11.5.10: the two clipped time values and the ordering check of
    /// 11.5.9's steps 1 to 3, and the rendering of each end in the offset its own moment had, which
    /// is what makes a range across a daylight-saving boundary show the two zone names it has rather
    /// than one of them twice.
    ///
    /// 15.6.8 puts the two ends through 15.6.14 one after the other, so the errors of the first come
    /// before the second is read; the same-type test of its step 3 belongs to the caller, which is the
    /// first place both arguments are in hand. That test is also what lets one format render both
    /// ends, which is the assert 15.6.8 makes about the two Value Format Records.
    fn range_ends(&self, cx: Context, x: &Formattable, y: &Formattable) -> EvalResult<Ends> {
        let Some(resolved) = self.resolved() else {
            return type_error(
                cx,
                "Intl.DateTimeFormat method called on an uninitialized instance",
            );
        };

        match (*x, *y) {
            (Formattable::TimeValue(start_number), Formattable::TimeValue(end_number)) => {
                // Steps 1 to 4: both values are clipped one after the other, so that the RangeError of
                // the first comes before the second is read. 11.5.8 asks for no ordering of the two
                // ends - the range pattern it selects is looked up by the difference of the civil
                // parts, and a negative difference is a difference like any other.
                let start = clip_time_value(cx, start_number)?;
                let end = clip_time_value(cx, end_number)?;

                let renderer = self.renderer(cx, &resolved)?;
                let start_zone = zone_at(cx, &resolved, &renderer, start)?;
                let end_zone = zone_at(cx, &resolved, &renderer, end)?;

                Ok(Ends::TimeValues(renderer, start, start_zone, end, end_zone))
            }
            (Formattable::Temporal(start), Formattable::Temporal(end)) => {
                // 15.6.8 step 3's `SameTemporalType`, which comes before either end is read: the two
                // ends of one range are one format's two moments, and the assert 15.6.8 makes about
                // the two Value Format Records is only true because this test was answered first.
                if start.kind != end.kind {
                    return type_error(
                        cx,
                        "Intl.DateTimeFormat formatRange needs two values of the same Temporal type",
                    );
                }

                check_temporal_calendar(cx, &resolved, &start)?;
                check_temporal_calendar(cx, &resolved, &end)?;

                let renderer = self.temporal_renderer(cx, &resolved, start.kind)?;
                let start_zone = temporal_zone_at(cx, &resolved, &renderer, &start)?;
                let end_zone = temporal_zone_at(cx, &resolved, &renderer, &end)?;

                Ok(Ends::Nanoseconds(
                    renderer,
                    start.epoch_nanoseconds,
                    start_zone,
                    end.epoch_nanoseconds,
                    end_zone,
                ))
            }
            (Formattable::ZonedDateTime, Formattable::ZonedDateTime) => {
                // 15.6.13 answers true of two of them, so 15.6.8 reaches the dispatch of 15.6.22 and
                // fails there instead.
                type_error(cx, ZONED_DATETIME_FAILURE)
            }
            _ => {
                // 15.6.8 step 3, which is the first place both arguments are in hand and so belongs to
                // the caller of this. Its answer is a TypeError rather than the RangeError a mismatched
                // calendar gets: the two ends are not two dates of one format, whatever each of them
                // would have rendered as on its own.
                type_error(
                    cx,
                    "Intl.DateTimeFormat formatRange needs two values of the same Temporal type",
                )
            }
        }
    }

    /// The rendering this instance formats through: the one its locale and its field set have, built
    /// on first use and shared by every instance of that pair after it.
    fn renderer(
        &self,
        cx: Context,
        resolved: &ResolvedDateTimeFormat,
    ) -> EvalResult<Rc<DateTimeRenderer>> {
        self.renderer_for(cx, resolved.data_locale, resolved.key)
    }

    /// The renderer of 15.6's slot for one Temporal type, which is the format 15.6.14's handler picks
    /// out of the instance and 15.6.15 to 15.6.18 refuse to do without.
    fn temporal_renderer(
        &self,
        cx: Context,
        resolved: &ResolvedDateTimeFormat,
        kind: TemporalKind,
    ) -> EvalResult<Rc<DateTimeRenderer>> {
        let Some(key) = temporal_key(resolved, kind) else {
            return type_error(
                cx,
                &format!(
                    "Intl.DateTimeFormat has no format for {}, because its options named a field of \
                     another group than that type has",
                    kind.name()
                ),
            );
        };

        self.renderer_for(cx, resolved.data_locale, key)
    }

    /// The lookup both of the two above end in.
    fn renderer_for(
        &self,
        cx: Context,
        data_locale: DataLocale,
        key: FieldSetKey,
    ) -> EvalResult<Rc<DateTimeRenderer>> {
        match cx.locale_cache().datetime_renderer(data_locale, key) {
            Ok(renderer) => Ok(renderer),
            // No program brings this about. The field set was built from the same options that made
            // this record, so the only way here is a build whose data is missing a marker the
            // formatter asks for.
            Err(error) => type_error(
                cx,
                &format!("Intl.DateTimeFormat has no locale data for its resolved format: {error}"),
            ),
        }
    }

    /// 11.1.2 CreateDateTimeFormat (https://tc39.es/ecma402/#sec-createdatetimeformat) steps 2 to
    /// 33, which is also what 11.3.2's `resolvedOptions` reads back. Steps 1 and 34 are the
    /// constructor's: the new instance needs `newTarget`, and the finished object is this
    /// function's caller's to return.
    ///
    /// The instance is a handle rather than `&mut self` on purpose: every step below allocates, and
    /// the block is only written at the end, through a deref taken after the last of those
    /// allocations. A `&mut` held across them would point at the block the collection moved away
    /// from, and the caller would hand back the address it had before.
    pub fn initialize(
        mut instance: Handle<Self>,
        mut cx: Context,
        locales: Handle<Value>,
        options: Handle<Value>,
        required: Required,
        defaults: Defaults,
    ) -> EvalResult<()> {
        // Step 4: ResolveOptions (https://tc39.es/ecma402/#sec-resolveoptions), 9.2.8 with
        // `coerce-options`. Its step 1 reads the locale list and its step 3 the options bag, in that
        // order, which is what makes a getter on the `locales` argument run before one on the
        // options.
        let requested_locales = canonicalize_locale_list_argument(cx, locales)?;
        let options = coerce_options_to_object(cx, options)?;

        // 9.2.8 step 4.
        let matcher = get_name_option(
            cx,
            options,
            cx.names.locale_matcher(),
            &["best fit", "lookup"],
            "best fit",
        )?;

        // 9.2.8 step 6, over 11.2.3's [[ResolutionOptionDescriptors]] - the three keys of
        // [[RelevantExtensionKeys]] each with the option of the same name - and its step 7's
        // `modifyResolutionOptions` between the second and the third: 11.1.2 step 3's closure reads
        // `hour12` there and blanks the `hc` of the record when it finds it, which is what makes the
        // Boolean win over the hour cycle named beside it.
        let calendar = get_extension_option(cx, options, cx.names.calendar())?;
        let numbering_system = get_extension_option(cx, options, cx.names.numbering_system())?;
        let hour12 = get_boolean_option(cx, options, cx.names.hour12())?;
        let hour_cycle = get_extension_option(cx, options, cx.names.hour_cycle())?;

        // 11.1.2 step 3.3, which is the `modifyResolutionOptions` of step 4: a `hour12` blanks the
        // record's "hc" over the value the `hourCycle` option had put in it, and the blank - the one
        // null among the values of this key's locale data (11.2.3) - is what 9.2.7 step 12.11 reads
        // to take a requested `-u-hc-` off the reported locale. The hour cycle is then asked of the
        // locale's own data, not of the request, as steps 14 to 16 do.
        let hc = if hour12.is_some() { None } else { hour_cycle };

        // Steps 6 to 11 through ResolveLocale (https://tc39.es/ecma402/#sec-resolvelocale), 9.2.7,
        // over the three keys. Each list of supported values is the one this build's data has, so a
        // `-u-ca-persian-` is dropped by step 12.7 rather than resolved into a calendar whose
        // patterns this build does not carry.
        let relevant_keys = [
            RelevantExtensionKey {
                key: "ca",
                supported_values: &["gregory"],
                option_value: calendar,
                option_is_null: false,
            },
            RelevantExtensionKey {
                key: "nu",
                supported_values: &["latn"],
                option_value: numbering_system,
                option_is_null: false,
            },
            RelevantExtensionKey {
                key: "hc",
                supported_values: &["h11", "h12", "h23", "h24"],
                option_value: hc.clone(),
                option_is_null: hour12.is_some(),
            },
        ];
        let resolved_locale = resolve_locale_with_keys(
            &requested_locales,
            if matcher == "lookup" {
                Matcher::Lookup
            } else {
                Matcher::BestFit
            },
            &relevant_keys,
        );

        // Steps 8 to 11. `[[NumberingSystem]]` is the constant the one list of values forces, and
        // [[Calendar]] is the one the one list for `ca` forces; both are reported because 11.3.2's
        // Table 15 has a row for each, and the resolution is still run because its steps 12.6 and
        // 12.11 are what decide whether a requested `-u-` keyword survives onto [[Locale]].
        let locale = cx.alloc_string(&resolved_locale.locale)?.as_value();
        let data_locale = data_locale(cx, &resolved_locale)?;
        let calendar = cx
            .alloc_string(&resolved_value(&resolved_locale, "ca", "gregory"))?
            .as_value();
        let numbering_system = cx
            .alloc_string(&resolved_value(&resolved_locale, "nu", "latn"))?
            .as_value();

        // Steps 12 to 16: the hour cycle the format counts by. A `hour12` wins, and what it wins to
        // is the 12- or the 24-hour cycle of the locale's own - `resolvedLocaleData` of steps 14 and
        // 15 is the data of the locale that was resolved to.
        //
        // What the request asked is read back from the record's "hc" above and from the `-u-hc-`
        // keyword step 12.6.3 accepted onto the tag - the two places the answer is spelled once the
        // Boolean has had its way - rather than from `values`, because 9.2.7 step 12.13 writes an
        // entry for every relevant key and, with no `hours` data in this payload to ask, falls back
        // to the first of its allowed values. That would answer "h11" for a locale nobody had asked
        // about, which is the bug the table below exists to avoid.
        let asked_hour_cycle = hc.as_deref().and_then(HourCycle::parse).or_else(|| {
            resolved_locale
                .extensions
                .iter()
                .find(|(key, _)| key == "hc")
                .and_then(|(_, value)| HourCycle::parse(value))
        });
        let locale_cycle = asked_hour_cycle
            // The locale's own answer, of the table first and of its rendering for a language the
            // table does not name.
            .or_else(|| hour_cycle_of_locale(data_locale))
            .or_else(|| (*instance).default_hour_cycle(cx, data_locale));
        let hour_cycle = match hour12 {
            None => locale_cycle,
            Some(twelve_hour) => {
                let base = locale_cycle.unwrap_or_else(|| {
                    (*instance)
                        .default_hour_cycle(cx, data_locale)
                        // A locale whose rendering has no hour to read an answer off is a build
                        // without its patterns, which the caller's own default answers.
                        .unwrap_or(HourCycle::H23)
                });

                Some(match (twelve_hour, base) {
                    (true, HourCycle::H11) => HourCycle::H11,
                    (true, _) => HourCycle::H12,
                    (false, HourCycle::H24) => HourCycle::H24,
                    (false, _) => HourCycle::H23,
                })
            }
        };

        // Steps 17 to 20: the zone. An absent `timeZone` is the host's own, which is what makes
        // `new Date().toLocaleString()` and `Intl.DateTimeFormat()` answer in the zone the machine's
        // clock is kept in rather than in UTC.
        let time_zone = match get_string_option(cx, options, cx.names.time_zone())? {
            Some(name) => match FormatZone::canonicalize(cx, &name) {
                Ok(zone) => zone,
                Err(message) => return range_error(cx, &message),
            },
            None => host_zone(cx),
        };
        // The name is what 11.3.2 reports and is kept as a JS string, the slot being `Copy` so that
        // the instance can hold it at all; the offset belongs to the moment and is filled in by
        // [`zone_at`] at each rendering.
        let time_zone_name = match time_zone.name(cx) {
            Ok(name) => cx.alloc_string(&name)?.as_value(),
            Err(message) => return range_error(cx, &message),
        };

        // Steps 21 to 24: Table 16, read in the order of its rows. `hasExplicitFormatComponents` is
        // step 24.5's answer to whether the bag said any one of them.
        let mut components = Components { hour_cycle, ..Components::default() };
        let mut has_explicit_format_components = false;

        for component in Component::ALL {
            // Step 24.3's `GetOption`, whose step 5 RangeError is raised by the helper.
            let Some(name) =
                get_optional_name_option(cx, options, component.name(cx), component.values())?
            else {
                continue;
            };

            let width = Width::parse(name).expect("the option list is Table 16's own values");
            component.set(&mut components, width);
            has_explicit_format_components = true;
        }

        // Step 24.2, the one row of the table whose values are numbers: `GetNumberOption` over 1 to
        // 3, which is what makes `fractionalSecondDigits: 4` the RangeError it is rather than a
        // silent three.
        if let Some(digits) =
            get_number_option(cx, options, cx.names.fractional_second_digits(), 1.0, 3.0, None)?
        {
            components.fractional_second_digits = Some(digits as u8);
            has_explicit_format_components = true;
        }

        // Step 24.3 again, for the zone group's own row.
        if let Some(name) = get_optional_name_option(
            cx,
            options,
            cx.names.time_zone_name(),
            &TimeZoneNameStyle::NAMES,
        )? {
            components.time_zone_name = Some(
                TimeZoneNameStyle::parse(name).expect("the option list is Table 16's own values"),
            );
            has_explicit_format_components = true;
        }

        // Step 25. What the option does is the RangeError of `GetOption` for a value outside the
        // list; the matcher itself distinguishes nothing here, which step 31.8 explains.
        let _format_matcher = get_name_option(
            cx,
            options,
            cx.names.format_matcher(),
            &["basic", "best fit"],
            "best fit",
        )?;

        // Steps 26 to 29, the two shorthands.
        let date_style =
            get_optional_name_option(cx, options, cx.names.date_style(), &STYLE_NAMES)?
                .and_then(Style::parse);
        let time_style =
            get_optional_name_option(cx, options, cx.names.time_style(), &STYLE_NAMES)?
                .and_then(Style::parse);

        components.date_style = date_style;
        components.time_style = time_style;

        // The bag as step 24 read it, which is what 15.6.1's per-type derivations start from: the
        // `components` below goes on to have step 31.6's defaults filled into it, and a derivation
        // that read the filled-in bag would ask 15.6.1's `anyPresent` a question it cannot answer
        // honestly.
        let asked = components;

        if date_style.is_some() || time_style.is_some() {
            // Step 30: a shorthand replaces whole groups of components, so a bag that named one of
            // them beside it is the TypeError of step 30.1, and a `timeStyle` where the caller needs
            // a date is the one of step 30.2.
            if has_explicit_format_components {
                return type_error(
                    cx,
                    "Intl.DateTimeFormat options cannot mix dateStyle or timeStyle with format components",
                );
            }

            if required == Required::Date && time_style.is_some() {
                return type_error(
                    cx,
                    "Intl.DateTimeFormat timeStyle is not allowed where a date format is required",
                );
            }

            if required == Required::Time && date_style.is_some() {
                return type_error(
                    cx,
                    "Intl.DateTimeFormat dateStyle is not allowed where a time format is required",
                );
            }
        } else {
            // Steps 31.1 to 31.5: whether the bag said anything about the groups the caller needs.
            // `timeZoneName` and `era` are not on the list, which is why `{ timeZoneName: "short" }`
            // and `{ era: "long" }` both still get the date the defaults give them.
            let explicit_date = components.weekday.is_some()
                || components.year.is_some()
                || components.month.is_some()
                || components.day.is_some();
            let explicit_time = components.day_period.is_some()
                || components.hour.is_some()
                || components.minute.is_some()
                || components.second.is_some()
                || components.fractional_second_digits.is_some();

            let need_defaults = match required {
                Required::Date => !explicit_date,
                Required::Time => !explicit_time,
                Required::Any => !(explicit_date || explicit_time),
            };

            if need_defaults {
                // Step 31.6, which is the bag `new Intl.DateTimeFormat()` is read with.
                if defaults == Defaults::Date || defaults == Defaults::All {
                    components.year = Some(Width::Numeric);
                    components.month = Some(Width::Numeric);
                    components.day = Some(Width::Numeric);
                }

                if defaults == Defaults::Time || defaults == Defaults::All {
                    components.hour = Some(Width::Numeric);
                    components.minute = Some(Width::Numeric);
                    components.second = Some(Width::Numeric);
                }
            }
        }

        // Step 31.8 asks `BasicFormatMatcher` to score the bag against the locale's whole list of
        // legacy patterns. ICU4X 2.3 publishes no such list - the CLDR skeleton is chosen while the
        // formatter is built, for the one pattern it ends up using - so there is nothing here for
        // the two matchers to disagree about, and both answer the skeleton. What step 25 validated
        // stands: a `formatMatcher` that is neither "basic" nor "best fit" is still the RangeError
        // `GetOption` throws.
        // An era is rendered beside the year, so a bag that asked for one and no year gets the year
        // anyway: `2/3/2021 Anno Domini` with `year: "numeric"` reported, which is what the engines
        // that have era patterns of their own answer.
        if components.era.is_some() && components.year.is_none() {
            components.year = Some(Width::Numeric);
        }

        // Steps 31.7 and 32: `BestFitFormatMatcher` is the CLDR skeleton this field set is, and the
        // renderer of it is what 11.5.4's table then formats with.
        let key = match FieldSetKey::for_components(&components) {
            Ok(key) => key,
            Err(error) => return unsupported_request(cx, error),
        };

        let renderer = match cx.locale_cache().datetime_renderer(data_locale, key) {
            Ok(renderer) => renderer,
            Err(error) => return unsupported_request(cx, error),
        };

        // Step 33, and Table 15 with it: the hour cycle is reported only for a format that has an
        // hour to count, and the widths are what the rendering of the matched pattern shows.
        let widths = ResolvedWidths {
            weekday: Component::Weekday.reported(&renderer.widths, components.weekday),
            era: Component::Era.reported(&renderer.widths, components.era),
            year: Component::Year.reported(&renderer.widths, components.year),
            month: Component::Month.reported(&renderer.widths, components.month),
            day: Component::Day.reported(&renderer.widths, components.day),
            day_period: Component::DayPeriod.reported(&renderer.widths, components.day_period),
            hour: Component::Hour.reported(&renderer.widths, components.hour),
            minute: Component::Minute.reported(&renderer.widths, components.minute),
            second: Component::Second.reported(&renderer.widths, components.second),
            time_zone_name: components.time_zone_name,
        };

        instance.resolved = Some(ResolvedDateTimeFormat {
            locale: *locale,
            data_locale,
            calendar: *calendar,
            numbering_system: *numbering_system,
            time_zone,
            time_zone_name: *time_zone_name,
            widths,
            // 11.1.2 step 21's [[HourCycle]] as the resolution of steps 12 to 16 settled it, for the
            // formats that have an hour to count and Table 15 has a row for. The rendering is asked
            // only for a locale neither the request nor [`hour_cycle_of_locale`]'s table named, which
            // is the case the measurement of the probe instants exists to cover.
            hour_cycle: if key.has_hour {
                key.hour_cycle.or(renderer.widths.hour_cycle)
            } else {
                None
            },
            date_style,
            time_style,
            fractional_second_digits: components.fractional_second_digits,
            components: asked,
            key,
        });

        Ok(())
    }

    /// The hour cycle a locale counts its hours by when nothing has said: the answer of the
    /// rendering of the format with nothing in it but an hour, which is what 11.1.2 step 13's
    /// `resolvedLocale.[[hc]]` is for a locale rather than a table of what the locales do.
    ///
    /// `None` is the rendering that has no hour to read an answer off, which is a build without the
    /// patterns of the locale rather than a locale that counts differently.
    fn default_hour_cycle(&self, cx: Context, data_locale: DataLocale) -> Option<HourCycle> {
        let key = FieldSetKey {
            date_fields: None,
            length: None,
            time_precision: Some(TimePrecisionKey::Hour),
            zone_style: None,
            year_style: None,
            two_digit_year: false,
            asked: AskedWidths::default(),
            narrow: NarrowWidths::default(),
            has_hour: true,
            hour_cycle: None,
        };

        let renderer = cx.locale_cache().datetime_renderer(data_locale, key).ok()?;

        renderer.widths.hour_cycle
    }
}

impl HeapItem for DateTimeFormatObject {
    fn byte_size(date_time_format: HeapPtr<Self>) -> usize {
        date_time_format.object_byte_size()
    }

    fn visit_pointers(mut date_time_format: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        date_time_format.visit_object_pointers(visitor);

        if let Some(resolved) = &mut date_time_format.resolved {
            visitor.visit_value(&mut resolved.locale);
            visitor.visit_value(&mut resolved.calendar);
            visitor.visit_value(&mut resolved.numbering_system);
            visitor.visit_value(&mut resolved.time_zone_name);
        }

        visitor.visit_value(&mut date_time_format.bound_format);
    }
}

/// The four values of the `dateStyle` and `timeStyle` rows of Table 17.
const STYLE_NAMES: [&str; 4] = ["full", "long", "medium", "short"];

/// 9.2.7 step 12.13's `result[[key]]` for one of the three keys, with the answer 11.2.3's
/// [[RelevantExtensionKeys]] leaves when the data says nothing.
fn resolved_value(resolved: &ResolvedLocale, key: &str, fallback: &str) -> String {
    resolved_value_opt(resolved, key).unwrap_or_else(|| fallback.to_owned())
}

fn resolved_value_opt(resolved: &ResolvedLocale, key: &str) -> Option<String> {
    resolved
        .values
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

/// What an option bag can ask for that no field set of this ICU4X generation renders, which is the
/// loud answer rather than a format that shows other fields.
fn unsupported_request<T>(cx: Context, error: FieldSetError) -> EvalResult<T> {
    range_error(cx, &format!("Intl.DateTimeFormat cannot render the requested format: {error}"))
}

/// GetOption (https://tc39.es/ecma402/#sec-getoption), 9.2.11, for an option whose absence is
/// nothing at all rather than a default: steps 1 to 4, step 5's RangeError, then step 7.
fn get_optional_name_option<'a>(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
    values: &'a [&'a str],
) -> EvalResult<Option<&'a str>> {
    let value = get(cx, options, property)?;

    if value.is_undefined() {
        // Step 7: the option was not said.
        return Ok(None);
    }

    // Step 4, which is the `ToString` that makes a `String` object and a number read the same way a
    // string does, and is also what a getter can be seen to run.
    let name = to_string(cx, value)?.to_wtf8_string()?;

    // Step 5, over the Values column of the row of Table 16 the caller is reading.
    let Some(found) = values
        .iter()
        .find(|allowed| name.as_str() == **allowed)
        .copied()
    else {
        let allowed = values.join(", ");

        return range_error(
            cx,
            &format!("Intl.DateTimeFormat option value must be one of: {allowed}"),
        );
    };

    Ok(Some(found))
}

/// GetOption for `hour12`, the one option of 11.1.2's bag that is a Boolean: step 3's closure reads
/// it as it comes and steps 14 and 15 compare it to `true` and `false`, which is what 9.2.11's step 3
/// `ToBoolean` answers with.
fn get_boolean_option(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
) -> EvalResult<Option<bool>> {
    let value = get(cx, options, property)?;

    Ok((!value.is_undefined()).then(|| to_boolean(*value)))
}

/// GetOption for the option whose absence is the system's own answer rather than nothing:
/// 11.1.2 step 17's `timeZone`.
fn get_string_option(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
) -> EvalResult<Option<String>> {
    let value = get(cx, options, property)?;

    if value.is_undefined() {
        return Ok(None);
    }

    let name = to_string(cx, value)?.to_wtf8_string()?;

    // A language tag and a zone id are both ASCII, so a value outside UTF-8 is a name no
    // canonicalization of step 19 would accept either, and is carried to step 19 to be refused there.
    Ok(Some(String::from_utf8_lossy(name.as_bytes()).into_owned()))
}

/// 9.2.8 step 6, for the three descriptors of 11.2.3's [[ResolutionOptionDescriptors]]: `GetOption`'s
/// read, then step 6.4.1's `ToString`, then step 6.4.2's RangeError for a value the `type` Unicode
/// locale nonterminal does not match. An absent option is `None`, which is what lets 9.2.7 step
/// 12.10 answer from the locale's own keyword instead.
fn get_extension_option(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
) -> EvalResult<Option<String>> {
    let value = get(cx, options, property)?;

    if value.is_undefined() {
        return Ok(None);
    }

    let name = to_string(cx, value)?.to_wtf8_string()?;

    let Ok(name) = std::str::from_utf8(name.as_bytes()) else {
        return range_error(cx, "Intl.DateTimeFormat option is not a Unicode extension value");
    };

    if !is_unicode_type_value(name) {
        return range_error(cx, "Intl.DateTimeFormat option is not a Unicode extension value");
    }

    Ok(Some(name.to_owned()))
}

/// Whether `value` is matched by the `type` Unicode locale nonterminal
/// (https://tc39.es/ecma402/#sec-unicode-locale-nonterminal), which 9.2.8 step 6.4.2 checks for a
/// value that will be looked up as a Unicode extension keyword: three to eight alphanumeric
/// characters, then any number of dashes each followed by three to eight of them.
///
/// The length test covers the ends by itself, because a leading, trailing or doubled dash makes one
/// of the pieces the split yields empty, and an empty piece is not three characters long.
fn is_unicode_type_value(value: &str) -> bool {
    value.split('-').all(|part| {
        (3..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

/// [[LocaleData]] (11.1.2 step 4's `resolvedLocale`) as the id of the baked data the format renders
/// with.
fn data_locale(cx: Context, resolved: &ResolvedLocale) -> EvalResult<DataLocale> {
    let Ok(data_locale) = DataLocale::try_from(resolved.data_locale.as_str()) else {
        return range_error(
            cx,
            &format!(
                "Intl.DateTimeFormat resolved the locale '{}' but this build has no data for it",
                resolved.data_locale
            ),
        );
    };

    Ok(data_locale)
}

/// 11.5.6 steps 1 and 2 and 11.5.8 steps 1 to 4: TimeClip
/// (https://tc39.es/ecma262/#sec-timeclip) of the number 15.6.11's `ToNumberToDateTimeFormattable`
/// read, of which this is the RangeError the formats of 11.3 give for an `Invalid Date` and for a
/// value outside the ±8.64e15 milliseconds a time value can hold.
///
/// The coercion is not here: 15.6.11 answers either that number or the Temporal object itself, so the
/// two branches of [`Formattable`] are what decides whether `ToNumber` runs at all.
fn clip_time_value(cx: Context, number: f64) -> EvalResult<f64> {
    let clipped = if !number.is_finite() || number.abs() > 8.64e15 {
        f64::NAN
    } else {
        // TimeClip's `ToIntegerOrInfinity`, which is a truncation toward zero: -0.9 milliseconds is
        // the millisecond 0, not the -1 that a floor would say, and TimeClip answers -0𝔽 as +0𝔽.
        let truncated = number.trunc();

        if truncated == 0.0 { 0.0 } else { truncated }
    };

    if clipped.is_nan() {
        return range_error(cx, "Invalid time value");
    }

    Ok(clipped)
}

/// The renderer of 15.6's slot for one Temporal type, with the moment it renders that type at: the
/// two halves of 15.6.5's `PartitionDateTimePattern`, which is 11.5.6's partition for a number and
/// 15.6.4's for a Temporal source.
///
/// The unit differs because the source differs: a time value is milliseconds and TimeClipped, and a
/// Temporal source is the epoch nanoseconds of its Value Format Record, which reach past both ends of
/// the range a `Date` holds and are finer than a millisecond.
enum Rendered {
    TimeValue(f64, FormatZone),
    Nanoseconds(i128, FormatZone),
}

/// [`Rendered`] for the two ends of 15.6.8's `PartitionDateTimeRangePattern`, which one renderer and
/// two zones: the format is shared because the same-type test passed, and the offsets are not because
/// each end names its own moment.
enum Ends {
    TimeValues(Rc<DateTimeRenderer>, f64, FormatZone, f64, FormatZone),
    Nanoseconds(Rc<DateTimeRenderer>, i128, FormatZone, i128, FormatZone),
}

/// The message for a range this build's renderer declines to partition, which 11.5.9 and 11.5.10 both
/// answer as a RangeError.
const RANGE_FAILURE: &str =
    "Intl.DateTimeFormat cannot render one of the dates of this range in this build";

/// The message of 15.6.22's last step, which is what a `Temporal.ZonedDateTime` answers with in either
/// of the two partitions.
const ZONED_DATETIME_FAILURE: &str =
    "Intl.DateTimeFormat cannot format a Temporal.ZonedDateTime directly";

/// The format of 15.6.1 for one Temporal type, as the key of the renderer that answers for it.
///
/// The two branches of 11.1.2 differ here the way they differ in construction. A shorthand is a whole
/// format that is then cut down to what one civil type can answer - 15.6.3's
/// `AdjustDateTimeStyleFormat`, which is [`FieldSetKey::kept_for_temporal`] - and a bag of components
/// is re-derived field by field, because the `required`, `defaults` and `inherit` of its
/// `GetDateTimeFormat` are different for each type.
///
/// `None` is 15.6.1's null, which 15.6.15 to 15.6.18 report as the TypeError of options that have no
/// overlap with the data model of the argument.
fn temporal_key(resolved: &ResolvedDateTimeFormat, kind: TemporalKind) -> Option<FieldSetKey> {
    if resolved.date_style.is_some() || resolved.time_style.is_some() {
        return resolved.key.kept_for_temporal(kind, resolved.time_style);
    }

    FieldSetKey::for_components(&temporal_components(&resolved.components, kind)?).ok()
}

/// The calendar test of 15.6.15 to 15.6.19, which those operations write as
/// `CalendarEquals(calendar, dateTimeFormat.[[Calendar]])` and which is a RangeError when it fails.
///
/// `iso8601` is accepted whatever `[[Calendar]]` says, and not only because 15.6's own tests construct
/// every case with `calendar: "iso8601"`: it is the identifier `temporal_rs` gives the calendar system
/// this build has patterns for, which that negotiation reports as `gregory` instead. The two name one
/// system, and a format that refused `iso8601` would refuse the only dates this engine can make.
/// Another identifier is still the disagreement the spec means, and is refused.
fn check_temporal_calendar(
    cx: Context,
    resolved: &ResolvedDateTimeFormat,
    source: &TemporalSource,
) -> EvalResult<()> {
    if source.calendar == "iso8601" {
        return Ok(());
    }

    // The slot is put in a handle before the read that may collect, which is the same reason
    // 11.3.2's `resolvedOptions` roots its four String slots before writing anything.
    let calendar = resolved.calendar.to_handle(cx);

    if calendar.as_string().flatten()?.eq_str(source.calendar) {
        return Ok(());
    }

    range_error(
        cx,
        &format!(
            "Intl.DateTimeFormat cannot format a {} of calendar {}, because its own calendar is a \
             different one",
            source.kind.name(),
            source.calendar
        ),
    )
}

/// 15.6.4's `timeZone` of 15.6.14's record, which the `[[IsPlain]]` of that record decides: the civil
/// types are made civil at the offset of `+00:00`, so that the fields the argument names are the
/// fields that render, and an instant is rendered in the zone 11.1.2 step 20 resolved.
fn temporal_zone_at(
    cx: Context,
    resolved: &ResolvedDateTimeFormat,
    renderer: &DateTimeRenderer,
    source: &TemporalSource,
) -> EvalResult<FormatZone> {
    if source.kind.is_plain() {
        // The offset alone is needed: a format derived for a civil type has no `timeZoneName` field,
        // so there is no name for an id to be looked up with.
        return Ok(FormatZone::from_offset_seconds(0));
    }

    let zone = match resolved
        .time_zone
        .with_offset_at_nanoseconds(cx, source.epoch_nanoseconds)
    {
        Ok(zone) => zone,
        Err(message) => return range_error(cx, &message),
    };

    if renderer.names_zone() && !zone.offset_is_renderable() {
        return range_error(
            cx,
            "Intl.DateTimeFormat cannot name an offset zone of more than ±18:00 in this build",
        );
    }

    Ok(zone)
}

/// [[TimeZone]] at one moment: the id the format resolved to, with the offset that id had then. This
/// is 11.5.6's step 4 and 11.5.11's `ToLocalTime` read together, and it is done per moment rather
/// than once at construction because a zone's offset is a property of the moment, not of the format.
///
/// The renderer is asked for one thing: a format with a `timeZoneName` field needs the zone handed to
/// ICU4X, which refuses an offset of more than ±18 h, although 11.5.3's grammar reaches ±23:59. The
/// civil parts of such a zone are right, so a format without that field is not refused.
fn zone_at(
    cx: Context,
    resolved: &ResolvedDateTimeFormat,
    renderer: &DateTimeRenderer,
    moment: f64,
) -> EvalResult<FormatZone> {
    let zone = match resolved.time_zone.with_offset_at(cx, moment) {
        Ok(zone) => zone,
        Err(message) => return range_error(cx, &message),
    };

    if renderer.names_zone() && !zone.offset_is_renderable() {
        return range_error(
            cx,
            "Intl.DateTimeFormat cannot name an offset zone of more than ±18:00 in this build",
        );
    }

    Ok(zone)
}
