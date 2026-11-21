//! The Intl.NumberFormat instance and the reading of the options that fill it.
//!
//! NumberFormat is split over three files the way ECMA-402 splits it: this one holds the instance
//! and the operations that work from it (16.1.1's option reading, 16.5.9's unwrap, 16.5.15's
//! mathematical value and 16.5.5's FormatNumeric),
//! [`super::number_format_constructor`] the constructor and `supportedLocalesOf` (16.1.1, 16.2.2),
//! and [`super::number_format_prototype`] the methods a program calls (16.3).
//!
//! An instance holds what 16.4 lists and nothing else: the resolved options, as
//! [`super::number_format_core::NumberFormatSlots`], its locale, and the function 16.3.3 caches. The
//! record is written once, at the end of construction, so no method can see an instance half read,
//! and its absence is this engine's spelling of `[[InitializedNumberFormat]]`.

use std::rc::Rc;

use icu_provider::DataError;

use super::cache::DataLocale;
use super::intl_object::canonicalize_locale_list_argument;
use super::locale_negotiation::{
    Matcher, RelevantExtensionKey, ResolvedLocale, resolve_locale_with_keys,
};
use super::number_format_core::{
    CompactDisplay, CurrencyCode, CurrencyDisplay, CurrencySign, DecimalRenderer, Increment,
    IntlMathValue, MAX_EXACT_DIGITS, Notation, NumberFormatSlots, NumberPart, RoundingMode,
    RoundingPriority, RoundingType, SignDisplay, Style, TrailingZeroDisplay, UseGrouping,
    math_value_from_bigint, math_value_from_number, math_value_from_string,
};
use super::number_format_currency::{CurrencyRenderer, CurrencySettings, currency_digits};
use super::options::{
    OptionType, coerce_options_to_object, default_number_option,
    get_boolean_or_string_number_format_option, get_enum_option, get_name_option,
    get_number_option, get_option,
};
use crate::extend_object;
use crate::runtime::{
    Context, Handle, PropertyKey, Value,
    abstract_operations::ordinary_has_instance,
    error::{range_error, type_error},
    eval_result::EvalResult,
    gc::{HeapItem, HeapPtr, HeapVisitor},
    get,
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ObjectBuilder,
    to_string,
    type_utilities::{ToPrimitivePreferredType, number_to_string, to_number, to_primitive},
};

extend_object! {
    /// Properties of Intl.NumberFormat Instances
    /// (https://tc39.es/ecma402/#sec-properties-of-intl-numberformat-instances), 16.4.
    ///
    /// There is no finalizer and nothing to release: the rendering a value is formatted with is not
    /// stored on the instance but taken from the per-context cache of [`super::cache::LocaleCache`],
    /// which is also why every instance of a locale, a `useGrouping` choice and a currency shares one.
    pub struct NumberFormatObject {
        /// What 16.1.1 steps 6 to 26 computed, and so the engine's spelling of
        /// `[[InitializedNumberFormat]]` together with the option slots behind it. A constructor
        /// that threw left this as `None`, which is what 16.5.9 step 2 looks for.
        resolved: Option<ResolvedNumberFormat>,
        /// [[BoundFormat]] (16.3.3 steps 4 to 6): undefined until the `format` getter builds the
        /// function, and that same function for every read after it.
        bound_format: Value,
    }
}

/// The rendering an instance formats through. 16.5.10 GetNumberFormatPattern answers a different
/// pattern for a currency than for the two styles `icu_decimal` holds, and the currency one is walked
/// by [`super::number_format_currency`] over the digits and affixes of the decimal one.
pub enum NumberRenderer {
    Decimal(Rc<DecimalRenderer>),
    Currency(Rc<CurrencyRenderer>),
}

impl NumberRenderer {
    fn format(&self, slots: &NumberFormatSlots, value: &IntlMathValue) -> String {
        match self {
            NumberRenderer::Decimal(renderer) => renderer.format(slots, value),
            NumberRenderer::Currency(renderer) => renderer.format(slots, value),
        }
    }

    fn format_to_parts(&self, slots: &NumberFormatSlots, value: &IntlMathValue) -> Vec<NumberPart> {
        match self {
            NumberRenderer::Decimal(renderer) => renderer.format_to_parts(slots, value),
            NumberRenderer::Currency(renderer) => renderer.format_to_parts(slots, value),
        }
    }
}

/// The slots 16.1.1 steps 6 to 8 resolves, carried with the options so that one `Option` on the
/// instance says whether it is initialized at all.
#[derive(Clone, Copy)]
pub struct ResolvedNumberFormat {
    /// [[Locale]] (16.1.1 step 6) as a String value, which is what 16.3.2 reports as `locale` and
    /// the only string an instance keeps.
    pub locale: Value,
    /// [[LocaleData]] (16.1.1 step 7), reduced to the id of the baked data 16.5.5's FormatNumeric
    /// renders with. This is 9.2.7 step 5's `dataLocale` rather than step 14's tag, because the
    /// `-u-nu-latn` that tag may carry names no locale the data has an entry for.
    pub data_locale: DataLocale,
    /// The option slots of 16.4 that follow the two above, of which this build has every one that
    /// does not name a currency or a unit.
    ///
    /// [[NumberingSystem]] is not in here because 16.2.3's [[RelevantExtensionKeys]] is the single
    /// key "nu" and the list of values this build supports for it is the single value "latn", so
    /// 9.2.7 step 12 resolves it to that whatever was asked and 16.3.2 can report the constant. The
    /// resolution is still run, because steps 12.6 and 12.11 are what decide whether the `-u-nu-`
    /// keyword of a requested tag survives onto [[Locale]].
    pub slots: NumberFormatSlots,
}

impl NumberFormatObject {
    /// 16.1.1 steps 1 and 2: `OrdinaryCreateFromConstructor(newTarget,
    /// "%Intl.NumberFormat.prototype%", ‹ the slots of 16.4 ›)`, every one of them empty. The
    /// options are read afterwards, by [`Self::initialize`].
    pub fn new(cx: Context, new_target: Handle<ObjectValue>) -> EvalResult<HeapPtr<Self>> {
        // Step 2, through `ObjectBuilder`, which reads `newTarget.prototype` and falls back to the
        // intrinsic the spec names: OrdinaryCreateFromConstructor steps 1 and 2.
        let mut number_format = ObjectBuilder::<Self>::new(cx)
            .constructor_proto(new_target, Intrinsic::NumberFormatPrototype)?
            .build()?;

        // The slots are empty as step 2 leaves them, and stay so until `initialize` writes the
        // whole record in one go. `[[BoundFormat]]` starts as undefined rather than absent, which
        // is what 16.3.3 step 1 tests.
        number_format.resolved = None;
        number_format.bound_format = Value::undefined();

        Ok(number_format)
    }

    /// 16.5.9 UnwrapNumberFormat (https://tc39.es/ecma402/#sec-unwrapnumberformat), which is the
    /// first step of every method of 16.3.
    pub fn unwrap(cx: Context, number_format: Handle<Value>) -> EvalResult<Handle<Value>> {
        // Step 1.
        if !number_format.is_object() {
            return type_error(cx, "Intl.NumberFormat method called on incompatible receiver");
        }

        let object = number_format.as_object();

        // Step 2: a value that is a NumberFormat by its prototype chain but was never initialized is
        // the `this` of the legacy call mode of 4.3 Note 1, and the instance 16.1.1.1 left under
        // %Intl%. [[FallbackSymbol]] is what the method has to work on.
        let initialized = object
            .as_opt::<Self>()
            .is_some_and(|instance| instance.resolved.is_some());

        if !initialized
            && ordinary_has_instance(
                cx,
                cx.get_intrinsic(Intrinsic::NumberFormatConstructor).into(),
                number_format,
            )?
        {
            // Step 2.1.
            return get(cx, object, cx.symbols.intl_fallback_symbol());
        }

        // Step 3.
        Ok(number_format)
    }

    /// 16.5.9 followed by the `[[InitializedNumberFormat]]` test that 16.3.2 step 3 and 16.3.3 step
    /// 3 each perform on its result.
    pub fn unwrap_initialized(
        cx: Context,
        number_format: Handle<Value>,
    ) -> EvalResult<Handle<Self>> {
        let number_format = Self::unwrap(cx, number_format)?;

        match number_format.as_opt::<Self>() {
            Some(instance) if instance.resolved.is_some() => Ok(instance),
            // The step after the unwrap, for a value that is not a NumberFormat at all, which the
            // unwrap's step 1 already refused, or is one that was never initialized.
            _ => type_error(cx, "Intl.NumberFormat method called on an uninitialized instance"),
        }
    }

    /// The `RequireInternalSlot(this, [[InitializedNumberFormat]])` of 16.3.6 step 1, on the value
    /// itself: 16.3.6 is the one method of 16.3 that does not begin with UnwrapNumberFormat, so an
    /// object that only carries an instance under %Intl%.[[FallbackSymbol]] is not the receiver it
    /// accepts.
    pub fn require_initialized(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(cx, "Intl.NumberFormat method called on incompatible receiver");
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(instance) if instance.resolved.is_some() => Ok(instance),
            _ => type_error(cx, "Intl.NumberFormat method called on an uninitialized instance"),
        }
    }

    /// The resolved options of an initialized instance: `resolvedOptions` reads them row by row
    /// through Table 28 (16.3.2 step 5), and 16.5.5's FormatNumeric and 16.5.6's
    /// FormatNumericToParts read them through UnwrapNumberFormat.
    pub fn resolved(&self) -> Option<ResolvedNumberFormat> {
        self.resolved
    }

    /// [[BoundFormat]], 16.3.3 steps 1 and 6.
    pub fn bound_format(&self) -> Value {
        self.bound_format
    }

    /// 16.3.3 step 5: `Set(numberFormat.[[BoundFormat]], boundFormat)`.
    pub fn set_bound_format(&mut self, bound_format: Value) {
        self.bound_format = bound_format;
    }

    /// 16.5.15 ToIntlMathematicalValue
    /// (https://tc39.es/ecma402/#sec-tointlmathematicalvalue).
    pub fn to_intl_mathematical_value(
        cx: Context,
        value: Handle<Value>,
    ) -> EvalResult<IntlMathValue> {
        // Step 1: ? ToPrimitive(x, number).
        let primitive = to_primitive(cx, value, ToPrimitivePreferredType::Number)?;

        if primitive.is_bigint() {
            // Step 2: ℝ of the BigInt, which is its decimal digits and reaches `fixed_decimal`
            // without going near a double.
            let digits = primitive.as_bigint().bigint().to_string();

            let Some(value) = math_value_from_bigint(&digits) else {
                // The cap of [`MAX_EXACT_DIGITS`], which `fixed_decimal`'s `i16` digit positions
                // force on a value this build formats exactly.
                return range_error(
                    cx,
                    &format!(
                        "Intl.NumberFormat cannot format a value of more than {MAX_EXACT_DIGITS} digits",
                    ),
                );
            };

            return Ok(value);
        }

        if primitive.is_string() {
            // Steps 3 and 5 to 10, over the String's own characters: the grammar of step 6 decides,
            // which is what makes `format("1,234")` refuse its argument rather than read the digits
            // it can. A language tag is ASCII, so the lossy conversion here only turns the lone
            // surrogates a String can hold into text that grammar refuses as well.
            let text = primitive.as_string().to_wtf8_string()?;

            return Ok(math_value_from_string(&String::from_utf8_lossy(text.as_bytes())));
        }

        // Step 4: ? ToNumber, which answers a Boolean, a Symbol and an unparseable string, then
        // steps 5 to 10 over the Number's shortest round-trip spelling. That spelling is
        // `Number::toString(x, 10)`, and it names exactly the double `x` is, so this path is exact
        // as well.
        let number = to_number(cx, primitive)?.as_number();

        Ok(math_value_from_number(number, &number_to_string(number)))
    }

    /// 16.5.5 FormatNumeric (https://tc39.es/ecma402/#sec-formatnumber) for this instance, which
    /// is where its resolved options and its locale's rendering meet. The rounding itself is
    /// 16.5.2's and lives in [`super::number_format_core`].
    pub fn format(&self, cx: Context, value: &IntlMathValue) -> EvalResult<String> {
        let Some(resolved) = self.resolved() else {
            return type_error(cx, "Intl.NumberFormat method called on an uninitialized instance");
        };

        let renderer = self.renderer(cx, &resolved)?;

        Ok(renderer.format(&resolved.slots, value))
    }

    /// The same as [`Self::format`], as the list of parts 16.3.6 reports.
    pub fn format_to_parts(
        &self,
        cx: Context,
        value: &IntlMathValue,
    ) -> EvalResult<Vec<NumberPart>> {
        let Some(resolved) = self.resolved() else {
            return type_error(cx, "Intl.NumberFormat method called on an uninitialized instance");
        };

        let renderer = self.renderer(cx, &resolved)?;

        Ok(renderer.format_to_parts(&resolved.slots, value))
    }

    /// The rendering this instance formats through: the currency one if 16.1.3 step 12 wrote a
    /// [[Currency]] and the decimal one otherwise, and in both cases the entry its locale and
    /// `useGrouping` choice have, built on first use and shared by every instance of that pair after
    /// it.
    fn renderer(&self, cx: Context, resolved: &ResolvedNumberFormat) -> EvalResult<NumberRenderer> {
        let cache = cx.locale_cache();
        let grouping = resolved.slots.use_grouping;

        let renderer = match CurrencySettings::of(&resolved.slots) {
            Some(settings) => cache
                .currency_renderer(resolved.data_locale, grouping, settings)
                .map(NumberRenderer::Currency),
            None => cache
                .decimal_renderer(resolved.data_locale, grouping)
                .map(NumberRenderer::Decimal),
        };

        locale_data(cx, renderer)
    }

    /// 16.1.1 (https://tc39.es/ecma402/#sec-initializenumberformat) steps 3 to 26, which is also
    /// what 16.3.2's `resolvedOptions` reads back. Steps 27 and 28, the legacy call mode, are the
    /// constructor's: they need the `this` value and NewTarget, which are not this function's
    /// business.
    /// The instance is a handle rather than `&mut self` on purpose: every step below allocates, and
    /// the block is only written at the end, through a deref taken after the last of those
    /// allocations. A `&mut` held across them would point at the block the collection moved away
    /// from, and the caller would hand back the address it had before.
    pub fn initialize(
        mut instance: Handle<Self>,
        mut cx: Context,
        locales: Handle<Value>,
        options: Handle<Value>,
    ) -> EvalResult<()> {
        // Steps 3 to 5: ResolveOptions (https://tc39.es/ecma402/#sec-resolveoptions), 9.2.8, with
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

        // 9.2.8 step 6, over 16.2.3's [[ResolutionOptionDescriptors]]: the one descriptor NumberFormat
        // has, the key "nu" read from the `numberingSystem` property.
        let numbering_system =
            get_unicode_extension_option(cx, options, cx.names.numbering_system())?;

        // Steps 6 to 8 through ResolveLocale (https://tc39.es/ecma402/#sec-resolvelocale), 9.2.7,
        // over 16.2.3's [[RelevantExtensionKeys]]. The list of supported values for "nu" is one
        // entry long because this build's data has one set of digits, so 9.2.7 step 12.7 refuses any
        // other value by dropping the locale that asked for it and step 4 answers with
        // DefaultLocale: `numberingSystem: "arab"` is a working English format rather than an error,
        // and [[Locale]] says "en" while it is at it.
        let relevant_keys = [RelevantExtensionKey {
            key: "nu",
            supported_values: &["latn"],
            option_value: numbering_system,
            // "nu" has no null among its locale data values (16.2.3), so the third state of step
            // 12.9 belongs to a `DateTimeFormat`'s "hc" alone.
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

        // Steps 6 and 7. Step 8, [[NumberingSystem]], is the constant the list forces, so it is not
        // kept: see [`ResolvedNumberFormat::slots`].
        let locale = cx.alloc_string(&resolved.locale)?.as_value();
        let data_locale = data_locale(cx, &resolved)?;

        let mut slots = NumberFormatSlots::default();

        // Step 9: SetNumberFormatUnitOptions (https://tc39.es/ecma402/#sec-setnumberformatunitoptions),
        // 16.1.3, which resolves the style and, for the currency style, the three currency options.
        set_number_format_unit_options(cx, options, &mut slots)?;

        // Steps 10 to 12: `notation`, of the four values step 11 lists. The two exponent notations
        // are still refused: they would print an exponent this build has no pattern data for, and
        // the exponent is not a piece this renderer can cut out of the number the way it cuts the
        // compact piece out.
        let notation = get_name_option(
            cx,
            options,
            cx.names.notation(),
            &["standard", "scientific", "engineering", "compact"],
            "standard",
        )?;

        let notation = match Notation::from_name(notation) {
            Some(notation) => notation,
            None => {
                return range_error(
                    cx,
                    &format!("NumberFormat notation '{notation}' is not supported in this build"),
                );
            }
        };

        slots.notation = notation;

        // Steps 13 to 15: the two defaults 16.1.2 is handed. A currency takes its own precision as
        // both of them - two digits for the dollar, none for the yen - but only for the standard
        // notation, which is why step 16 cannot be read before step 9 has resolved [[Currency]].
        let (min_fraction_digits_default, max_fraction_digits_default) = match slots.style {
            Style::Decimal | Style::Currency if notation != Notation::Standard => (0.0, 3.0),
            Style::Decimal => (0.0, 3.0),
            Style::Percent => (0.0, 0.0),
            Style::Currency => {
                // Step 13.1: [[Currency]], which 16.1.3 step 12.1 wrote and its step 4.1 made sure
                // this style has.
                let code = slots
                    .currency
                    .expect("the currency style resolves a currency");

                // Step 13.2: CurrencyDigits, from the locale-free table of `CurrencyFractionsV1`.
                let digits = locale_data(cx, currency_digits(code))?;

                (f64::from(digits), f64::from(digits))
            }
        };

        // Step 16: SetNumberFormatDigitOptions (https://tc39.es/ecma402/#sec-setnfdigitoptions),
        // 16.1.2.
        set_number_format_digit_options(
            cx,
            options,
            &mut slots,
            min_fraction_digits_default,
            max_fraction_digits_default,
            notation,
        )?;

        // Steps 17 to 24. Step 17's `compactDisplay` is read for every notation and only [[Notation]]
        // of step 19.1 keeps it, which is what makes `resolvedOptions` say nothing about it for the
        // standard notation. Steps 20 to 23 then replace the two strings of the older spelling and
        // turn a Boolean `true` into "always".
        let compact_display = get_enum_option(
            cx,
            options,
            cx.names.compact_display(),
            &CompactDisplay::NAMES,
            "short",
            CompactDisplay::from_name,
        )?;

        if notation == Notation::Compact {
            slots.compact_display = compact_display;
        }

        // Step 19.2: the compact notation groups a four-digit number whatever its style is.
        let default_use_grouping = if notation == Notation::Compact {
            "min2"
        } else {
            "auto"
        };

        let grouping = get_boolean_or_string_number_format_option(
            cx,
            options,
            cx.names.use_grouping(),
            &["min2", "auto", "always", "true", "false"],
            cx.alloc_static_string(default_use_grouping)?.as_value(),
        )?;

        slots.use_grouping = if grouping.is_bool() {
            // Step 23, and 9.2.12 step 4's answer for every falsy value: a Boolean.
            if grouping.as_bool() {
                UseGrouping::Always
            } else {
                UseGrouping::Never
            }
        } else {
            let name = grouping.as_string().flatten()?;

            if name.eq_str("always") {
                UseGrouping::Always
            } else if name.eq_str("min2") {
                UseGrouping::Min2
            } else {
                // "auto", and the "true" and "false" step 22 replaced with the default, which step
                // 19.2 made "min2" for the compact notation and left "auto" for the other.
                UseGrouping::Auto
            }
        };

        // Steps 25 and 26.
        slots.sign_display = get_enum_option(
            cx,
            options,
            cx.names.sign_display(),
            &SignDisplay::NAMES,
            "auto",
            SignDisplay::from_name,
        )?;

        instance.resolved = Some(ResolvedNumberFormat { locale: *locale, data_locale, slots });

        Ok(())
    }
}

impl HeapItem for NumberFormatObject {
    fn byte_size(number_format: HeapPtr<Self>) -> usize {
        number_format.object_byte_size()
    }

    fn visit_pointers(mut number_format: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        number_format.visit_object_pointers(visitor);

        if let Some(resolved) = &mut number_format.resolved {
            visitor.visit_value(&mut resolved.locale);
        }

        visitor.visit_value(&mut number_format.bound_format);
    }
}

/// The data every NumberFormat reads comes from the baked tables that are compiled into the binary,
/// so the only way a load fails is that `icu/generate_intl_data.sh` was run without a marker the code
/// asks for: a broken build, said as loudly as an options mistake is. No program brings it about,
/// because `resolve_locale_with_keys` hands back a tag out of `BUILT_IN_LOCALES` and `DataLocale` is
/// an index into that same list.
fn locale_data<T>(cx: Context, loaded: Result<T, DataError>) -> EvalResult<T> {
    match loaded {
        Ok(data) => Ok(data),
        Err(error) => type_error(
            cx,
            &format!("Intl.NumberFormat has no locale data for its resolved locale: {error}"),
        ),
    }
}

/// 16.1.3 SetNumberFormatUnitOptions (https://tc39.es/ecma402/#sec-setnumberformatunitoptions), the
/// unit-related options of an instance: the style, and for the currency style the three options that
/// say how its amount is spelled.
///
/// Steps 8 to 11, the unit and its display, are not read here: this build has no patterns for style
/// "unit" and says so at step 1's list, so the unit option can only ever be refused with it and
/// reading it would answer nothing.
fn set_number_format_unit_options(
    cx: Context,
    options: Handle<ObjectValue>,
    slots: &mut NumberFormatSlots,
) -> EvalResult<()> {
    // Steps 1 and 2, with all four values of step 1's list so that `style: "unit"` is refused for
    // being unsupported rather than for being a name GetOption does not know.
    let name = get_name_option(cx, options, cx.names.style(), &Style::NAMES, "decimal")?;
    let style = match Style::from_name(name) {
        Some(style) => style,
        None => {
            return range_error(
                cx,
                &format!("NumberFormat style '{name}' is not supported in this build"),
            );
        }
    };

    // Step 2.
    slots.style = style;

    // Steps 3 to 5. An absent `currency` stays undefined, and a present one is converted by step 3's
    // GetOption before step 5.1 reads it.
    let currency = get_option(
        cx,
        options,
        cx.names.currency(),
        OptionType::String,
        &[],
        Some(cx.undefined()),
    )?;

    // Steps 4 and 5, in that order: the style that needs the option says so when it is missing, and a
    // code that is there has to be well formed whatever the style is.
    let currency = if currency.is_undefined() {
        if style == Style::Currency {
            // Step 4.1.
            return type_error(
                cx,
                "Intl.NumberFormat option 'currency' is required with style 'currency'",
            );
        }

        None
    } else {
        // Step 5.1: IsWellFormedCurrencyCode, which is the three letters this build reads no further
        // than CLDR's own tables do. GetOption step 4 already made the value a String, and anything
        // that is not ASCII letters fails the parse the same way the well-formedness test does.
        let text = currency.as_string().to_wtf8_string()?;

        match CurrencyCode::parse(&String::from_utf8_lossy(text.as_bytes())) {
            Some(code) => Some(code),
            None => {
                return range_error(
                    cx,
                    "Intl.NumberFormat option 'currency' must be a three-letter ISO 4217 code",
                );
            }
        }
    };

    // Steps 6 and 7, read and checked for every style, and stored for one of them below.
    let currency_display = get_enum_option(
        cx,
        options,
        cx.names.currency_display(),
        &CurrencyDisplay::NAMES,
        "symbol",
        CurrencyDisplay::from_name,
    )?;

    let currency_sign = get_enum_option(
        cx,
        options,
        cx.names.currency_sign(),
        &CurrencySign::NAMES,
        "standard",
        CurrencySign::from_name,
    )?;

    // Steps 12.1 to 12.3: the three slots, and only for the style that formats with them. Step 13,
    // the two unit slots, belongs to the style this build refuses above.
    if style == Style::Currency {
        slots.currency = currency;
        slots.currency_display = currency_display;
        slots.currency_sign = currency_sign;
    }

    Ok(())
}

/// 9.2.8 step 6, for the one descriptor of 16.2.3's [[ResolutionOptionDescriptors]]: GetOption step
/// 1's read, then step 6.4.1's `ToString`, then step 6.4.2's RangeError when the string is not
/// matched by the `type` Unicode locale nonterminal. An absent option stays `None`, which is what
/// lets 9.2.7 step 12.10 answer from the locale's own keyword instead.
pub(crate) fn get_unicode_extension_option(
    cx: Context,
    options: Handle<ObjectValue>,
    property: Handle<PropertyKey>,
) -> EvalResult<Option<String>> {
    let value = get(cx, options, property)?;

    // GetOption step 2.1: absent is not said, and 9.2.8 step 6.4 is not reached for it.
    if value.is_undefined() {
        return Ok(None);
    }

    // Step 6.4.1.
    let text = to_string(cx, value)?.to_wtf8_string()?;

    // A language tag is ASCII, so anything outside UTF-8 is a request no canonicalization of
    // step 12.10.2 would accept either, and is refused here as the same RangeError.
    let Ok(name) = std::str::from_utf8(text.as_bytes()) else {
        return range_error(cx, "NumberFormat numberingSystem is not a Unicode extension value");
    };

    if !is_unicode_type_value(name) {
        return range_error(cx, "NumberFormat numberingSystem is not a Unicode extension value");
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

/// [[LocaleData]] (16.1.1 step 7) from the data locale 9.2.7 step 5 chose.
fn data_locale(cx: Context, resolved: &ResolvedLocale) -> EvalResult<DataLocale> {
    let Ok(data_locale) = DataLocale::try_from(resolved.data_locale.as_str()) else {
        return range_error(
            cx,
            &format!(
                "Intl.NumberFormat resolved the locale '{}' but this build has no data for it",
                resolved.data_locale
            ),
        );
    };

    Ok(data_locale)
}

/// 16.1.2 SetNumberFormatDigitOptions
/// (https://tc39.es/ecma402/#sec-setnfdigitoptions) over `options` into `slots`. Steps 1 to 10 are
/// the reads, in the order the spec lists them, because every one of them can be observed by a
/// getter on the options object; steps 11 to 28 are the arithmetic of what the four digit options
/// mean together, and after step 10 the operation touches nothing but those values.
pub(crate) fn set_number_format_digit_options(
    cx: Context,
    options: Handle<ObjectValue>,
    slots: &mut NumberFormatSlots,
    min_fraction_digits_default: f64,
    mut max_fraction_digits_default: f64,
    notation: Notation,
) -> EvalResult<()> {
    // Steps 1, 3 and 4: `minimumIntegerDigits` and the raw fraction-digit values.
    slots.minimum_integer_digits =
        get_number_option(cx, options, cx.names.minimum_integer_digits(), 1.0, 21.0, Some(1.0))?
            .unwrap_or(1.0) as u8;

    let min_fraction_digits = get(cx, options, cx.names.minimum_fraction_digits())?;
    let max_fraction_digits = get(cx, options, cx.names.maximum_fraction_digits())?;

    // Steps 5 and 6: the raw significant-digit values.
    let min_significant_digits = get(cx, options, cx.names.minimum_significant_digits())?;
    let max_significant_digits = get(cx, options, cx.names.maximum_significant_digits())?;

    // Steps 6 and 7 of the spec's numbering, the increment and its list.
    let increment =
        get_number_option(cx, options, cx.names.rounding_increment(), 1.0, 5000.0, Some(1.0))?
            .unwrap_or(1.0);

    let increment = match Increment::from_u32(increment as u32) {
        Some(increment) => increment,
        // 16.1.2 step 7's list of the increments restricted rounding is specified for.
        None => {
            return range_error(
                cx,
                &format!("NumberFormat roundingIncrement {increment} is not a supported increment"),
            );
        }
    };
    slots.rounding_increment = increment;

    // Steps 8 to 10.
    slots.rounding_mode = get_enum_option(
        cx,
        options,
        cx.names.rounding_mode(),
        &RoundingMode::NAMES,
        "halfExpand",
        RoundingMode::from_name,
    )?;

    let rounding_priority = get_enum_option(
        cx,
        options,
        cx.names.rounding_priority(),
        &RoundingPriority::NAMES,
        "auto",
        RoundingPriority::from_name,
    )?;

    slots.trailing_zero_display = get_enum_option(
        cx,
        options,
        cx.names.trailing_zero_display(),
        &TrailingZeroDisplay::NAMES,
        "auto",
        TrailingZeroDisplay::from_name,
    )?;

    // Step 12: an increment of anything but 1 cannot be met by a `maximumFractionDigits` that was
    // not said, so its default becomes the `minimum` rather than 3.
    if !increment.is_one() {
        max_fraction_digits_default = min_fraction_digits_default;
    }

    // Steps 17 and 18: which of the two lists the program said anything in. A property that is
    // absent and a property that is `undefined` are the same here, which is 9.2.13 step 1.
    let has_significant_digits =
        !min_significant_digits.is_undefined() || !max_significant_digits.is_undefined();
    let has_fraction_digits =
        !min_fraction_digits.is_undefined() || !max_fraction_digits.is_undefined();

    // Steps 19 and 20. The significant digits are needed when they were said, and the fraction
    // digits whenever they were not - except that step 19.3 takes the fraction digits away again for
    // a compact format nobody said anything about the precision of, which is step 23's rounding of
    // two significant digits instead.
    let (need_significant_digits, need_fraction_digits) =
        if rounding_priority == RoundingPriority::Auto {
            let need_significant_digits = has_significant_digits;
            let need_fraction_digits =
                !need_significant_digits && (has_fraction_digits || notation != Notation::Compact);

            (need_significant_digits, need_fraction_digits)
        } else {
            // Steps 20.1 and 20.2: a priority that was said applies both lists.
            (true, true)
        };

    // Steps 21 and 22, and with them the two flags that say which of the two pairs of digit slots
    // exist at all: 16.1.2 leaves a pair it does not need undefined, and 16.3.2 step 5.3 reports an
    // option only when its slot is not undefined.
    slots.reports_significant_digits = need_significant_digits;
    slots.reports_fraction_digits = need_fraction_digits;

    // Step 21.
    if need_significant_digits {
        let (minimum, maximum) = if has_significant_digits {
            // Steps 21.1.1 and 21.1.2.
            let minimum = default_number_option(cx, min_significant_digits, 1.0, 21.0, Some(1.0))?
                .unwrap_or(1.0);

            (
                minimum,
                default_number_option(cx, max_significant_digits, minimum, 21.0, Some(21.0))?
                    .unwrap_or(21.0),
            )
        } else {
            // Steps 21.2.1 and 21.2.2.
            (1.0, 21.0)
        };

        slots.minimum_significant_digits = minimum as u8;
        slots.maximum_significant_digits = maximum as u8;
    }

    // Step 22.
    if need_fraction_digits {
        let (minimum, maximum) = if has_fraction_digits {
            // Steps 22.1.1 and 22.1.2 read both entries with an `undefined` fallback: steps 22.1.3
            // and 22.1.4 do not fill an unsaid entry from the `minFDDefault`/`maxFDDefault` alone
            // but from the *other* resolved entry as well, which is what makes
            // `{ minimumFractionDigits: 5 }` resolve to the pair (5, 5) rather than to (5, 3).
            let minimum = default_number_option(cx, min_fraction_digits, 0.0, 100.0, None)?;
            let maximum = default_number_option(cx, max_fraction_digits, 0.0, 100.0, None)?;

            match (minimum, maximum) {
                // Step 22.1.5.
                (Some(minimum), Some(maximum)) if minimum > maximum => {
                    return range_error(
                        cx,
                        "NumberFormat minimumFractionDigits must not be greater than maximumFractionDigits",
                    );
                }
                (Some(minimum), Some(maximum)) => (minimum, maximum),
                // Step 22.1.3, then step 22.1.4. `has_fraction_digits` (step 18) is true here, so
                // at least one of the two was said and one of these two arms is what runs; the
                // last arm only keeps the match total over the pair step 18 rules out.
                (None, Some(maximum)) => (min_fraction_digits_default.min(maximum), maximum),
                (Some(minimum), None) => (minimum, max_fraction_digits_default.max(minimum)),
                _ => (min_fraction_digits_default, max_fraction_digits_default),
            }
        } else {
            // Steps 22.2.1 and 22.2.2.
            (min_fraction_digits_default, max_fraction_digits_default)
        };

        slots.minimum_fraction_digits = minimum as u8;
        slots.maximum_fraction_digits = maximum as u8;
    }

    // Steps 23 to 27. Step 23 is the compact notation with nothing said about the precision: 16.1.2
    // step 19.3 takes the fraction digits away, so neither list is needed and the rounding is the
    // two-significant-digit one of step 23, kept over the fraction-digit rounding by the
    // "morePrecision" it reports.
    let (rounding_type, computed_priority) = if !need_significant_digits && !need_fraction_digits {
        // Steps 23.1 to 23.6.
        slots.minimum_fraction_digits = 0;
        slots.maximum_fraction_digits = 0;
        slots.minimum_significant_digits = 1;
        slots.maximum_significant_digits = 2;

        // Steps 21 and 22 wrote neither pair above from an option, but step 23 wrote both, and
        // 16.3.2 reports a slot it does not leave `undefined`.
        slots.reports_fraction_digits = true;
        slots.reports_significant_digits = true;

        (RoundingType::MorePrecision, RoundingPriority::MorePrecision)
    } else if rounding_priority == RoundingPriority::MorePrecision {
        // Steps 24.1 and 24.2.
        (RoundingType::MorePrecision, RoundingPriority::MorePrecision)
    } else if rounding_priority == RoundingPriority::LessPrecision {
        // Steps 25.1 and 25.2.
        (RoundingType::LessPrecision, RoundingPriority::LessPrecision)
    } else if has_significant_digits {
        // Steps 26.1 and 26.2.
        (RoundingType::SignificantDigits, RoundingPriority::Auto)
    } else {
        // Steps 27.1 and 27.2.
        (RoundingType::FractionDigits, RoundingPriority::Auto)
    };

    slots.rounding_type = rounding_type;
    slots.rounding_priority = computed_priority;

    // Step 28.
    if !increment.is_one() {
        if rounding_type != RoundingType::FractionDigits {
            return type_error(
                cx,
                "NumberFormat roundingIncrement is only supported with fraction digits",
            );
        }

        if slots.maximum_fraction_digits != slots.minimum_fraction_digits {
            return range_error(
                cx,
                "NumberFormat roundingIncrement requires minimumFractionDigits to equal maximumFractionDigits",
            );
        }
    }

    Ok(())
}
