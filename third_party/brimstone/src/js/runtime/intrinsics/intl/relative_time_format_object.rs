//! The `Intl.RelativeTimeFormat` instance
//! (https://tc39.es/ecma402/#sec-properties-of-intl-relativetimeformat-instances), 18.4, and
//! PartitionRelativeTimePattern
//! (https://tc39.es/ecma402/#sec-partitionrelativetimepattern), 18.5.2: the unit names, the
//! reading of the pattern data, and the one pass that turns a value and a unit into parts.
//!
//! The constructor and the methods a program calls are [`super::relative_time_format_constructor`]
//! and [`super::relative_time_format_prototype`].
//!
//! An instance holds what 18.4 lists and nothing else: the resolved locale, `style` and `numeric`.
//! The [[NumberFormat]] and [[PluralRules]] slots of 18.4 are the two pieces of machinery the
//! partition formats with - a default decimal rendering of the locale and its cardinal plural
//! rules - and neither is a value a program can reach, so they live in
//! [`super::cache::LocaleCache`] like every other service's, and the instance keeps only the
//! [`RelativeTimeSettings`] that name them. [[LocaleData]] and [[NumberingSystem]] are constants of
//! this build: the pattern tables the data crate baked and the one numbering system it has digits
//! for, which is why neither is stored either.
//!
//! The partition itself does not go through `icu_experimental`'s own `RelativeTimeFormatter`. Its
//! `Numeric::Auto` lookup reads the magnitude of the *rounded* value and would answer "today" for
//! -0.5 in a `{ numeric: "auto" }` formatter, where 18.5.2 step 7 keys the lookup on `ToString`
//! of the value as it was passed and only an integer can name a phrase; and its tense follows the
//! sign of the decimal it was handed, which cannot ask -0 for the past that step 8 requires. The
//! data is the same either way - the tables this module reads are the ones the formatter would
//! load - so the spec's own arithmetic runs directly over them instead, and every string it
//! produces is one the data wrote.

use brimstone_intl_data::IntlDataProvider;
use fixed_decimal::{Decimal, Sign};
use icu_experimental::relativetime::provider::{
    LongDayRelativeV1, LongHourRelativeV1, LongMinuteRelativeV1, LongMonthRelativeV1,
    LongQuarterRelativeV1, LongSecondRelativeV1, LongWeekRelativeV1, LongYearRelativeV1,
    NarrowDayRelativeV1, NarrowHourRelativeV1, NarrowMinuteRelativeV1, NarrowMonthRelativeV1,
    NarrowQuarterRelativeV1, NarrowSecondRelativeV1, NarrowWeekRelativeV1, NarrowYearRelativeV1,
    RelativeTimePatternData, ShortDayRelativeV1, ShortHourRelativeV1, ShortMinuteRelativeV1,
    ShortMonthRelativeV1, ShortQuarterRelativeV1, ShortSecondRelativeV1, ShortWeekRelativeV1,
    ShortYearRelativeV1,
};
use icu_locale::Locale as IcuLocale;
use icu_plurals::PluralOperands;
use icu_provider::marker::ErasedMarker;
use icu_provider::prelude::{
    DataError, DataIdentifierBorrowed, DataLocale as IcuDataLocale, DataPayload, DataProvider,
    DataRequest, DataResponse,
};
use writeable::{Part, PartsWrite, Writeable};

use super::cache::DataLocale;
use super::intl_object::canonicalize_locale_list_argument;
use super::locale_negotiation::{Matcher, RelevantExtensionKey, resolve_locale_with_keys};
use super::number_format_core::{
    NumberFormatSlots, RoundedNumber, UseGrouping, math_value_from_number, round_number,
};
use super::number_format_object::get_unicode_extension_option;
use super::options::{coerce_options_to_object, get_enum_option, get_name_option};
use super::plural_rules_object::{PluralRuleType, PluralRulesSettings};
use crate::extend_object;
use crate::runtime::{
    Context, Handle, Value,
    error::{range_error, type_error},
    eval_result::EvalResult,
    gc::{HeapItem, HeapPtr, HeapVisitor},
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ObjectBuilder,
    type_utilities::number_to_string,
};

/// One of the eight units 18.5.1 accepts, as `[[<unit>]]` names the pattern tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelativeTimeUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl RelativeTimeUnit {
    /// The eight names 18.5.1 step 9 accepts, in its order.
    pub const NAMES: [&'static str; 8] = [
        "second", "minute", "hour", "day", "week", "month", "quarter", "year",
    ];

    /// The `unit` String of the records 18.5.3 makes, and one of the two names the data crate
    /// spells the tables under: CLDR writes the day table of `LongDayRelativeV1` and the parts of
    /// 18.5.3 stamp `day` on them.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Second => "second",
            Self::Minute => "minute",
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Quarter => "quarter",
            Self::Year => "year",
        }
    }

    /// SingularRelativeTimeUnit (https://tc39.es/ecma402/#sec-singularrelativetimeunit), 18.5.1:
    /// steps 1 to 8 fold the plural names onto their singulars and step 9 refuses everything else,
    /// so this one function is the whole operation.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "second" | "seconds" => Some(Self::Second),
            "minute" | "minutes" => Some(Self::Minute),
            "hour" | "hours" => Some(Self::Hour),
            "day" | "days" => Some(Self::Day),
            "week" | "weeks" => Some(Self::Week),
            "month" | "months" => Some(Self::Month),
            "quarter" | "quarters" => Some(Self::Quarter),
            "year" | "years" => Some(Self::Year),
            _ => None,
        }
    }
}

/// [[Style]] (https://tc39.es/ecma402/#sec-Intl.RelativeTimeFormat), 18.1.1 step 11.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelativeTimeStyle {
    Long,
    Short,
    Narrow,
}

impl RelativeTimeStyle {
    /// The three names 18.1.1 step 10 lists, in that order.
    pub const NAMES: [&'static str; 3] = ["long", "short", "narrow"];

    /// The String value 18.3.2 reports for it, and the option value 18.1.1 step 10 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
            Self::Narrow => "narrow",
        }
    }

    /// 18.1.1 step 10 has already refused everything but the three names of this enum.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "long" => Some(Self::Long),
            "short" => Some(Self::Short),
            "narrow" => Some(Self::Narrow),
            _ => None,
        }
    }
}

/// [[Numeric]] (https://tc39.es/ecma402/#sec-Intl.RelativeTimeFormat), 18.1.1 step 13.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumericOption {
    Always,
    Auto,
}

impl NumericOption {
    /// The two names 18.1.1 step 12 lists, in that order.
    pub const NAMES: [&'static str; 2] = ["always", "auto"];

    /// The String value 18.3.2 reports for it, and the option value 18.1.1 step 12 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Auto => "auto",
        }
    }

    /// 18.1.1 step 12 has already refused everything but the two names of this enum.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "always" => Some(Self::Always),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// The `style` and `numeric` of 18.1.1 steps 11 and 13 with the resolved data locale of step 6,
/// as the `Copy` key the pattern tables of [[LocaleData]] are cached under and the settings the
/// partition formats from.
///
/// The data locale rather than the reported tag because the `-u-nu-latn` that tag may carry names
/// no locale the data has an entry for, and because 18.2.3's [[RelevantExtensionKeys]] is the
/// single key "nu" with the single value this build supports, so nothing an extension can say
/// changes which tables are read - the same reason every other entry of
/// [`super::cache::LocaleCache`] is keyed this way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RelativeTimeSettings {
    pub locale: DataLocale,
    pub style: RelativeTimeStyle,
    pub numeric: NumericOption,
}

/// The pattern tables one marker holds, in the erased form every one of the twenty-four markers
/// can be cast to: the phrases of the `numeric: "auto"` lookup, and the `past` and `future`
/// patterns that are keyed by plural category.
pub(crate) type RelativeTimePatterns = DataPayload<ErasedMarker<RelativeTimePatternData<'static>>>;

/// The [[LocaleData]][[<unit>]] tables of 18.4 for one built-in locale, `style` and unit, read
/// from the baked data by the marker they are stored under. `style` names the marker attribute and
/// the unit its base name, which is the same mapping 18.5.2 steps 4 to 6 make with their
/// `unit-style` keys.
///
/// The `DataError` a missing table is not something a program can bring about - the locale was
/// resolved from `BUILT_IN_LOCALES`, whose every entry has all twenty-four tables - so callers
/// turn it into the RangeError that names the build rather than the program as the cause.
pub(crate) fn build_relative_time_patterns(
    locale: DataLocale,
    style: RelativeTimeStyle,
    unit: RelativeTimeUnit,
) -> Result<RelativeTimePatterns, DataError> {
    // The tables are keyed by language, which is what the tags of `BUILT_IN_LOCALES` are.
    let Ok(icu_locale) = locale.tag().parse::<IcuLocale>() else {
        // The tags come out of `BUILT_IN_LOCALES`, which the build script wrote from subtags
        // `icu_locale` itself parsed.
        return Err(DataError::custom("a built-in locale tag that does not parse"));
    };

    let icu_data_locale = IcuDataLocale::from(&icu_locale);
    let request = DataRequest {
        id: DataIdentifierBorrowed::for_locale(&icu_data_locale),
        ..Default::default()
    };

    // One arm per marker, the way the twenty-four names of 18.5.2 steps 4 to 6 map onto the
    // markers of `icu/intl_markers.txt`: the unit names the base name and the style the attribute.
    // Each arm is its own concrete load because the baked provider implements one `DataProvider`
    // per marker rather than one for all of them.
    macro_rules! load {
        ($marker:ty) => {{
            let response: DataResponse<$marker> = IntlDataProvider.load(request)?;
            response.payload.cast()
        }};
    }

    Ok(match (unit, style) {
        (RelativeTimeUnit::Second, RelativeTimeStyle::Long) => load!(LongSecondRelativeV1),
        (RelativeTimeUnit::Second, RelativeTimeStyle::Short) => load!(ShortSecondRelativeV1),
        (RelativeTimeUnit::Second, RelativeTimeStyle::Narrow) => load!(NarrowSecondRelativeV1),
        (RelativeTimeUnit::Minute, RelativeTimeStyle::Long) => load!(LongMinuteRelativeV1),
        (RelativeTimeUnit::Minute, RelativeTimeStyle::Short) => load!(ShortMinuteRelativeV1),
        (RelativeTimeUnit::Minute, RelativeTimeStyle::Narrow) => load!(NarrowMinuteRelativeV1),
        (RelativeTimeUnit::Hour, RelativeTimeStyle::Long) => load!(LongHourRelativeV1),
        (RelativeTimeUnit::Hour, RelativeTimeStyle::Short) => load!(ShortHourRelativeV1),
        (RelativeTimeUnit::Hour, RelativeTimeStyle::Narrow) => load!(NarrowHourRelativeV1),
        (RelativeTimeUnit::Day, RelativeTimeStyle::Long) => load!(LongDayRelativeV1),
        (RelativeTimeUnit::Day, RelativeTimeStyle::Short) => load!(ShortDayRelativeV1),
        (RelativeTimeUnit::Day, RelativeTimeStyle::Narrow) => load!(NarrowDayRelativeV1),
        (RelativeTimeUnit::Week, RelativeTimeStyle::Long) => load!(LongWeekRelativeV1),
        (RelativeTimeUnit::Week, RelativeTimeStyle::Short) => load!(ShortWeekRelativeV1),
        (RelativeTimeUnit::Week, RelativeTimeStyle::Narrow) => load!(NarrowWeekRelativeV1),
        (RelativeTimeUnit::Month, RelativeTimeStyle::Long) => load!(LongMonthRelativeV1),
        (RelativeTimeUnit::Month, RelativeTimeStyle::Short) => load!(ShortMonthRelativeV1),
        (RelativeTimeUnit::Month, RelativeTimeStyle::Narrow) => load!(NarrowMonthRelativeV1),
        (RelativeTimeUnit::Quarter, RelativeTimeStyle::Long) => load!(LongQuarterRelativeV1),
        (RelativeTimeUnit::Quarter, RelativeTimeStyle::Short) => load!(ShortQuarterRelativeV1),
        (RelativeTimeUnit::Quarter, RelativeTimeStyle::Narrow) => load!(NarrowQuarterRelativeV1),
        (RelativeTimeUnit::Year, RelativeTimeStyle::Long) => load!(LongYearRelativeV1),
        (RelativeTimeUnit::Year, RelativeTimeStyle::Short) => load!(ShortYearRelativeV1),
        (RelativeTimeUnit::Year, RelativeTimeStyle::Narrow) => load!(NarrowYearRelativeV1),
    })
}

extend_object! {
    /// Properties of Intl.RelativeTimeFormat Instances
    /// (https://tc39.es/ecma402/#sec-properties-of-intl-relativetimeformat-instances), 18.4.
    ///
    /// Nothing here is an ICU4X object, which a GC object may not hold: the pattern tables, the
    /// decimal rendering and the plural rules are the cache's, and what an instance keeps is the
    /// [`RelativeTimeSettings`] that names them.
    pub struct RelativeTimeFormatObject {
        /// What 18.1.1 steps 6 to 13 computed. Its absence is this engine's spelling of
        /// `[[InitializedRelativeTimeFormat]]`, which 18.3.2 step 2, 18.3.3 step 2 and 18.3.4
        /// step 2 each require, and which a constructor that threw leaves unset.
        resolved: Option<ResolvedRelativeTimeFormat>,
    }
}

/// The slots of 18.4 this build keeps, carried with the options so that one `Option` on the
/// instance says whether it is initialized at all.
#[derive(Clone, Copy)]
pub struct ResolvedRelativeTimeFormat {
    /// [[Locale]] (18.1.1 step 7), which 18.3.2 reports as `locale`.
    pub locale: Value,

    /// The data locale of 9.2.7 step 5 with [[Style]] (step 11) and [[Numeric]] (step 13): the
    /// three things the pattern tables are cached under, and the last two of what 18.3.2 reports.
    ///
    /// [[NumberingSystem]] (step 9) is not in here because 18.2.3's [[RelevantExtensionKeys]] is
    /// the single key "nu" and the list of values this build supports for it is the single value
    /// "latn", so the resolved value is the constant that 18.3.2 reports.
    pub settings: RelativeTimeSettings,
}

impl ResolvedRelativeTimeFormat {
    /// The length of the pattern tables, which is what 18.3.2 reports as `style`.
    pub fn style(&self) -> RelativeTimeStyle {
        self.settings.style
    }

    /// Whether the phrases of step 7 are consulted, which is what 18.3.2 reports as `numeric`.
    pub fn numeric(&self) -> NumericOption {
        self.settings.numeric
    }
}

impl RelativeTimeFormatObject {
    /// 18.1.1 steps 1 and 2: `OrdinaryCreateFromConstructor(newTarget,
    /// "%Intl.RelativeTimeFormat.prototype%", « [[InitializedRelativeTimeFormat]], [[Locale]],
    /// [[LocaleData]], [[Style]], [[Numeric]], [[NumberFormat]], [[NumberingSystem]],
    /// [[PluralRules]] »)`, every one of them empty. Like ListFormat's and PluralRules' and unlike
    /// NumberFormat's and Collator's, this constructor has no legacy call mode: 18.1.1 step 1
    /// throws when NewTarget is undefined, so there is no %Intl%.[[FallbackSymbol]] instance to
    /// make and no ChainRelativeTimeFormat.
    pub fn new(cx: Context, new_target: Handle<ObjectValue>) -> EvalResult<HeapPtr<Self>> {
        // Step 2, through `ObjectBuilder`, which reads `newTarget.prototype` and falls back to the
        // intrinsic the spec names: OrdinaryCreateFromConstructor steps 1 and 2.
        let mut relative_time_format = ObjectBuilder::<Self>::new(cx)
            .constructor_proto(new_target, Intrinsic::RelativeTimeFormatPrototype)?
            .build()?;

        // The slots are empty as step 2 leaves them, and stay so until `initialize` writes the
        // whole record in one go.
        relative_time_format.resolved = None;

        Ok(relative_time_format)
    }

    /// `? RequireInternalSlot(rtf, [[InitializedRelativeTimeFormat]])` of 18.3.2 step 2, 18.3.3
    /// step 2 and 18.3.4 step 2, on the `this` value itself. There is no UnwrapRelativeTimeFormat
    /// in front of it: 18.1.1 step 1 refuses the only call mode that would have put an instance
    /// behind a fallback symbol.
    pub fn require_initialized(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(
                cx,
                "Intl.RelativeTimeFormat method called on incompatible receiver",
            );
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(instance) if instance.resolved.is_some() => Ok(instance),
            _ => {
                type_error(cx, "Intl.RelativeTimeFormat method called on an uninitialized instance")
            }
        }
    }

    /// The resolved options of an initialized instance: 18.3.2's `resolvedOptions` reads them
    /// through Table 33, and the partition below works from them.
    pub fn resolved(&self) -> Option<ResolvedRelativeTimeFormat> {
        self.resolved
    }

    /// 18.1.1 (https://tc39.es/ecma402/#sec-Intl.RelativeTimeFormat) steps 3 to 17. The instance
    /// is a handle rather than `&mut self` on purpose, as in every service: every step below
    /// allocates, and the record is only written at the end, through a deref taken after the last
    /// of those allocations. A `&mut` held across them would point at the block the collection
    /// moved away from, and the caller would hand back the address it had before.
    pub fn initialize(
        mut instance: Handle<Self>,
        mut cx: Context,
        locales: Handle<Value>,
        options: Handle<Value>,
    ) -> EvalResult<()> {
        // Steps 3 to 5: ResolveOptions (https://tc39.es/ecma402/#sec-resolveoptions), 9.2.8, with
        // the `coerce-options` behaviour 18.1.1 step 3 names, which ListFormat's call of it does
        // not: a number or a Boolean there is wrapped rather than refused. Its step 1 still reads
        // the locale list first, which is what makes a getter on `locales` run before one on the
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

        // 9.2.8 step 6, over 18.2.3's [[ResolutionOptionDescriptors]]: the one descriptor
        // RelativeTimeFormat has, the key "nu" read from the `numberingSystem` property.
        let numbering_system =
            get_unicode_extension_option(cx, options, cx.names.numbering_system())?;

        // Steps 6 to 9 through ResolveLocale (https://tc39.es/ecma402/#sec-resolvelocale), 9.2.7,
        // over 18.2.3's [[RelevantExtensionKeys]]. The list of supported values for "nu" is one
        // entry long because this build has one set of digits, so 9.2.7 step 12.7 refuses any
        // other value by dropping the locale that asked for it and step 4 answers with
        // DefaultLocale: `numberingSystem: "arab"` is a working English formatter rather than an
        // error, and [[Locale]] says "en" while it is at it.
        let relevant_keys = [RelevantExtensionKey {
            key: "nu",
            supported_values: &["latn"],
            option_value: numbering_system,
            // "nu" has no null among its locale data values, so the third state of step 12.9
            // belongs to a `DateTimeFormat`'s "hc" alone.
            option_is_null: false,
        }];
        let resolved = resolve_locale_with_keys(
            &requested_locales,
            if matcher == "lookup" {
                Matcher::Lookup
            } else {
                Matcher::BestFit
            },
            &relevant_keys,
        );

        // Steps 6 and 7.
        let locale = cx.alloc_string(&resolved.locale)?.as_value();

        // Steps 10 to 17 pick the tables by the built-in locale the resolved tag names, and
        // 18.2.3's [[AvailableLocales]] is exactly the set this build was generated with, so a tag
        // outside it means data is missing rather than that the caller can be answered in another
        // locale.
        let Ok(data_locale) = DataLocale::try_from(resolved.data_locale.as_str()) else {
            return range_error(
                cx,
                &format!(
                    "Intl.RelativeTimeFormat resolved the locale '{}' but this build has no data for it",
                    resolved.data_locale
                ),
            );
        };

        // Steps 10 and 11.
        let style = get_enum_option(
            cx,
            options,
            cx.names.style(),
            &RelativeTimeStyle::NAMES,
            "long",
            RelativeTimeStyle::from_name,
        )?;

        // Steps 12 and 13.
        let numeric = get_enum_option(
            cx,
            options,
            cx.names.numeric(),
            &NumericOption::NAMES,
            "always",
            NumericOption::from_name,
        )?;

        // Steps 14 to 17: [[NumberFormat]] is the default decimal rendering of the locale and
        // [[PluralRules]] its cardinal rules, both of which the partition reaches through
        // `LocaleCache` rather than through instances, since the two slots of 18.4 are not values
        // a program can read.
        instance.resolved = Some(ResolvedRelativeTimeFormat {
            locale: *locale,
            settings: RelativeTimeSettings { locale: data_locale, style, numeric },
        });

        Ok(())
    }

    /// PartitionRelativeTimePattern
    /// (https://tc39.es/ecma402/#sec-partitionrelativetimepattern), 18.5.2, over a value that
    /// 18.3.3 step 3 or 18.3.4 step 3 has already run through ToNumber and a unit that step 4 of
    /// either has already run through ToString.
    ///
    /// `self` is a borrow of a heap object that no step below reads a second time: the resolved
    /// record is `Copy` and is taken out first, so nothing held across the loads and allocations
    /// of the rest points into the heap.
    pub fn partition(
        &self,
        cx: Context,
        value: f64,
        unit: &str,
    ) -> EvalResult<Vec<RelativeTimePart>> {
        let Some(resolved) = self.resolved() else {
            // The question `require_initialized` has already answered.
            return type_error(
                cx,
                "Intl.RelativeTimeFormat method called on an uninitialized instance",
            );
        };

        // Step 1: NaN and the two infinities are the values no relative time has a name for.
        if value.is_nan() || value.is_infinite() {
            return range_error(cx, "A relative time value must be finite");
        }

        // Step 2: SingularRelativeTimeUnit (18.5.1), which folds the plural names and refuses the
        // rest.
        let Some(unit) = RelativeTimeUnit::from_name(unit) else {
            return range_error(
                cx,
                &format!(
                    "Invalid unit argument for Intl.RelativeTimeFormat: '{unit}' is not a singular relative time unit"
                ),
            );
        };

        // Steps 3 to 6: the tables of [[LocaleData]][[<unit>]], with the `unit-style` key of steps
        // 6.1 and 6.2 being the marker the data is baked under.
        let patterns = match cx.locale_cache().relative_time_patterns(
            resolved.settings.locale,
            resolved.settings.style,
            unit,
        ) {
            Ok(patterns) => patterns,
            Err(_) => {
                return range_error(
                    cx,
                    &format!(
                        "Intl.RelativeTimeFormat relative time patterns for '{}' are not supported in this build",
                        unit.as_str()
                    ),
                );
            }
        };

        // Step 7: `numeric: "auto"` looks the phrase `ToString(value)` names up, and only a phrase
        // that exists is used, as one literal part with no unit.
        if resolved.settings.numeric == NumericOption::Auto {
            if let Some(phrase) = relative_phrase_of(patterns.get(), value) {
                return Ok(vec![RelativeTimePart {
                    kind: RelativeTimePartKind::Literal,
                    value: phrase.to_owned(),
                    unit: None,
                }]);
            }
        }

        // Steps 8 and 9: past for -0 and everything below it, because `value.is_sign_negative()`
        // is the sign bit of the Number itself and `ToString` of -0 is "0", which step 7 has
        // already had its chance at.
        let past = value.is_sign_negative();

        // Step 11: the pieces PartitionNumberPattern(numberFormat, ℝ(value)) writes. The clause
        // spells the value itself rather than its absolute value, but test262 pins the absolute
        // one - `en-us-numeric-always.js` asks `format(-3, "hour")` for "3 hours ago", with no
        // minus to be seen and the sign of the value living in the tense of steps 8 and 9 alone -
        // and that is what every engine does, so the absolute value is what is formatted here.
        //
        // The digits come out of this build's own decimal slots at their defaults, which is what
        // an `Intl.NumberFormat(locale)` of step 16 resolves to: at most three fraction digits,
        // half-expanded, grouped.
        let absolute = value.abs();
        let rounded = round_number(
            &NumberFormatSlots::default(),
            &math_value_from_number(absolute, &number_to_string(absolute)),
        );
        let RoundedNumber::Digits(_, digits) = rounded else {
            // Step 1 has already refused the only values that round to a special.
            return range_error(cx, "A relative time value must be finite");
        };

        // Step 12: ResolvePlural (17.5.2) over the internal PluralRules of step 17. Its digit
        // slots are the defaults too - 17.1.1 step 13 hands SetNumberFormatDigitOptions the same
        // 0 and 3 that step 16's NumberFormat gets - so the rounding this category is read from is
        // the rounding the digits above went through, with the sign that no plural operand keeps
        // folded away the same way.
        let operands = PluralOperands::from(&digits);

        // Steps 10 and 13: the pattern of the tense and the category.
        let data = patterns.get();
        let rules = cx.locale_cache().plural_rules(PluralRulesSettings {
            locale: resolved.settings.locale,
            kind: PluralRuleType::Cardinal,
        });
        let pattern = if past { &data.past } else { &data.future };

        // Step 14: MakePartsList (18.5.3), which is the interpolation of the pattern with the
        // number written into it. The rendering is the default decimal rendering of the locale,
        // the one step 16's NumberFormat formats with, and the writeable pass it makes is what
        // carries the part annotations the sink below reads.
        let renderer = match cx
            .locale_cache()
            .decimal_renderer(resolved.settings.locale, UseGrouping::Auto)
        {
            Ok(renderer) => renderer,
            Err(_) => {
                return range_error(
                    cx,
                    "Intl.RelativeTimeFormat decimal formatting is not supported in this build",
                );
            }
        };

        // The digits are the absolute value, so the decimal carries no sign of its own for the
        // rendering to write an affix around.
        let mut number = Decimal::default();
        number.sign = Sign::None;
        number.absolute = digits;

        let mut sink = RelativeTimePartsSink { parts: Vec::new(), unit, annotated: None };

        // A sink only ever pushes onto a `Vec`, so the only thing that can come back is success;
        // a debug build checks that instead of an `unwrap` carrying a panic into a call. The pass
        // itself is outside the check on purpose: inside `debug_assert!` a release build would not
        // run it at all, and would produce no parts.
        let written = pattern
            .get(operands, rules.rules())
            .interpolate((renderer.decimal_formatter().format(&number),))
            .write_to_parts(&mut sink);
        debug_assert!(written.is_ok(), "writing into a sink of memory does not fail");

        Ok(sink.parts)
    }
}

impl HeapItem for RelativeTimeFormatObject {
    fn byte_size(relative_time_format: HeapPtr<Self>) -> usize {
        relative_time_format.object_byte_size()
    }

    fn visit_pointers(mut relative_time_format: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        relative_time_format.visit_object_pointers(visitor);

        if let Some(resolved) = &mut relative_time_format.resolved {
            visitor.visit_value(&mut resolved.locale);
        }
    }
}

/// 18.5.2 step 7's field lookup, over the phrases the data has.
///
/// The keys of `relatives` are the integers CLDR defines unique relative names for, and
/// `ToString` of a Number names one of them exactly when the Number is that integer: `ToString`
/// of an integral Number in the map's range is its digits, `ToString(-0)` is "0", and every other
/// Number - fractions, magnitudes past the map, the infinities step 1 has already refused - has a
/// `ToString` that is no field at all.
fn relative_phrase_of<'a>(data: &'a RelativeTimePatternData<'_>, value: f64) -> Option<&'a str> {
    if value.fract() == 0.0 && (i8::MIN as f64..=i8::MAX as f64).contains(&value) {
        data.relatives.get(&(value as i8))
    } else {
        None
    }
}

/// One of the part types the records of 18.5.2 can carry: `literal` for the pattern's own text
/// and the four pieces of a number for the placeholder's, which are the types
/// PartitionNumberPattern writes and 18.5.3 step 3.2.1 passes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelativeTimePartKind {
    Literal,
    Integer,
    Group,
    Decimal,
    Fraction,
}

impl RelativeTimePartKind {
    /// The `type` of the record 18.5.5 step 4.2 makes, which is the name the spec gives the slot.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Literal => "literal",
            Self::Integer => "integer",
            Self::Group => "group",
            Self::Decimal => "decimal",
            Self::Fraction => "fraction",
        }
    }
}

/// One part of a formatted relative time: 18.5.2's record, whose `[[Value]]` 18.5.5 step 4.3
/// makes into a String and whose `[[Unit]]` 18.5.5 step 4.4 makes a property only when it is
/// there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeTimePart {
    pub kind: RelativeTimePartKind,
    pub value: String,
    /// The unit of a piece of the number, which 18.5.3 step 3.2.1 stamps on every one of them;
    /// the literals of the pattern carry none.
    pub unit: Option<RelativeTimeUnit>,
}

/// The sink the interpolation of step 14 writes into: the literals of the pattern arrive as plain
/// text and the pieces of the number arrive through `icu_decimal`'s part annotations, which is
/// exactly the split MakePartsList (18.5.3) makes its records on.
struct RelativeTimePartsSink {
    parts: Vec<RelativeTimePart>,
    /// The unit of 18.5.3's arguments, stamped on every piece of the number.
    unit: RelativeTimeUnit,
    /// The annotation the text being written is inside, which is how `writeable` scopes its
    /// parts: the innermost annotation is the only one that says what the text is.
    annotated: Option<&'static str>,
}

impl PartsWrite for RelativeTimePartsSink {
    type SubPartsWrite = Self;

    fn with_part(
        &mut self,
        part: Part,
        mut f: impl FnMut(&mut Self::SubPartsWrite) -> std::fmt::Result,
    ) -> std::fmt::Result {
        let outer = self.annotated.replace(part.value);
        let result = f(self);
        self.annotated = outer;
        result
    }
}

impl std::fmt::Write for RelativeTimePartsSink {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        // The four number types are the annotations `icu_decimal` writes around digits; the sign
        // affixes of 16.5.2 never occur because the value written is the absolute one, so
        // anything else is a piece of the pattern's own text and is a literal.
        let (kind, unit) = match self.annotated {
            Some("integer") => (RelativeTimePartKind::Integer, Some(self.unit)),
            Some("group") => (RelativeTimePartKind::Group, Some(self.unit)),
            Some("decimal") => (RelativeTimePartKind::Decimal, Some(self.unit)),
            Some("fraction") => (RelativeTimePartKind::Fraction, Some(self.unit)),
            _ => (RelativeTimePartKind::Literal, None),
        };

        // Pieces of the same type are one part of 18.5.3's `parts` argument - the digits of a
        // number arrive here one annotation at a time, and `PartitionNumberPattern` reports one
        // integer part per run of digits between two separators, which is what
        // [`super::number_format_core::PartList`] merges its own parts on too. The empty writes a
        // rendering can produce are not parts at all.
        if !text.is_empty() {
            match self.parts.last_mut() {
                Some(last) if last.kind == kind && last.unit == unit => last.value.push_str(text),
                _ => self
                    .parts
                    .push(RelativeTimePart { kind, value: text.to_owned(), unit }),
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::number_format_core::DecimalRenderer;
    use super::*;

    /// The tables of 18.4 for one built-in locale, `style` and unit, loaded the way the cache
    /// loads them - the data level `partition` reaches without a heap, since a `Context` has no
    /// place in a unit test. The payload is handed back rather than the data it carries, because
    /// the borrowed `get()` does not outlive it.
    fn payload(
        locale: &str,
        style: RelativeTimeStyle,
        unit: RelativeTimeUnit,
    ) -> RelativeTimePatterns {
        build_relative_time_patterns(
            DataLocale::try_from(locale).expect("a built in locale"),
            style,
            unit,
        )
        .expect("every built in locale has every table")
    }

    /// Steps 8 to 14 for a value whose absolute part is an integer: the tense picks the pattern
    /// map, the digits and the operands both come from the one decimal, and the interpolation is
    /// cut by the same sink `partition` writes into.
    fn parts(
        locale: &str,
        style: RelativeTimeStyle,
        unit: RelativeTimeUnit,
        past: bool,
        number: i64,
    ) -> Vec<RelativeTimePart> {
        let patterns = payload(locale, style, unit);
        let data = patterns.get();

        let rules = PluralRulesSettings {
            locale: DataLocale::try_from(locale).expect("a built in locale"),
            kind: PluralRuleType::Cardinal,
        }
        .build_plural_rules();

        let mut decimal = Decimal::from(number);
        decimal.sign = Sign::None;
        let operands = PluralOperands::from(&decimal);

        let renderer = DecimalRenderer::try_new(locale, UseGrouping::Auto)
            .expect("every built in locale has a decimal format");

        let mut sink = RelativeTimePartsSink { parts: Vec::new(), unit, annotated: None };
        let pattern = if past { &data.past } else { &data.future };
        let written = pattern
            .get(operands, rules.rules())
            .interpolate((renderer.decimal_formatter().format(&decimal),))
            .write_to_parts(&mut sink);
        assert!(written.is_ok(), "writing into a sink of memory does not fail");

        sink.parts
    }

    fn format_relative_time(
        locale: &str,
        style: RelativeTimeStyle,
        unit: RelativeTimeUnit,
        past: bool,
        number: i64,
    ) -> String {
        parts(locale, style, unit, past, number)
            .iter()
            .map(|part| part.value.as_str())
            .collect()
    }

    fn kinds(parts: &[RelativeTimePart]) -> Vec<&'static str> {
        parts.iter().map(|part| part.kind.as_str()).collect()
    }

    fn part_units(parts: &[RelativeTimePart]) -> Vec<Option<&'static str>> {
        parts
            .iter()
            .map(|part| part.unit.map(RelativeTimeUnit::as_str))
            .collect()
    }

    #[test]
    fn every_built_in_locale_has_every_table() {
        for locale in brimstone_intl_data::BUILT_IN_LOCALES {
            for style in [
                RelativeTimeStyle::Long,
                RelativeTimeStyle::Short,
                RelativeTimeStyle::Narrow,
            ] {
                for unit in [
                    RelativeTimeUnit::Second,
                    RelativeTimeUnit::Minute,
                    RelativeTimeUnit::Hour,
                    RelativeTimeUnit::Day,
                    RelativeTimeUnit::Week,
                    RelativeTimeUnit::Month,
                    RelativeTimeUnit::Quarter,
                    RelativeTimeUnit::Year,
                ] {
                    let patterns = payload(locale, style, unit);
                    let data = patterns.get();

                    // 18.5.2 step 7's phrase table and step 10's two pattern maps are all there,
                    // and the phrase table has the 0 that `ToString(-0)` and `ToString(0)` read.
                    assert!(!data.relatives.is_empty(), "{locale} {style:?} {unit:?}");
                    assert!(data.relatives.get(&0).is_some(), "{locale} {style:?} {unit:?}");
                    assert!(
                        !format_relative_time(locale, style, unit, true, 3).is_empty(),
                        "{locale} {style:?} {unit:?}"
                    );
                    assert!(
                        !format_relative_time(locale, style, unit, false, 3).is_empty(),
                        "{locale} {style:?} {unit:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_day_phrases_are_the_data_ones() {
        let en = payload("en", RelativeTimeStyle::Long, RelativeTimeUnit::Day);
        let en = en.get();
        assert_eq!(en.relatives.get(&-1), Some("yesterday"));
        assert_eq!(en.relatives.get(&0), Some("today"));
        assert_eq!(en.relatives.get(&1), Some("tomorrow"));

        let da = payload("da", RelativeTimeStyle::Long, RelativeTimeUnit::Day);
        let da = da.get();
        assert_eq!(da.relatives.get(&-1), Some("i går"));
        assert_eq!(da.relatives.get(&0), Some("i dag"));
        assert_eq!(da.relatives.get(&1), Some("i morgen"));

        let it = payload("it", RelativeTimeStyle::Long, RelativeTimeUnit::Day);
        let it = it.get();
        assert_eq!(it.relatives.get(&-1), Some("ieri"));
        assert_eq!(it.relatives.get(&0), Some("oggi"));
        assert_eq!(it.relatives.get(&1), Some("domani"));
    }

    #[test]
    fn a_past_day_wraps_the_number_in_the_pattern_words() {
        // The placeholder is one piece 18.5.3 step 3.2 stamps with the unit, and the words around
        // it are literals of the pattern with no unit of their own.
        let en = parts("en", RelativeTimeStyle::Long, RelativeTimeUnit::Day, true, 3);
        assert_eq!(kinds(&en), ["integer", "literal"]);
        assert_eq!(
            en.iter()
                .map(|part| part.value.as_str())
                .collect::<Vec<_>>(),
            ["3", " days ago"]
        );
        assert_eq!(part_units(&en), [Some("day"), None]);

        let da = parts("da", RelativeTimeStyle::Long, RelativeTimeUnit::Day, true, 3);
        assert_eq!(kinds(&da), ["literal", "integer", "literal"]);
        assert_eq!(
            da.iter()
                .map(|part| part.value.as_str())
                .collect::<Vec<_>>(),
            ["for ", "3", " dage siden"]
        );

        let it = parts("it", RelativeTimeStyle::Long, RelativeTimeUnit::Day, true, 3);
        assert_eq!(kinds(&it), ["integer", "literal"]);
        assert_eq!(
            it.iter()
                .map(|part| part.value.as_str())
                .collect::<Vec<_>>(),
            ["3", " giorni fa"]
        );
    }

    #[test]
    fn the_future_patterns_use_their_own_words() {
        assert_eq!(
            format_relative_time("en", RelativeTimeStyle::Long, RelativeTimeUnit::Day, false, 3),
            "in 3 days"
        );
        assert_eq!(
            format_relative_time("da", RelativeTimeStyle::Long, RelativeTimeUnit::Day, false, 3),
            "om 3 dage"
        );
        assert_eq!(
            format_relative_time("it", RelativeTimeStyle::Long, RelativeTimeUnit::Day, false, 3),
            "tra 3 giorni"
        );
    }

    #[test]
    fn the_plural_category_picks_the_pattern() {
        // 18.5.2 step 12: the category of the rounded value decides between the `one` and `other`
        // patterns, so one of them names the unit in the singular and the other in the plural.
        assert_eq!(
            format_relative_time("en", RelativeTimeStyle::Long, RelativeTimeUnit::Minute, true, 1),
            "1 minute ago"
        );
        assert_eq!(
            format_relative_time("en", RelativeTimeStyle::Long, RelativeTimeUnit::Minute, true, 2),
            "2 minutes ago"
        );
        assert_eq!(
            format_relative_time("en", RelativeTimeStyle::Long, RelativeTimeUnit::Minute, true, 0),
            "0 minutes ago"
        );
        assert_eq!(
            format_relative_time(
                "en",
                RelativeTimeStyle::Short,
                RelativeTimeUnit::Quarter,
                true,
                3
            ),
            "3 qtrs. ago"
        );
    }

    #[test]
    fn the_phrase_lookup_is_the_tostring_of_the_value() {
        let en = payload("en", RelativeTimeStyle::Long, RelativeTimeUnit::Day);
        let en = en.get();

        // The integers whose names the data has are the ones `ToString` spells as digits, with
        // `ToString(-0)` spelling "0".
        assert_eq!(relative_phrase_of(en, 1.0), Some("tomorrow"));
        assert_eq!(relative_phrase_of(en, 0.0), Some("today"));
        assert_eq!(relative_phrase_of(en, -0.0), Some("today"));
        assert_eq!(relative_phrase_of(en, -1.0), Some("yesterday"));

        // A fraction's `ToString` has a point in it, a magnitude past the table's keys names no
        // field, and the NaN and infinities step 1 refuses never name one either.
        assert_eq!(relative_phrase_of(en, 1.5), None);
        assert_eq!(relative_phrase_of(en, 1000.0), None);
        assert_eq!(relative_phrase_of(en, f64::NAN), None);
        assert_eq!(relative_phrase_of(en, f64::INFINITY), None);
    }

    #[test]
    fn the_unit_names_fold_and_refuse() {
        for (name, unit) in [
            ("second", RelativeTimeUnit::Second),
            ("seconds", RelativeTimeUnit::Second),
            ("minute", RelativeTimeUnit::Minute),
            ("minutes", RelativeTimeUnit::Minute),
            ("hour", RelativeTimeUnit::Hour),
            ("hours", RelativeTimeUnit::Hour),
            ("day", RelativeTimeUnit::Day),
            ("days", RelativeTimeUnit::Day),
            ("week", RelativeTimeUnit::Week),
            ("weeks", RelativeTimeUnit::Week),
            ("month", RelativeTimeUnit::Month),
            ("months", RelativeTimeUnit::Month),
            ("quarter", RelativeTimeUnit::Quarter),
            ("quarters", RelativeTimeUnit::Quarter),
            ("year", RelativeTimeUnit::Year),
            ("years", RelativeTimeUnit::Year),
        ] {
            assert_eq!(RelativeTimeUnit::from_name(name), Some(unit), "{name}");
            assert_eq!(RelativeTimeUnit::from_name(unit.as_str()), Some(unit));
        }

        // The names of 18.5.1 are case sensitive and there is no tenth unit.
        assert_eq!(RelativeTimeUnit::from_name("SECOND"), None);
        assert_eq!(RelativeTimeUnit::from_name(""), None);
        assert_eq!(RelativeTimeUnit::from_name("decade"), None);
        assert_eq!(RelativeTimeUnit::from_name("fortnight"), None);
        assert_eq!(RelativeTimeUnit::from_name(" second"), None);
    }

    #[test]
    fn the_style_and_numeric_names_refuse_the_rest() {
        assert_eq!(RelativeTimeStyle::from_name("long"), Some(RelativeTimeStyle::Long));
        assert_eq!(RelativeTimeStyle::from_name("short"), Some(RelativeTimeStyle::Short));
        assert_eq!(RelativeTimeStyle::from_name("narrow"), Some(RelativeTimeStyle::Narrow));
        assert_eq!(RelativeTimeStyle::from_name("wide"), None);
        assert_eq!(RelativeTimeStyle::from_name("LONG"), None);

        assert_eq!(NumericOption::from_name("always"), Some(NumericOption::Always));
        assert_eq!(NumericOption::from_name("auto"), Some(NumericOption::Auto));
        assert_eq!(NumericOption::from_name("maybe"), None);
        assert_eq!(NumericOption::from_name("AUTO"), None);
    }
}
