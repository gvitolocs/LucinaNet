//! The locale tables Intl works with, kept per context instead of rebuilt per call.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use brimstone_intl_data::{BUILT_IN_LOCALES, IntlDataProvider};
use icu_collator::Collator;
use icu_list::ListFormatter;
use icu_locale::LocaleCanonicalizer;
use icu_plurals::{PluralRules, PluralRulesWithRanges};
use icu_provider::prelude::DataError;
use icu_time::zone::IanaParser;

use super::collator_object::CollatorSettings;
use super::date_time_format_core::{DateTimeRenderer, FieldSetError, FieldSetKey};
use super::display_names_object::{DisplayNamesStyle, DisplayNamesTables};
use super::list_format_object::ListFormatSettings;
use super::number_format_core::{DecimalRenderer, UseGrouping};
use super::number_format_currency::{CurrencyRenderer, CurrencySettings};
use super::plural_rules_object::PluralRulesSettings;
use super::relative_time_format_object::{
    RelativeTimePatterns, RelativeTimeStyle, RelativeTimeUnit, build_relative_time_patterns,
};
use super::segmenter_object::{NativeSegmenter, SegmenterSettings};

/// A built-in locale as the small `Copy` id the engine hands around instead of its tag.
///
/// The tag itself is a `&'static str` in `BUILT_IN_LOCALES`, so this is not about owning anything:
/// it is so that a NumberFormat instance, which ECMA-402 16.4 lets store no borrowed data, can hold
/// its [[LocaleData]] as one byte, and so that the renderer cache below has a key it can hash
/// without comparing strings.
///
/// The `TryFrom<&str>` is total over the tags Intl deals with, because [`resolve_locale`] only ever
/// hands back a tag out of `BUILT_IN_LOCALES`. The `TryFrom` rather than a `From` and a panic is for
/// the list having been built without the tag some caller already holds: that is a broken build, and
/// saying so at the conversion is clearer than a renderer formatting with no separators.
///
/// [`resolve_locale`]: super::locale_negotiation::resolve_locale
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataLocale {
    index: u8,
}

impl DataLocale {
    /// The locale every code that has to answer before it knows which one is meant should answer
    /// with: the first of the built-in list, which the build script puts there for exactly this.
    pub const DEFAULT: DataLocale = DataLocale { index: 0 };

    /// The tag this locale is, as the `&'static str` the data crate compiled it from.
    pub fn tag(self) -> &'static str {
        BUILT_IN_LOCALES[self.index as usize]
    }
}

impl TryFrom<&str> for DataLocale {
    type Error = ();

    fn try_from(tag: &str) -> Result<Self, ()> {
        BUILT_IN_LOCALES
            .iter()
            .position(|baked| *baked == tag)
            .and_then(|index| u8::try_from(index).ok())
            .map(|index| DataLocale { index })
            .ok_or(())
    }
}

/// The ICU4X objects that locale negotiation needs, built on first use.
///
/// Constructing a `LocaleCanonicalizer` copies the alias and likely-subtags tables out of the baked
/// data into owned structures, which is far too much work for every `Intl` call that resolves a
/// locale. It lives here rather than in a `static` because contexts are what the engine is
/// instantiated per, and because the tables are only ever reached from Intl's own operations, so
/// an engine that never touches `Intl` never pays for them.
#[derive(Default)]
pub struct LocaleCache {
    canonicalizer: OnceCell<LocaleCanonicalizer>,

    /// The collators that have been built so far, under the resolved options they were built from.
    ///
    /// A `Collator` owns the compiled tailoring rules of its locale plus several buffers, so it
    /// cannot live in a collator instance, which has to be trivially destructible. What an instance
    /// holds instead is its `CollatorSettings`, and this map is what turns those settings back into
    /// the object - once per distinct set of options rather than once per comparison.
    collators: RefCell<HashMap<CollatorSettings, Rc<Collator>>>,

    /// One `icu_decimal` rendering per locale and per `useGrouping` choice, which are the only two
    /// things a decimal format's rendering depends on. A NumberFormat instance holds its resolved
    /// options and nothing else (ECMA-402 16.4), so the formatters have to live here rather than on
    /// the instances. The key is the data locale of 9.2.7 step 5, not the resolved tag step 13
    /// builds, because the `-u-nu-latn` a request can leave on that tag names no locale the data
    /// has an entry for.
    decimal_renderers: RefCell<HashMap<(DataLocale, UseGrouping), Rc<DecimalRenderer>>>,

    /// The plural rules that have been built so far, under the locale and `type` they were built
    /// for. A PluralRules instance holds its resolved options and nothing else (ECMA-402 17.4), so
    /// the ICU4X rules - which own the compiled operand expressions - live here, and the [[Type]]
    /// of 17.1.1 step 8 is the only option that changes which rules those are.
    plural_rules: RefCell<HashMap<PluralRulesSettings, Rc<PluralRulesWithRanges<PluralRules>>>>,

    /// The list formatters that have been built so far, under the locale, `type` and `style` they
    /// were built for. A ListFormat instance holds its resolved options and nothing else (ECMA-402
    /// 14.4), and the [[Templates]] of 14.4 are the compiled pattern strings a `ListFormatter` owns,
    /// so this map is what turns an instance's settings back into them.
    list_formatters: RefCell<HashMap<ListFormatSettings, Rc<ListFormatter>>>,

    /// The parser that turns an IANA time-zone id into the BCP-47 subtag CLDR's zone tables are
    /// keyed at, which is the table `icu/generate_intl_data.sh` bakes rather than the one ICU4X's
    /// own compiled data carries.
    iana_parser: OnceCell<IanaParser>,

    /// One `icu_datetime` rendering per locale and per resolved field set, which are the only two
    /// things a date format's rendering depends on. The reason is the same as the one above: an
    /// instance of `Intl.DateTimeFormat` may hold no borrowed data, so the loaded pattern set lives
    /// here and the instance holds the key that reaches it.
    datetime_renderers: RefCell<HashMap<(DataLocale, FieldSetKey), Rc<DateTimeRenderer>>>,

    /// The segmenters that have been built so far, under the locale and `granularity` they were
    /// built for. A Segmenter instance holds its resolved options and nothing else (ECMA-402 19.4),
    /// and an ICU4X segmenter owns its data payloads and buffers, so this map is what turns an
    /// instance's settings back into one. Unlike the formatters above a segmenter is not enough on
    /// its own to answer a question - it is the boundary table of one string that does that, and
    /// that table is the Segments object's own, because it depends on the string too.
    segmenters: RefCell<HashMap<SegmenterSettings, Rc<NativeSegmenter>>>,

    /// One `CurrencyRenderer` per locale, `useGrouping` choice and currency, the last being the three
    /// options 16.1.3 resolves for style "currency" and nothing else about the amount. As with the
    /// decimals, an instance of 16.4 can hold only resolved options, so the pattern data it formats
    /// through lives here and every instance of the same key shares it.
    ///
    /// The grouping is part of the key because a currency rendering is a decimal rendering of the
    /// digits, and the one entry per locale and grouping that the map above keeps is what it reads
    /// them through.
    currency_renderers:
        RefCell<HashMap<(DataLocale, UseGrouping, CurrencySettings), Rc<CurrencyRenderer>>>,

    /// One relative-time pattern set per locale, `style` and unit, which are the three things
    /// 18.5.2 reads its tables by. A RelativeTimeFormat instance holds its resolved options and
    /// nothing else (ECMA-402 18.4), and the [[LocaleData]] those options name is one of the
    /// twenty-four markers the data crate bakes, so the loaded tables live here under the
    /// [`RelativeTimeSettings`] that reaches them and the unit of the call that asked.
    relative_time_patterns: RefCell<
        HashMap<(DataLocale, RelativeTimeStyle, RelativeTimeUnit), Rc<RelativeTimePatterns>>,
    >,

    /// The display name tables per locale and `style`, the two things 12.3.3's [[Fields]] is keyed
    /// by: `type` picks which of the three tables of [`DisplayNamesTables`] a name is read out of,
    /// and `fallback` and `languageDisplay` change no data. A DisplayNames instance holds its
    /// resolved options and nothing else (ECMA-402 12.4), and the tables a `multi::*` formatter owns
    /// are compiled structures, so they live here and every instance of the same pair shares them.
    display_names: RefCell<HashMap<(DataLocale, DisplayNamesStyle), Rc<DisplayNamesTables>>>,
}

impl LocaleCache {
    /// The canonicalizer to hand to `locale_negotiation`'s `_with` operations.
    pub fn canonicalizer(&self) -> &LocaleCanonicalizer {
        self.canonicalizer.get_or_init(|| {
            LocaleCanonicalizer::try_new_extended_unstable(&IntlDataProvider)
                // The tables are compiled into the binary, so the only way this fails is that
                // `icu/generate_intl_data.sh` was run without a marker the code asks for, which is
                // a broken build rather than anything a program can cause.
                .unwrap()
        })
    }

    /// The collator for a set of resolved options, built on first use and shared from then on.
    pub fn collator(&self, settings: CollatorSettings) -> Rc<Collator> {
        // The borrow is dropped before the build: compiling a tailoring is the slow part of this,
        // and a panic inside it must not leave the map borrowed.
        let cached = self.collators.borrow().get(&settings).cloned();
        if let Some(collator) = cached {
            return collator;
        }

        let collator = Rc::new(settings.build_collator());
        self.collators
            .borrow_mut()
            .insert(settings, Rc::clone(&collator));
        collator
    }

    /// The rendering of `locale` for one grouping choice, built on first use.
    pub fn decimal_renderer(
        &self,
        locale: DataLocale,
        grouping: UseGrouping,
    ) -> Result<Rc<DecimalRenderer>, DataError> {
        if let Some(renderer) = self.decimal_renderers.borrow().get(&(locale, grouping)) {
            return Ok(renderer.clone());
        }

        let renderer = Rc::new(DecimalRenderer::try_new(locale.tag(), grouping)?);
        self.decimal_renderers
            .borrow_mut()
            .insert((locale, grouping), renderer.clone());

        Ok(renderer)
    }

    /// The zone-id parser, built on first use from the baked table.
    pub(crate) fn iana_parser(&self) -> &IanaParser {
        self.iana_parser.get_or_init(|| {
            IanaParser::try_new_unstable(&IntlDataProvider)
                // The table is compiled into the binary, so the only way this fails is a build
                // that did not bake the marker the code asks for.
                .unwrap()
        })
    }

    /// The rendering of `locale` for one resolved field set, built on first use.
    pub(crate) fn datetime_renderer(
        &self,
        locale: DataLocale,
        key: FieldSetKey,
    ) -> Result<Rc<DateTimeRenderer>, FieldSetError> {
        if let Some(renderer) = self.datetime_renderers.borrow().get(&(locale, key)) {
            return Ok(renderer.clone());
        }

        let renderer = Rc::new(DateTimeRenderer::try_new(locale.tag(), key)?);
        self.datetime_renderers
            .borrow_mut()
            .insert((locale, key), renderer.clone());

        Ok(renderer)
    }

    /// The currency rendering of `locale` for one grouping choice and one resolved currency, built on
    /// first use. The digits are read through the decimal rendering of the same pair, which is why
    /// this asks for it from the map above rather than building another.
    pub fn currency_renderer(
        &self,
        locale: DataLocale,
        grouping: UseGrouping,
        settings: CurrencySettings,
    ) -> Result<Rc<CurrencyRenderer>, DataError> {
        // The borrow is dropped before the build, so that a panic inside the data loading cannot
        // leave the map borrowed.
        let key = (locale, grouping, settings);
        let cached = self.currency_renderers.borrow().get(&key).cloned();
        if let Some(renderer) = cached {
            return Ok(renderer);
        }

        let decimals = self.decimal_renderer(locale, grouping)?;
        let renderer = Rc::new(CurrencyRenderer::try_new(locale.tag(), decimals, settings)?);
        self.currency_renderers
            .borrow_mut()
            .insert(key, Rc::clone(&renderer));

        Ok(renderer)
    }

    /// The plural rules for a locale and a `type`, built on first use and shared from then on.
    pub fn plural_rules(
        &self,
        settings: PluralRulesSettings,
    ) -> Rc<PluralRulesWithRanges<PluralRules>> {
        // The borrow is dropped before the build, as in [`Self::collator`].
        let cached = self.plural_rules.borrow().get(&settings).cloned();
        if let Some(plural_rules) = cached {
            return plural_rules;
        }

        let plural_rules = Rc::new(settings.build_plural_rules());
        self.plural_rules
            .borrow_mut()
            .insert(settings, Rc::clone(&plural_rules));

        plural_rules
    }

    /// The templates for a locale, a `type` and a `style`, built on first use and shared from then on.
    pub fn list_formatter(&self, settings: ListFormatSettings) -> Rc<ListFormatter> {
        // The borrow is dropped before the build, as in [`Self::collator`].
        let cached = self.list_formatters.borrow().get(&settings).cloned();
        if let Some(list_formatter) = cached {
            return list_formatter;
        }

        let list_formatter = settings.build_formatter_or_panic();
        self.list_formatters
            .borrow_mut()
            .insert(settings, Rc::clone(&list_formatter));

        list_formatter
    }

    /// The segmenter for a locale and a `granularity`, built on first use and shared from then on.
    pub fn segmenter(&self, settings: SegmenterSettings) -> Rc<NativeSegmenter> {
        // The borrow is dropped before the build, as in [`Self::collator`]: loading a break table is
        // the slow part of this, and a panic inside it must not leave the map borrowed.
        let cached = self.segmenters.borrow().get(&settings).cloned();
        if let Some(segmenter) = cached {
            return segmenter;
        }

        let segmenter = Rc::new(settings.build_segmenter());
        self.segmenters
            .borrow_mut()
            .insert(settings, Rc::clone(&segmenter));

        segmenter
    }

    /// The relative-time pattern tables of a locale, a `style` and a unit, built on first use and
    /// shared from then on.
    ///
    /// A `DataError` here means the baked data has no relative-time marker for a built-in locale,
    /// which `brimstone_intl_data`'s tests refuse; the callers turn it into the RangeError that
    /// names the build rather than the program as the cause.
    pub(crate) fn relative_time_patterns(
        &self,
        locale: DataLocale,
        style: RelativeTimeStyle,
        unit: RelativeTimeUnit,
    ) -> Result<Rc<RelativeTimePatterns>, DataError> {
        let key = (locale, style, unit);
        if let Some(patterns) = self.relative_time_patterns.borrow().get(&key) {
            return Ok(patterns.clone());
        }

        let patterns = Rc::new(build_relative_time_patterns(locale, style, unit)?);
        self.relative_time_patterns
            .borrow_mut()
            .insert(key, Rc::clone(&patterns));

        Ok(patterns)
    }

    /// The display name tables of a locale and a `style`, built on first use and shared from then on.
    ///
    /// A `DataError` here means the baked data has no display names marker for a built-in locale,
    /// which `brimstone_intl_data`'s tests refuse; 12.3.3 has no error for it, so the caller reports
    /// the build rather than the program as the cause.
    pub(crate) fn display_names(
        &self,
        locale: DataLocale,
        style: DisplayNamesStyle,
    ) -> Result<Rc<DisplayNamesTables>, DataError> {
        // The borrow is dropped before the build, as in [`Self::collator`]: building the three
        // formatters is the slow part of this, and a panic inside it must not leave the map borrowed.
        let cached = self.display_names.borrow().get(&(locale, style)).cloned();
        if let Some(tables) = cached {
            return Ok(tables);
        }

        let tables = Rc::new(DisplayNamesTables::build(locale, style)?);
        self.display_names
            .borrow_mut()
            .insert((locale, style), Rc::clone(&tables));

        Ok(tables)
    }
}
