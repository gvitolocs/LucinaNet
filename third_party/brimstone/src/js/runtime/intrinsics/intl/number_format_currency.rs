//! Style `currency` of `Intl.NumberFormat`: the digits of [`DecimalRenderer`] inside a CLDR
//! currency pattern.
//!
//! ECMA-402 16.5.3 walks one pattern string and puts each piece of it under a part type. For the two
//! styles `icu_decimal` has, that pattern is inside the formatter and the walk is its own; for
//! currency the pattern lives in `CurrencyEssentialsV1`, which `icu_decimal` never reads, so the walk
//! is here. What it needs from the decimal side is the digits without a sign
//! ([`DecimalRenderer::render`], [`DecimalRenderer::write_unsigned`]) and the locale's sign affixes
//! ([`DecimalRenderer::signs`]).
//!
//! `icu_experimental`'s own `CurrencyFormatter` is not used to render: it re-rounds the value to the
//! currency's own precision, which would ignore `minimumFractionDigits` and the rest of 16.1.2, and it
//! writes through `writeable` without annotating a single part, so there would be nothing for 16.5.6
//! to list. What is taken from it instead is the meaning of the data and its pattern selection, which
//! is repeated below against the same markers.

use std::rc::Rc;
use std::str::FromStr;

use brimstone_intl_data::IntlDataProvider;
use fixed_decimal::{Sign, UnsignedDecimal};
use icu_experimental::dimension::currency::CurrencyType;
use icu_experimental::dimension::provider::currency::{
    essentials::CurrencyEssentialsV1, extended::CurrencyExtendedDataV1,
    fractions::CurrencyFractionsV1, patterns::CurrencyPatternsDataV1, symbols::CurrencySymbolsV1,
};
use icu_locale::Locale;
use icu_pattern::{DoublePlaceholderKey, DoublePlaceholderPattern, PatternItem};
use icu_plurals::{PluralOperands, PluralRules};
use icu_provider::prelude::{
    DataError, DataErrorKind, DataIdentifierBorrowed, DataLocale, DataMarker, DataMarkerAttributes,
    DataPayload, DataProvider, DataRequest, DataRequestMetadata,
};

use super::number_format_core::{
    Compacting, CurrencyCode, CurrencyDisplay, CurrencySign, DecimalRenderer, INFINITY_STRING,
    IntlMathValue, NAN_STRING, NumberFormatSlots, NumberPart, NumberSink, PartKind, Rendered,
    SignAffixes, SignCategory, pattern_sign,
};

/// The part of a currency format that is not the number.
///
/// The four values of [[CurrencyDisplay]] fall into two groups here rather than four, because that is
/// how the data falls: `symbol`, `narrowSymbol` and `code` each name a string that goes in the
/// locale's `CurrencyEssentialsV1` pattern and never changes with the value, while `name` reads two
/// other markers whose string *and* pattern are both chosen by the plural category of the number.
enum CurrencyText {
    /// `$`, `US$`, `¥` or the ISO code, with the two flags of the pattern selection: a currency
    /// string whose letters touch the number can need a different pattern, which is CLDR's
    /// `currencyAlphaNextToNumberPattern` and what makes `USD 1,234.50` carry a no-break space where
    /// `$1,234.50` carries none.
    Fixed {
        text: String,
        starts_with_letter: bool,
        ends_with_letter: bool,
    },
    /// A spelled-out currency name: the plural-selected string of `CurrencyExtendedDataV1` in the
    /// plural-selected pattern of `CurrencyPatternsDataV1`.
    Name {
        /// `None` for a code the name data has no entry for, which leaves only the code to show.
        extended: Option<DataPayload<CurrencyExtendedDataV1>>,
        patterns: DataPayload<CurrencyPatternsDataV1>,
        plural_rules: PluralRules,
        code: String,
    },
}

/// The three resolved options a currency rendering depends on, besides the locale and the grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CurrencySettings {
    pub code: CurrencyCode,
    pub display: CurrencyDisplay,
    pub sign: CurrencySign,
}

impl CurrencySettings {
    /// The settings of a resolved instance: 16.1.3 step 12 writes [[Currency]] for style "currency"
    /// and for no other style, so an instance whose slot is absent has no currency to render with
    /// and the two options that came with it are nothing but checked.
    pub fn of(slots: &NumberFormatSlots) -> Option<CurrencySettings> {
        Some(CurrencySettings {
            code: slots.currency?,
            display: slots.currency_display,
            sign: slots.currency_sign,
        })
    }
}

/// One locale and currency, built once and shared from the per-context cache.
pub struct CurrencyRenderer {
    /// The digits, separators and sign affixes of the locale.
    decimals: Rc<DecimalRenderer>,
    settings: CurrencySettings,
    essentials: DataPayload<CurrencyEssentialsV1>,
    text: CurrencyText,
}

impl CurrencyRenderer {
    /// Read the currency data of `tag` for one code, display and sign. The digits come from
    /// `decimals`, which the caller has built for the locale and the grouping already.
    pub fn try_new(
        tag: &str,
        decimals: Rc<DecimalRenderer>,
        settings: CurrencySettings,
    ) -> Result<Self, DataError> {
        let locale = Locale::from_str(tag)
            // `resolve_locale` only ever hands out tags this build has data for, all of which parse.
            .map_err(|_| DataError::custom("Intl resolved a locale tag that does not parse"))?;
        let data_locale = DataLocale::from(&locale);
        let code = settings.code.as_str().to_string();

        let essentials: DataPayload<CurrencyEssentialsV1> =
            DataProvider::<CurrencyEssentialsV1>::load(
                &IntlDataProvider,
                request(&data_locale, None),
            )?
            .payload;

        let text = match settings.display {
            // The code is its own currency string, and a code is three letters, so it both starts and
            // ends with one.
            CurrencyDisplay::Code => {
                CurrencyText::Fixed { text: code, starts_with_letter: true, ends_with_letter: true }
            }
            CurrencyDisplay::Symbol | CurrencyDisplay::NarrowSymbol => {
                // The identifier the symbols are keyed by, which is `CurrencySymbolsV1`'s own
                // `make_attributes`: the width marker `s` for the full symbol and `n` for the narrow
                // one, a slash, and the uppercase ISO code.
                let width = match settings.display {
                    CurrencyDisplay::Symbol => "s",
                    _ => "n",
                };
                let key = format!("{width}/{code}");

                match load_optional::<CurrencySymbolsV1>(&data_locale, marker_attributes(&key)?)? {
                    Some(symbol) => {
                        let symbol = symbol.get();
                        CurrencyText::Fixed {
                            text: symbol.as_str().to_string(),
                            starts_with_letter: symbol.starts_with_letter(),
                            ends_with_letter: symbol.ends_with_letter(),
                        }
                    }
                    // A code the symbol data has nothing for is not an error: 16.5.3 step 9.11 says to
                    // use the currency itself, which is what `icu_experimental` does for the same hole.
                    None => CurrencyText::Fixed {
                        text: code,
                        starts_with_letter: true,
                        ends_with_letter: true,
                    },
                }
            }
            CurrencyDisplay::Name => {
                let attributes = marker_attributes(&code)?;
                let extended = load_optional::<CurrencyExtendedDataV1>(&data_locale, attributes)?;
                // The unit pattern of a currency name is not per-locale in this data: the baked table
                // has one entry, under `und`, for the `{number} {name}` shape all three locales use.
                let patterns: DataPayload<CurrencyPatternsDataV1> =
                    DataProvider::<CurrencyPatternsDataV1>::load(
                        &IntlDataProvider,
                        request(&DataLocale::default(), None),
                    )?
                    .payload;
                CurrencyText::Name {
                    extended,
                    patterns,
                    plural_rules: PluralRules::try_new_cardinal_unstable(
                        &IntlDataProvider,
                        (&locale).into(),
                    )?,
                    code,
                }
            }
        };

        Ok(Self { decimals, settings, essentials, text })
    }

    /// FormatNumeric (https://tc39.es/ecma402/#sec-formatnumeric), 16.5.5.
    pub fn format(&self, slots: &NumberFormatSlots, value: &IntlMathValue) -> String {
        let mut sink = super::number_format_core::StringSink::default();
        self.partition(slots, value, &mut sink);
        sink.0
    }

    /// FormatNumericToParts (https://tc39.es/ecma402/#sec-formatnumerictoparts), 16.5.6.
    pub fn format_to_parts(
        &self,
        slots: &NumberFormatSlots,
        value: &IntlMathValue,
    ) -> Vec<NumberPart> {
        let mut sink = super::number_format_core::PartList::default();
        self.partition(slots, value, &mut sink);
        sink.0
    }

    /// PartitionNumberPattern (https://tc39.es/ecma402/#sec-partitionnumberpattern), 16.5.3, with the
    /// pattern of the currency data instead of the one `icu_decimal` holds.
    fn partition<S: NumberSink>(
        &self,
        slots: &NumberFormatSlots,
        value: &IntlMathValue,
        sink: &mut S,
    ) {
        match self.decimals.render(slots, value) {
            Rendered::Special { category, kind } => {
                self.write(slots, category, Body::Special(kind), sink)
            }
            Rendered::Number { category, digits, compact } => {
                self.write(slots, category, Body::Number(&digits, compact), sink)
            }
        }
    }

    /// 16.5.10's choice of pattern and 16.5.3's walk of it.
    ///
    /// The choice is the one `icu_experimental`'s `CurrencyFormatter` makes, and for its reason:
    /// `CurrencyEssentialsV1` holds a positive and a negative pattern per sign usage and per whether
    /// the currency string touches the number, and where the negative pattern of a usage is absent the
    /// positive one of that usage stands with the locale's minus affix applied outside it (UTS #35
    /// section 3.2.1, which 16.5.10 step 16 leaves to the pattern data). A negative pattern that is
    /// present carries its own sign, which is why `currencySign: "accounting"` puts the amount between
    /// parentheses and no minus anywhere.
    fn write<S: NumberSink>(
        &self,
        slots: &NumberFormatSlots,
        category: SignCategory,
        body: Body<'_>,
        sink: &mut S,
    ) {
        let sign = pattern_sign(slots.sign_display, category);
        let signs = self.decimals.signs();
        let accounting = self.settings.sign == CurrencySign::Accounting;

        match &self.text {
            CurrencyText::Fixed { text, starts_with_letter, ends_with_letter } => {
                let essentials = self.essentials.get();
                let (pattern, leftover) = if accounting {
                    if sign == Sign::Negative {
                        match essentials
                            .get_negative_accounting(*starts_with_letter, *ends_with_letter)
                        {
                            Some(pattern) => (pattern, Sign::None),
                            None => (
                                essentials.get_positive_accounting(
                                    *starts_with_letter,
                                    *ends_with_letter,
                                ),
                                sign,
                            ),
                        }
                    } else {
                        (
                            essentials
                                .get_positive_accounting(*starts_with_letter, *ends_with_letter),
                            sign,
                        )
                    }
                } else if sign == Sign::Negative {
                    match essentials.get_negative(*starts_with_letter, *ends_with_letter) {
                        Some(pattern) => (pattern, Sign::None),
                        None => {
                            (essentials.get_positive(*starts_with_letter, *ends_with_letter), sign)
                        }
                    }
                } else {
                    (essentials.get_positive(*starts_with_letter, *ends_with_letter), sign)
                };

                signs.wrap(leftover, sink, |sink| {
                    self.walk(pattern, text, signs, body, sink);
                });
            }
            CurrencyText::Name { extended, patterns, plural_rules, code } => {
                let patterns = patterns.get();
                let (pattern, text) = match (extended, body) {
                    // The plural category is that of the rendered number, which is why this arm wants
                    // the digits 16.5.2 produced rather than the value they came from: `1.00 US dollar`
                    // and `1.0 US dollars` differ by the fraction digits that are shown, not by the
                    // number they are rounded from. A special value has no digits to pluralize, and
                    // then the data is read as it reads zero.
                    (Some(extended), _) => {
                        let operands = match body {
                            Body::Number(digits, _) => PluralOperands::from(digits),
                            Body::Special(_) => PluralOperands::default(),
                        };
                        (
                            patterns.elements.get(operands, plural_rules).1,
                            extended.get().elements.get(operands, plural_rules).1,
                        )
                    }
                    // 16.5.3 step 9.11 with a currency the name data does not know: the pattern of
                    // another category stands, with the code in it.
                    (None, _) => (patterns.elements.get_default().1, code.as_str()),
                };

                signs.wrap(sign, sink, |sink| {
                    self.walk(pattern, text, signs, body, sink);
                });
            }
        }
    }

    /// 16.5.3 steps 5 to 9 over one currency pattern: its literals, its number and its currency
    /// string. `Place0` is the number and `Place1` the currency string, which is how the data's own
    /// adjacency test reads them.
    fn walk<S: NumberSink>(
        &self,
        pattern: &DoublePlaceholderPattern,
        currency: &str,
        signs: &SignAffixes,
        body: Body<'_>,
        sink: &mut S,
    ) {
        let mut body = Some(body);
        for item in pattern.iter() {
            match item {
                PatternItem::Literal(text) => {
                    if !text.is_empty() {
                        sink.write_part(signs.kind_of_literal(text), text);
                    }
                }
                PatternItem::Placeholder(DoublePlaceholderKey::Place0) => {
                    if let Some(body) = body.take() {
                        match body {
                            Body::Number(digits, compact) => {
                                self.decimals.write_unsigned(digits, compact, sink)
                            }
                            Body::Special(kind) => sink.write_part(
                                kind,
                                if kind == PartKind::Nan {
                                    NAN_STRING
                                } else {
                                    INFINITY_STRING
                                },
                            ),
                        }
                    }
                }
                PatternItem::Placeholder(DoublePlaceholderKey::Place1) => {
                    sink.write_part(PartKind::Currency, currency);
                }
            }
        }
        debug_assert!(
            body.is_none(),
            "a currency pattern the number does not fit into: {pattern:?}"
        );
    }
}

/// What goes where the pattern puts its number: the digits, or the word of a special value.
#[derive(Clone, Copy)]
enum Body<'a> {
    /// The scaled digits and, for the compact notation, the scale they were divided by, which is
    /// what puts `K` or `thousand` next to them.
    Number(&'a UnsignedDecimal, Option<Compacting>),
    Special(PartKind),
}

/// [[Currency]] (16.1.3 steps 5 and 12.1) as the data keys it: the same three letters, which
/// `CurrencyType` validates on the way in.
fn currency_type(code: CurrencyCode) -> Result<CurrencyType, DataError> {
    CurrencyType::from_str(code.as_str())
        .map_err(|_| DataError::custom("a currency code the data cannot be asked with"))
}

/// The data identifier of a currency's symbols and names, which is its ISO code.
pub(crate) fn marker_attributes(code: &str) -> Result<&DataMarkerAttributes, DataError> {
    DataMarkerAttributes::try_from_str(code)
        .map_err(|_| DataError::custom("a currency code the data cannot be asked with"))
}

/// CurrencyDigits (https://tc39.es/ecma402/#sec-currencydigits), which 16.1.1 step 13 puts in both
/// of the fraction-digit defaults of a currency format: two for almost every code, none for the yen
/// and the old dinars, and the table is locale-free so `en`, `da` and `it` read the same digits.
///
/// What the same table also carries and this does not read is the rounding increment - the
/// five-centime rounding of the Swiss franc, `Rounding::R5` - because `CurrencyDigits` is a digit
/// count and 16.1.1 asks for that count alone. A Swiss franc amount is therefore rounded to two
/// digits here, which is also what the implementations this build is compared against do.
pub fn currency_digits(code: CurrencyCode) -> Result<u8, DataError> {
    let fractions: DataPayload<CurrencyFractionsV1> = DataProvider::<CurrencyFractionsV1>::load(
        &IntlDataProvider,
        request(&DataLocale::default(), None),
    )?
    .payload;

    Ok(fractions.get().resolve(currency_type(code)?).digits)
}

/// Load a marker that may have nothing for the currency asked for. `None` is the hole in the data
/// and not a failure - it lets the caller fall back to the code, as 16.5.3 step 9.11 says - while
/// any other error is the broken build the caller reports as its own.
pub(crate) fn load_optional<M>(
    locale: &DataLocale,
    attributes: &DataMarkerAttributes,
) -> Result<Option<DataPayload<M>>, DataError>
where
    M: DataMarker,
    IntlDataProvider: DataProvider<M>,
{
    match DataProvider::<M>::load(&IntlDataProvider, request(locale, Some(attributes))) {
        Ok(response) => Ok(Some(response.payload)),
        Err(error) if error.kind == DataErrorKind::IdentifierNotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// A data request for `locale` and, where the marker needs one, the identifier of a currency.
fn request<'a>(
    locale: &'a DataLocale,
    attributes: Option<&'a DataMarkerAttributes>,
) -> DataRequest<'a> {
    let id = match attributes {
        Some(attributes) => {
            DataIdentifierBorrowed::for_marker_attributes_and_locale(attributes, locale)
        }
        // The locale-keyed markers are asked with no identifier at all, which is the empty one.
        None => DataIdentifierBorrowed::for_locale(locale),
    };

    // `DataRequestMetadata` is `#[non_exhaustive]`, so the one field is set on a default rather than
    // written in a literal.
    let mut metadata = DataRequestMetadata::default();

    // A miss is answered by the caller - with the ISO code for a hole in the data, and with a
    // TypeError for a table that is missing altogether - so the library is not asked to log the same
    // miss as a warning on top of that.
    metadata.silent = true;

    DataRequest { id, metadata }
}

#[cfg(test)]
mod tests {
    use super::super::number_format_core::{
        SignDisplay, Style, UseGrouping, math_value_from_string,
    };
    use super::*;

    /// The renderer of one locale and currency, with the digits 16.1.1 step 13 gives it.
    fn renderer(
        locale: &str,
        code: CurrencyCode,
        display: CurrencyDisplay,
        sign: CurrencySign,
    ) -> CurrencyRenderer {
        let decimals = Rc::new(DecimalRenderer::try_new(locale, UseGrouping::Auto).unwrap());
        CurrencyRenderer::try_new(locale, decimals, CurrencySettings { code, display, sign })
            .unwrap()
    }

    /// The slots a `NumberFormat` of this currency ends up with, plus the value to format.
    fn formatted(
        renderer: &CurrencyRenderer,
        text: &str,
        change: impl FnOnce(&mut NumberFormatSlots),
    ) -> String {
        let mut slots = currency_slots(renderer.settings.code);
        change(&mut slots);
        renderer.format(&slots, &math_value_from_string(text))
    }

    /// [[Style]], [[Currency]] and the two fraction-digit slots of 16.1.1 step 13, which are the
    /// whole of what the constructor leaves on a currency format that asks for no digits.
    fn currency_slots(code: CurrencyCode) -> NumberFormatSlots {
        let digits = currency_digits(code).unwrap();
        let mut slots = NumberFormatSlots::default();
        slots.style = Style::Currency;
        slots.currency = Some(code);
        slots.minimum_fraction_digits = u8::from(digits);
        slots.maximum_fraction_digits = u8::from(digits);
        slots
    }

    fn code(text: &str) -> CurrencyCode {
        CurrencyCode::parse(text).unwrap()
    }

    fn parts(renderer: &CurrencyRenderer, text: &str) -> Vec<(PartKind, String)> {
        let slots = currency_slots(renderer.settings.code);
        renderer
            .format_to_parts(&slots, &math_value_from_string(text))
            .into_iter()
            .map(|part| (part.kind, part.value))
            .collect()
    }

    #[test]
    fn the_us_dollar_symbol_wraps_the_number_the_way_the_data_says() {
        let usd = renderer("en", code("USD"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(formatted(&usd, "1234.5", no_change), "$1,234.50");
        assert_eq!(formatted(&usd, "-5", no_change), "-$5.00");
        assert_eq!(formatted(&usd, "0", no_change), "$0.00");
        // The CLDR digit count, not the three of 16.1.1 step 15.
        assert_eq!(formatted(&usd, "5", no_change), "$5.00");
    }

    #[test]
    fn an_accounting_negative_is_in_parentheses_and_has_no_minus() {
        let usd = renderer("en", code("USD"), CurrencyDisplay::Symbol, CurrencySign::Accounting);
        assert_eq!(formatted(&usd, "-5", no_change), "($5.00)");
        assert_eq!(formatted(&usd, "5", no_change), "$5.00");
        // 16.5.3 keeps the parentheses literals as literals: they are not the locale's minus.
        assert_eq!(
            parts(&usd, "-5"),
            vec![
                (PartKind::Literal, "(".to_string()),
                (PartKind::Currency, "$".to_string()),
                (PartKind::Integer, "5".to_string()),
                (PartKind::Decimal, ".".to_string()),
                (PartKind::Fraction, "00".to_string()),
                (PartKind::Literal, ")".to_string()),
            ]
        );
    }

    #[test]
    fn a_plain_negative_marks_its_sign_as_a_sign() {
        let usd = renderer("en", code("USD"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(
            parts(&usd, "-5"),
            vec![
                (PartKind::MinusSign, "-".to_string()),
                (PartKind::Currency, "$".to_string()),
                (PartKind::Integer, "5".to_string()),
                (PartKind::Decimal, ".".to_string()),
                (PartKind::Fraction, "00".to_string()),
            ]
        );
    }

    #[test]
    fn the_code_display_separates_with_a_no_break_space() {
        let usd = renderer("en", code("USD"), CurrencyDisplay::Code, CurrencySign::Standard);
        // CLDR puts U+00A0 between a currency string of letters and the number, which is what the
        // alpha-next-to-number pattern of the data is for.
        assert_eq!(formatted(&usd, "1234.5", no_change), "USD\u{a0}1,234.50");
        assert_eq!(
            parts(&usd, "1234.5"),
            vec![
                (PartKind::Currency, "USD".to_string()),
                (PartKind::Literal, "\u{a0}".to_string()),
                (PartKind::Integer, "1".to_string()),
                (PartKind::Group, ",".to_string()),
                (PartKind::Integer, "234".to_string()),
                (PartKind::Decimal, ".".to_string()),
                (PartKind::Fraction, "50".to_string()),
            ]
        );
    }

    #[test]
    fn the_name_display_pluralizes_with_the_rendered_digits() {
        let usd = renderer("en", code("USD"), CurrencyDisplay::Name, CurrencySign::Standard);
        assert_eq!(formatted(&usd, "1234.5", no_change), "1,234.50 US dollars");
        assert_eq!(formatted(&usd, "-5", no_change), "-5.00 US dollars");
        // Two fraction digits make even a one a plural, which is what the operands of the rendered
        // value say and what the measured output shows.
        assert_eq!(formatted(&usd, "1", no_change), "1.00 US dollars");
        // Without fraction digits the same value is the singular category.
        assert_eq!(
            formatted(&usd, "1", |slots| {
                slots.minimum_fraction_digits = 0;
                slots.maximum_fraction_digits = 0;
            }),
            "1 US dollar"
        );
    }

    #[test]
    fn the_narrow_symbol_and_the_symbol_differ_where_the_data_says_they_do() {
        let narrow =
            renderer("en", code("USD"), CurrencyDisplay::NarrowSymbol, CurrencySign::Standard);
        assert_eq!(formatted(&narrow, "1234.5", no_change), "$1,234.50");
        // In Danish the two differ: the symbol names the country, the narrow form does not.
        let danish = renderer("da", code("USD"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(formatted(&danish, "1234.5", no_change), "1.234,50\u{a0}US$");
        let danish_narrow =
            renderer("da", code("USD"), CurrencyDisplay::NarrowSymbol, CurrencySign::Standard);
        assert_eq!(formatted(&danish_narrow, "1234.5", no_change), "1.234,50\u{a0}$");
    }

    #[test]
    fn the_suffix_pattern_of_a_locale_keeps_the_sign_in_front_of_everything() {
        let danish = renderer("da", code("USD"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(formatted(&danish, "-5", no_change), "-5,00\u{a0}US$");
        assert_eq!(
            parts(&danish, "-5"),
            vec![
                (PartKind::MinusSign, "-".to_string()),
                (PartKind::Integer, "5".to_string()),
                (PartKind::Decimal, ",".to_string()),
                (PartKind::Fraction, "00".to_string()),
                (PartKind::Literal, "\u{a0}".to_string()),
                (PartKind::Currency, "US$".to_string()),
            ]
        );
    }

    #[test]
    fn the_italian_digits_group_only_where_the_locale_groups() {
        let it = renderer("it", code("USD"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        // Italian wants two or more digits in front of a separator, so 1234 stays as it is.
        assert_eq!(formatted(&it, "1234.5", no_change), "1234,50\u{a0}US$");
        assert_eq!(formatted(&it, "1234567.891", no_change), "1.234.567,89\u{a0}US$");
        // The symbol is the one the root data carries, because the Italian row for the US dollar
        // spells it the same way: a locale that overrides nothing falls back to root the way the
        // library's own formatter does.
        let euro = renderer("it", code("EUR"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(formatted(&euro, "1234.5", no_change), "1234,50\u{a0}€");
        // A code the symbol data has no row for anywhere is named by itself, in the same pattern.
        let koruna = renderer("it", code("DKK"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(formatted(&koruna, "1234.5", no_change), "1234,50\u{a0}DKK");
    }

    #[test]
    fn a_currency_with_no_fraction_digits_shows_none() {
        let jpy = renderer("en", code("JPY"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(currency_digits(code("JPY")).unwrap(), 0);
        assert_eq!(formatted(&jpy, "5", no_change), "¥5");
        assert_eq!(formatted(&jpy, "-5", no_change), "-¥5");
    }

    #[test]
    fn a_code_the_data_has_no_entry_for_is_named_by_itself() {
        // 16.5.10 step 5 has the locale's pattern stand, and 16.5.3 step 9.11 has the code fill the
        // place of the string that is missing.
        let unknown = renderer("en", code("XYZ"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(formatted(&unknown, "1234.5", no_change), "XYZ\u{a0}1,234.50");
        let unknown_name =
            renderer("en", code("XYZ"), CurrencyDisplay::Name, CurrencySign::Standard);
        assert_eq!(formatted(&unknown_name, "1234.5", no_change), "1,234.50 XYZ");
        // The digit table knows no more than the symbol table does, so the default is its two.
        assert_eq!(currency_digits(code("XYZ")).unwrap(), 2);
    }

    #[test]
    fn a_special_value_goes_where_the_pattern_puts_the_number() {
        let usd = renderer("en", code("USD"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(formatted(&usd, "NaN", no_change), "$NaN");
        assert_eq!(formatted(&usd, "-Infinity", no_change), "-$∞");
        assert_eq!(
            parts(&usd, "Infinity"),
            vec![
                (PartKind::Currency, "$".to_string()),
                (PartKind::Infinity, "∞".to_string()),
            ]
        );
        // 16.5.10 step 9 counts NaN as a positive zero, so `negative` leaves it unsigned.
        assert_eq!(
            formatted(&usd, "NaN", |slots| slots.sign_display = SignDisplay::Negative),
            "$NaN"
        );
        assert_eq!(
            formatted(&usd, "-5", |slots| slots.sign_display = SignDisplay::Always),
            "-$5.00"
        );
        assert_eq!(
            formatted(&usd, "5", |slots| slots.sign_display = SignDisplay::Always),
            "+$5.00"
        );
    }

    #[test]
    fn the_currency_digits_of_the_three_tested_currencies() {
        assert_eq!(currency_digits(code("USD")).unwrap(), 2);
        assert_eq!(currency_digits(code("EUR")).unwrap(), 2);
        assert_eq!(currency_digits(code("JPY")).unwrap(), 0);
        assert_eq!(currency_digits(code("BHD")).unwrap(), 3);
    }

    #[test]
    fn the_digit_options_beat_the_currency_digits() {
        let usd = renderer("en", code("USD"), CurrencyDisplay::Symbol, CurrencySign::Standard);
        assert_eq!(
            formatted(&usd, "1234.5", |slots| {
                slots.minimum_fraction_digits = 0;
                slots.maximum_fraction_digits = 0;
            }),
            "$1,235"
        );
        assert_eq!(
            formatted(&usd, "1234.5", |slots| {
                slots.minimum_fraction_digits = 4;
                slots.maximum_fraction_digits = 4;
            }),
            "$1,234.5000"
        );
    }

    #[test]
    fn grouping_off_leaves_the_number_ungrouped_under_the_pattern() {
        let renderer = |grouping| {
            let decimals = Rc::new(DecimalRenderer::try_new("en", grouping).unwrap());
            CurrencyRenderer::try_new(
                "en",
                decimals,
                CurrencySettings {
                    code: code("USD"),
                    display: CurrencyDisplay::Symbol,
                    sign: CurrencySign::Standard,
                },
            )
            .unwrap()
        };
        let wide = renderer(UseGrouping::Never);
        assert_eq!(formatted(&wide, "1234567.891", no_change), "$1234567.89");
        let grouped = renderer(UseGrouping::Auto);
        assert_eq!(formatted(&grouped, "1234567.891", no_change), "$1,234,567.89");
    }

    fn no_change(_slots: &mut NumberFormatSlots) {}
}
