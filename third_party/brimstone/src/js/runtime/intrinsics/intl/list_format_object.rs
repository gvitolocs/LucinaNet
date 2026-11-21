//! The `Intl.ListFormat` instance (https://tc39.es/ecma402/#sec-listformat-objects), 14.4, and the
//! operations that work from it: CreatePartsFromList (14.5.2), FormatList (14.5.3), FormatListToParts
//! (14.5.4) and StringListFromIterable (14.5.5).
//!
//! An instance holds what 14.4 lists and nothing else: the resolved locale, `type` and `style`. The
//! [[Templates]] slot of 14.4 is the fourth one and the only one not stored here, for the same reason
//! the plural rules are not stored in [`super::plural_rules_object`]: a `ListFormatter` owns the
//! compiled pattern strings and their automata, which a GC object may not hold, so the instance keeps
//! the [`ListFormatSettings`] that name them and [`super::cache::LocaleCache`] holds the formatter.
//!
//! 14.5.2 is written against [[Templates]] as a bag of five strings, and `icu_list` - the crate this
//! build reads its list data with - keeps them compiled and hands back no way to read them. What it
//! does hand back is the formatted list as the pieces it came from, and those pieces are exactly the
//! two kinds of record 14.5.2 returns: `element` for one place of the list and `literal` for the text
//! between two places. So the parts below come out of `icu_list`'s own formatting pass, and `format`
//! is those parts concatenated rather than a second pass, which is what makes 14.5.3 and 14.5.4 agree
//! with each other.
//!
//! The choice of template, which is the other half of 14.5.2, is `icu_list`'s too: it reads the
//! `Pair` of the locale for two places, and `Start`, `Middle` and `End` around the rest - the rule
//! UTS 35 documents and every engine formats by. That is a choice 14.5.2 leaves open in its own words:
//! both its step 3.1 and its step 7.2 replace a placeholder at "an implementation-defined index".
//! Following the clause's arithmetic literally instead would ask a pattern for a third place that the
//! list has no text for, so a list of four in `en` would spell its own separator twice - a bug in the
//! clause rather than a difference, and it is recorded in NOTES-intl-small-four.md.
//!
//! A part's text is kept as WTF-8, the engine's own representation of a JavaScript String: a place of
//! the list can hold a lone surrogate, `icu_list` reads only well-formed text, and 14.5.5 says nothing
//! that would turn one into the other. Each part's text is therefore written back from the list rather
//! than taken from the formatting pass.
//!
//! Reading the parts out of a `writeable` pass, and the `element`/`literal` split that comes with it,
//! is adapted from Boa (Adapted from Boa (boa-dev/boa, MIT OR Unlicense),
//! core/engine/src/builtins/intl/list_format/mod.rs).

use std::rc::Rc;

use brimstone_intl_data::IntlDataProvider;
use icu_list::{
    ListFormatter, ListFormatterPreferences,
    options::{ListFormatterOptions, ListLength},
};
use icu_locale::Locale as IcuLocale;
use writeable::{Part, PartsWrite, Writeable};

use super::cache::DataLocale;
use super::intl_object::canonicalize_locale_list_argument;
use super::locale_negotiation::{Matcher, resolve_locale};
use super::options::{get_enum_option, get_name_option, get_options_object};
use crate::common::wtf_8::Wtf8String;
use crate::extend_object;
use crate::runtime::{
    Context, Handle, PropertyKey, Value,
    abstract_operations::create_data_property_or_throw,
    error::{range_error, type_error},
    eval_result::EvalResult,
    gc::{HeapItem, HeapPtr, HeapVisitor},
    intrinsics::intrinsics::Intrinsic,
    iterator::{IteratorHint, get_iterator, iterator_close, iterator_step_value},
    object_value::ObjectValue,
    ordinary_object::{ObjectBuilder, ordinary_object_create},
};

/// [[Type]] (https://tc39.es/ecma402/#sec-intl-listformat-constructor), 14.1.1 step 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListFormatType {
    Conjunction,
    Disjunction,
    Unit,
}

impl ListFormatType {
    /// The three names 14.1.1 step 7 lists, in that order.
    pub const NAMES: [&'static str; 3] = ["conjunction", "disjunction", "unit"];

    /// The String value 14.3.2 reports for it, and the option value 14.1.1 step 7 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conjunction => "conjunction",
            Self::Disjunction => "disjunction",
            Self::Unit => "unit",
        }
    }

    /// 14.1.1 step 7 has already refused everything but the three names of this enum, so the only
    /// question this answers is which of them was said.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "conjunction" => Some(Self::Conjunction),
            "disjunction" => Some(Self::Disjunction),
            "unit" => Some(Self::Unit),
            _ => None,
        }
    }
}

/// [[Style]] (https://tc39.es/ecma402/#sec-intl-listformat-constructor), 14.1.1 step 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListFormatStyle {
    Long,
    Short,
    Narrow,
}

impl ListFormatStyle {
    /// The three names 14.1.1 step 9 lists, in that order.
    pub const NAMES: [&'static str; 3] = ["long", "short", "narrow"];

    /// The String value 14.3.2 reports for it, and the option value 14.1.1 step 9 accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Long => "long",
            Self::Short => "short",
            Self::Narrow => "narrow",
        }
    }

    /// 14.1.1 step 9 has already refused everything but the three names of this enum.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "long" => Some(Self::Long),
            "short" => Some(Self::Short),
            "narrow" => Some(Self::Narrow),
            _ => None,
        }
    }

    /// The length `icu_list` spells this style: CLDR names the set a `long` list reads `wide`, which
    /// is the marker attribute the list data is baked under.
    fn as_icu_length(self) -> ListLength {
        match self {
            Self::Long => ListLength::Wide,
            Self::Short => ListLength::Short,
            Self::Narrow => ListLength::Narrow,
        }
    }
}

/// The `type` and `style` of 14.1.1 steps 8 and 10 with the resolved data locale of step 6, as the
/// `Copy` key of the cache entry that holds the [[Templates]] of 14.4.
///
/// The data locale rather than the reported tag because 14.2.3's [[RelevantExtensionKeys]] is empty,
/// so nothing a program puts in a `-u-` extension can have any say over which templates are used - the
/// same reason every other entry of [`super::cache::LocaleCache`] is keyed this way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListFormatSettings {
    pub locale: DataLocale,
    pub kind: ListFormatType,
    pub style: ListFormatStyle,
}

impl ListFormatSettings {
    /// The formatter that reads the template set of 14.2.3 this key names: `locale` picks the locale's
    /// data, `kind` which of its three lists and `style` which of that list's three lengths. Note 2 of
    /// 14.2.3 maps this engine's option names onto the data's own: `conjunction` is the "standard"
    /// list, `disjunction` the "or" one and `unit` the "unit" one.
    ///
    /// A `DataError` here means the baked data has no list marker for a built-in locale, which
    /// `brimstone_intl_data`'s tests refuse; no program can bring it about.
    fn build_formatter(&self) -> Result<Rc<ListFormatter>, ()> {
        let locale = self
            .locale
            .tag()
            .parse::<IcuLocale>()
            // The tags come out of `BUILT_IN_LOCALES`, which the build script wrote from subtags
            // `icu_locale` itself parsed.
            .map_err(|_| ())?;

        let preferences = ListFormatterPreferences::from(&locale);
        let options = ListFormatterOptions::default().with_length(self.style.as_icu_length());

        let formatter = match self.kind {
            ListFormatType::Conjunction => {
                ListFormatter::try_new_and_unstable(&IntlDataProvider, preferences, options)
            }
            ListFormatType::Disjunction => {
                ListFormatter::try_new_or_unstable(&IntlDataProvider, preferences, options)
            }
            ListFormatType::Unit => {
                ListFormatter::try_new_unit_unstable(&IntlDataProvider, preferences, options)
            }
        };

        formatter.map(Rc::new).map_err(|_| ())
    }

    /// The templates of 14.4, built on first use and shared by every instance of the same three
    /// options after that.
    ///
    /// The failure this swallows is a build without one of the three list markers for a built-in
    /// locale, which that crate's tests and this module's own both refuse.
    pub fn build_formatter_or_panic(&self) -> Rc<ListFormatter> {
        self.build_formatter()
            .expect("every built in locale has and, or and unit list patterns in three lengths")
    }
}

extend_object! {
    /// Properties of Intl.ListFormat Instances
    /// (https://tc39.es/ecma402/#sec-properties-of-intl-listformat-instances), 14.4.
    ///
    /// Nothing here is an ICU4X object, which a GC object may not hold: the templates are the cache's,
    /// and what an instance keeps is the key they are cached under.
    pub struct ListFormatObject {
        /// What 14.1.1 steps 6 to 10 computed. Its absence is this engine's spelling of
        /// `[[InitializedListFormat]]`, which 14.3.2 step 2, 14.3.3 step 2 and 14.3.4 step 2 each
        /// require, and which a constructor that threw leaves unset.
        resolved: Option<ResolvedListFormat>,
    }
}

/// The slots 14.1.1 resolves, carried with the options so that one `Option` on the instance says
/// whether it is initialized at all.
#[derive(Clone, Copy)]
pub struct ResolvedListFormat {
    /// [[Locale]] (14.1.1 step 6), which 14.3.2 reports as `locale`.
    pub locale: Value,

    /// [[Type]] (step 8) and [[Style]] (step 10) with the data locale of step 6: the three things the
    /// templates of steps 11 to 13 are cached under, and the last two of what 14.3.2 reports.
    pub settings: ListFormatSettings,
}

impl ResolvedListFormat {
    /// The `type` of the [[Templates]] of 14.4, which is what 14.3.2 reports as `type`.
    pub fn kind(&self) -> ListFormatType {
        self.settings.kind
    }

    /// The length of the template set, which is what 14.3.2 reports as `style`.
    pub fn style(&self) -> ListFormatStyle {
        self.settings.style
    }
}

impl ListFormatObject {
    /// 14.1.1 steps 1 and 2: `OrdinaryCreateFromConstructor(newTarget,
    /// "%Intl.ListFormat.prototype%", « [[InitializedListFormat]], [[Locale]], [[Type]], [[Style]],
    /// [[Templates]] »)`, every one of them empty. Like PluralRules' and unlike NumberFormat's and
    /// Collator's, this constructor has no legacy call mode: 14.1.1 step 1 throws when NewTarget is
    /// undefined, so there is no %Intl%.[[FallbackSymbol]] instance to make and no ChainListFormat.
    pub fn new(cx: Context, new_target: Handle<ObjectValue>) -> EvalResult<HeapPtr<Self>> {
        // Step 2, through `ObjectBuilder`, which reads `newTarget.prototype` and falls back to the
        // intrinsic the spec names: OrdinaryCreateFromConstructor steps 1 and 2.
        let mut list_format = ObjectBuilder::<Self>::new(cx)
            .constructor_proto(new_target, Intrinsic::ListFormatPrototype)?
            .build()?;

        // The slots are empty as step 2 leaves them, and stay so until `initialize` writes the whole
        // record in one go.
        list_format.resolved = None;

        Ok(list_format)
    }

    /// `? RequireInternalSlot(lf, [[InitializedListFormat]])` of 14.3.2 step 2, 14.3.3 step 2 and
    /// 14.3.4 step 2, on the `this` value itself. There is no UnwrapListFormat in front of it: 14.1.1
    /// step 1 refuses the only call mode that would have put an instance behind a fallback symbol.
    pub fn require_initialized(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(cx, "Intl.ListFormat method called on incompatible receiver");
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(instance) if instance.resolved.is_some() => Ok(instance),
            _ => type_error(cx, "Intl.ListFormat method called on an uninitialized instance"),
        }
    }

    /// The resolved options of an initialized instance: 14.3.2's `resolvedOptions` reads them through
    /// Table 25, and the formatting below works from them.
    pub fn resolved(&self) -> Option<ResolvedListFormat> {
        self.resolved
    }

    /// 14.1.1 (https://tc39.es/ecma402/#sec-intl-listformat-constructor) steps 3 to 13.
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
        // Steps 3 to 5: ResolveOptions (https://tc39.es/ecma402/#sec-resolveoptions), 9.2.8. This is
        // the one service whose call of it names no special behaviours, so its step 3 reads the
        // options bag with GetOptionsObject rather than CoerceOptionsToObject: a number or a Boolean
        // there is the TypeError the clause means rather than a quietly wrapped primitive. Its step 1
        // still reads the locale list first, which is what makes a getter on `locales` run before one
        // on the options.
        let requested_locales = canonicalize_locale_list_argument(cx, locales)?;
        let options = get_options_object(cx, options)?;

        // 9.2.8 step 4.
        let matcher = get_name_option(
            cx,
            options,
            cx.names.locale_matcher(),
            &["best fit", "lookup"],
            "best fit",
        )?;

        // Steps 5 and 6 through ResolveLocale (https://tc39.es/ecma402/#sec-resolvelocale), 9.2.7,
        // over 14.2.3's [[AvailableLocales]] and with both its [[RelevantExtensionKeys]] and its
        // [[ResolutionOptionDescriptors]] empty: no option is read as part of resolving the locale,
        // and no `-u-` extension of a requested tag survives into the reported one.
        let resolved = resolve_locale(
            &requested_locales,
            if matcher == "lookup" {
                Matcher::Lookup
            } else {
                Matcher::BestFit
            },
        );

        // Step 6.
        let locale = cx.alloc_string(&resolved.locale)?.as_value();

        // Steps 11 to 13 pick the templates by the built-in locale the resolved tag names, and 14.2.3's
        // [[AvailableLocales]] is exactly the set this build was generated with, so a tag outside it
        // means data is missing rather than that the caller can be answered in another locale.
        let Ok(settings_locale) = DataLocale::try_from(resolved.data_locale.as_str()) else {
            return range_error(
                cx,
                &format!(
                    "Intl.ListFormat resolved the locale '{}' but this build has no data for it",
                    resolved.data_locale
                ),
            );
        };

        // Steps 7 and 8.
        let kind = get_enum_option(
            cx,
            options,
            cx.names.type_(),
            &ListFormatType::NAMES,
            "conjunction",
            ListFormatType::from_name,
        )?;

        // Steps 9 and 10.
        let style = get_enum_option(
            cx,
            options,
            cx.names.style(),
            &ListFormatStyle::NAMES,
            "long",
            ListFormatStyle::from_name,
        )?;

        instance.resolved = Some(ResolvedListFormat {
            locale: *locale,
            settings: ListFormatSettings { locale: settings_locale, kind, style },
        });

        Ok(())
    }

    /// The formatter these settings select, from the per-context cache of them.
    fn formatter(&self, cx: Context) -> EvalResult<Rc<ListFormatter>> {
        let Some(resolved) = self.resolved() else {
            return type_error(cx, "Intl.ListFormat method called on an uninitialized instance");
        };

        Ok(cx.locale_cache().list_formatter(resolved.settings))
    }

    /// FormatList (https://tc39.es/ecma402/#sec-formatlist), 14.5.3, which is 14.5.2's list of parts
    /// with their text concatenated: the same pass as [`Self::parts`], so the string and the parts
    /// cannot disagree.
    pub fn format(&self, cx: Context, list: &[Wtf8String]) -> EvalResult<Wtf8String> {
        let parts = self.parts(cx, list)?;

        let mut text = Wtf8String::new();
        for part in &parts {
            text.push_wtf8_str(part.value.as_str());
        }

        Ok(text)
    }

    /// FormatListToParts (https://tc39.es/ecma402/#sec-formatlisttoparts) step 4:
    /// CreatePartsFromList (https://tc39.es/ecma402/#sec-createpartsfromlist), 14.5.2, for the
    /// templates of 14.4 and the list that was read from the argument.
    pub fn parts(&self, cx: Context, list: &[Wtf8String]) -> EvalResult<Vec<ListPart>> {
        let formatter = self.formatter(cx)?;

        Ok(create_parts_from_list(&formatter, list))
    }

    /// StringListFromIterable (https://tc39.es/ecma402/#sec-createstringlistfromiterable), 14.5.5,
    /// which is what both 14.3.3 step 3 and 14.3.4 step 3 do before formatting anything.
    pub fn string_list_from_iterable(
        cx: Context,
        iterable: Handle<Value>,
    ) -> EvalResult<Vec<Wtf8String>> {
        // Step 1: an absent list is the empty list, which is why `format()` answers `""` rather than
        // throwing on the undefined it was called with.
        if iterable.is_undefined() {
            return Ok(Vec::new());
        }

        // Step 2.
        let mut iterator = get_iterator(cx, iterable, IteratorHint::Sync, None)?;

        // Steps 3 and 4.
        let mut list = Vec::new();
        while let Some(element) = iterator_step_value(cx, &mut iterator)? {
            // Step 4.3: only a String is a place of the list, and there is no coercion to fall back
            // on, which is what the clause's own note says - the reason reading a list does not look
            // like reading an option.
            if !element.is_string() {
                let error = type_error::<Handle<Value>>(
                    cx,
                    "Intl.ListFormat can only format a list of strings",
                );

                // The error leaves through IteratorClose (ECMA-262 7.4.5), so a generator's `finally`
                // runs before the method reports it.
                return match iterator_close(cx, iterator.iterator, error) {
                    // IteratorClose step 2: an abrupt completion is what comes back, never a value.
                    Ok(_) => {
                        unreachable!("an error completion always leaves IteratorClose an error")
                    }
                    Err(error) => Err(error),
                };
            }

            // Step 4.2, and the text is copied out here rather than when it is formatted: a place says
            // what the list held when it was read.
            list.push(element.as_string().to_wtf8_string()?);
        }

        Ok(list)
    }
}

impl HeapItem for ListFormatObject {
    fn byte_size(list_format: HeapPtr<Self>) -> usize {
        list_format.object_byte_size()
    }

    fn visit_pointers(mut list_format: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        list_format.visit_object_pointers(visitor);

        if let Some(resolved) = &mut list_format.resolved {
            visitor.visit_value(&mut resolved.locale);
        }
    }
}

/// One of the two record types CreatePartsFromList
/// (https://tc39.es/ecma402/#sec-createpartsfromlist), 14.5.2, is defined to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListPartKind {
    /// `[[Type]]: "element"`: one place of the list.
    Element,

    /// `[[Type]]: "literal"`: the text between two places, which is what a template's literal parts
    /// are.
    Literal,
}

impl ListPartKind {
    /// The `type` of the record 14.5.4 step 4.2 makes, which is the name the spec gives the slot.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Element => "element",
            Self::Literal => "literal",
        }
    }
}

/// One part of a formatted list: 14.5.2's record, whose `[[Value]]` 14.5.4 step 4.2 makes into a
/// String. The text is WTF-8 because a place of the list can hold a lone surrogate and 14.5.5 says
/// nothing that would make it anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPart {
    pub kind: ListPartKind,
    pub value: Wtf8String,
}

/// The text `icu_list` is handed for a place of the list that is not empty. A formatter reads a
/// place's text for one thing only: whether it is empty, which is what decides the pair pattern read
/// without its tail. What it writes back for the place is replaced with the list's own text below, so
/// nothing a program wrote is ever substituted for.
const ELEMENT_PLACEHOLDER: &str = "\u{0}";

/// CreatePartsFromList (https://tc39.es/ecma402/#sec-createpartsfromlist), 14.5.2.
pub fn create_parts_from_list(formatter: &ListFormatter, list: &[Wtf8String]) -> Vec<ListPart> {
    let mut sink = ListPartsSink::default();

    let placeholders: Vec<&str> = list
        .iter()
        .map(|text| {
            if text.is_empty() {
                ""
            } else {
                ELEMENT_PLACEHOLDER
            }
        })
        .collect();

    // Step 1 and steps 3 to 8 are what one formatting pass of `icu_list` is: every place written
    // once, and every span of the templates between them written as it comes. A sink only ever pushes
    // onto a `Vec` or appends to a buffer, so the only thing that can come back is success; a debug
    // build checks that instead of an `unwrap` carrying a panic into a call. The pass itself is
    // outside the check on purpose: inside `debug_assert!` a release build would not run it at all,
    // and would produce no parts.
    let _written = formatter
        .format(placeholders.iter())
        .write_to_parts(&mut sink);

    debug_assert!(_written.is_ok(), "writing into a sink of memory does not fail");

    // Step 4.2 and step 7.2: the text of a part that is one of the list's places is that place. A
    // formatter writes one part per place, in the order of the list, so the two lists line up.
    let mut places = list.iter();
    for part in &mut sink.parts {
        if part.kind == ListPartKind::Element {
            part.value = places
                .next()
                .expect("a formatted list has one part for every place of it")
                .clone();
        }
    }

    sink.parts
}

/// The collector the parts of 14.5.2 are read out of a `writeable` pass with.
///
/// An element is a part even when its text is empty, because 14.5.2 appends a record for the place
/// whatever it holds; an empty literal is not, which is what PartitionPattern
/// (https://tc39.es/ecma402/#sec-partitionpattern), 9.2.15 step 2.2 and step 6, says of a pattern that
/// begins or ends with a placeholder - and of the empty tail of the pair pattern that 14.5.2's step
/// 7.2 reads without.
#[derive(Default)]
struct ListPartsSink {
    parts: Vec<ListPart>,

    /// The kind of part the text being written right now belongs to, which `with_part` sets and
    /// clears again. Text with no part around it is the text between two places.
    open: Option<ListPartKind>,
}

impl PartsWrite for ListPartsSink {
    type SubPartsWrite = Self;

    fn with_part(
        &mut self,
        part: Part,
        mut write: impl FnMut(&mut Self::SubPartsWrite) -> std::fmt::Result,
    ) -> std::fmt::Result {
        self.open = part_kind(part);

        let result = write(self);

        self.open = None;
        result
    }
}

impl std::fmt::Write for ListPartsSink {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let kind = self.open.unwrap_or(ListPartKind::Literal);

        if kind == ListPartKind::Literal && text.is_empty() {
            return Ok(());
        }

        self.parts
            .push(ListPart { kind, value: Wtf8String::from_str(text) });

        Ok(())
    }
}

/// The record type of 14.5.2 that an annotation of `icu_list`'s says its text was. A list formatter
/// annotates a place of the list with `element` and the text between two places with `literal`, which
/// are the two the clause allows and nothing else.
fn part_kind(part: Part) -> Option<ListPartKind> {
    match part.value {
        "element" => Some(ListPartKind::Element),
        "literal" => Some(ListPartKind::Literal),
        _ => None,
    }
}

/// The `{ type, value }` record of 14.5.4 step 4.2, under the two property names the caller read once
/// for the whole array.
pub fn part_object(
    mut cx: Context,
    part: &ListPart,
    kind: Handle<PropertyKey>,
    value: Handle<PropertyKey>,
) -> EvalResult<Handle<ObjectValue>> {
    let record = ordinary_object_create(cx)?;

    create_data_property_or_throw(
        cx,
        record,
        kind,
        cx.alloc_string(part.kind.as_str())?.as_value(),
    )?;
    create_data_property_or_throw(
        cx,
        record,
        value,
        cx.alloc_wtf8_string(&part.value)?.as_value(),
    )?;

    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formatter one set of resolved options names, built the way the cache builds it.
    fn formatter(locale: &str, kind: ListFormatType, style: ListFormatStyle) -> Rc<ListFormatter> {
        ListFormatSettings {
            locale: DataLocale::try_from(locale).expect("a built in locale"),
            kind,
            style,
        }
        .build_formatter_or_panic()
    }

    /// CreatePartsFromList for a list a test wrote out, in the same shape 14.5.5 hands it over.
    fn parts(
        locale: &str,
        kind: ListFormatType,
        style: ListFormatStyle,
        list: &[&str],
    ) -> Vec<ListPart> {
        let list: Vec<Wtf8String> = list.iter().map(|text| Wtf8String::from_str(text)).collect();

        create_parts_from_list(&formatter(locale, kind, style), &list)
    }

    /// The text of every part as UTF-8 a test can write out: the cases below are about the templates,
    /// and a lone surrogate has a test of its own.
    fn texts(parts: &[ListPart]) -> Vec<String> {
        parts
            .iter()
            .map(|part| String::from_utf8_lossy(part.value.as_bytes()).into_owned())
            .collect()
    }

    fn kinds(parts: &[ListPart]) -> Vec<&'static str> {
        parts.iter().map(|part| part.kind.as_str()).collect()
    }

    /// FormatList (14.5.3) for the same list, which is the parts' text.
    fn format(locale: &str, kind: ListFormatType, style: ListFormatStyle, list: &[&str]) -> String {
        texts(&parts(locale, kind, style, list)).concat()
    }

    #[test]
    fn every_built_in_locale_has_templates_of_every_type_and_style() {
        for locale in brimstone_intl_data::BUILT_IN_LOCALES {
            for kind in [
                ListFormatType::Conjunction,
                ListFormatType::Disjunction,
                ListFormatType::Unit,
            ] {
                for style in [
                    ListFormatStyle::Long,
                    ListFormatStyle::Short,
                    ListFormatStyle::Narrow,
                ] {
                    // 14.2.3's template set carries Pair, Start, Middle and End for every length, and
                    // a list of four is the shortest case that reads all of them.
                    let parts = parts(locale, kind, style, &["A", "B", "C", "D"]);

                    assert_eq!(
                        parts
                            .iter()
                            .filter(|part| part.kind == ListPartKind::Element)
                            .count(),
                        4,
                        "{locale} {kind:?} {style:?}"
                    );
                    assert!(!texts(&parts).concat().is_empty(), "{locale} {kind:?} {style:?}");
                }
            }
        }
    }

    #[test]
    fn a_pair_is_the_pair_template_and_nothing_else() {
        let parts = parts("en", ListFormatType::Conjunction, ListFormatStyle::Long, &["A", "B"]);

        // 14.5.2 step 3: two places read the `Pair` template, and neither `Start` nor `End` of the
        // same data is anywhere in the answer.
        assert_eq!(kinds(&parts), ["element", "literal", "element"]);
        assert_eq!(texts(&parts), ["A", " and ", "B"]);
    }

    #[test]
    fn a_longer_list_is_the_start_middle_and_end_templates_in_order() {
        let parts =
            parts("en", ListFormatType::Conjunction, ListFormatStyle::Long, &["A", "B", "C", "D"]);

        // Steps 7.2 to 7.5: `Start` around the first two places, `Middle` for the ones between and
        // `End` for the last two, so a list of four has two of the locale's plain separators and one
        // of its serial-comma separator.
        assert_eq!(
            kinds(&parts),
            [
                "element", "literal", "element", "literal", "element", "literal", "element"
            ]
        );
        assert_eq!(texts(&parts), ["A", ", ", "B", ", ", "C", ", and ", "D"]);
    }

    #[test]
    fn a_one_element_list_is_the_element_and_an_empty_list_is_nothing() {
        assert_eq!(
            parts("en", ListFormatType::Conjunction, ListFormatStyle::Long, &["A"]),
            [ListPart {
                kind: ListPartKind::Element,
                value: Wtf8String::from_str("A")
            }]
        );

        // 14.5.2 steps 1 and 2: an empty list has no parts, so 14.5.3 concatenates nothing at all.
        assert!(parts("en", ListFormatType::Conjunction, ListFormatStyle::Long, &[]).is_empty());
        assert_eq!(format("en", ListFormatType::Conjunction, ListFormatStyle::Long, &[]), "");
    }

    #[test]
    fn the_three_types_are_the_three_lists_of_the_locale_data() {
        let long = ListFormatStyle::Long;

        assert_eq!(format("en", ListFormatType::Conjunction, long, &["A", "B"]), "A and B");
        assert_eq!(format("en", ListFormatType::Disjunction, long, &["A", "B"]), "A or B");

        // Note 3 of 14.2.3: a `unit` list is the bare comma list, for values that name units.
        assert_eq!(
            format("en", ListFormatType::Unit, long, &["5 pounds", "12 ounces"]),
            "5 pounds, 12 ounces"
        );
    }

    #[test]
    fn the_three_styles_are_the_three_lengths_of_the_same_list() {
        let conjunction = ListFormatType::Conjunction;

        assert_eq!(
            format("en", conjunction, ListFormatStyle::Long, &["A", "B", "C"]),
            "A, B, and C"
        );
        assert_eq!(
            format("en", conjunction, ListFormatStyle::Short, &["A", "B", "C"]),
            "A, B, & C"
        );
        assert_eq!(format("en", conjunction, ListFormatStyle::Narrow, &["A", "B", "C"]), "A, B, C");
    }

    #[test]
    fn danish_and_italian_read_their_own_templates() {
        let long = ListFormatStyle::Long;

        // 14.2.3's LocaleData is per locale: da connects with "og" and "eller" and it with "e" and
        // "o", and neither has the serial comma English has.
        assert_eq!(format("da", ListFormatType::Conjunction, long, &["A", "B"]), "A og B");
        assert_eq!(
            format("da", ListFormatType::Disjunction, long, &["A", "B", "C"]),
            "A, B eller C"
        );
        assert_eq!(format("it", ListFormatType::Conjunction, long, &["A", "B"]), "A e B");
        assert_eq!(format("it", ListFormatType::Disjunction, long, &["A", "B"]), "A o B");
        assert_eq!(format("it", ListFormatType::Conjunction, long, &["A", "B", "C"]), "A, B e C");
    }

    #[test]
    fn an_empty_element_is_a_part_and_an_empty_literal_is_not() {
        assert_eq!(
            texts(&parts("en", ListFormatType::Conjunction, ListFormatStyle::Long, &["", ""])),
            ["", " and ", ""]
        );

        // DeconstructPattern step 3.4.1 appends a place whatever it holds, where PartitionPattern step
        // 2.2 and step 6 drop a literal that came out empty, and nothing merges two literals that end
        // up neighbours. So this list of three keeps its empty place as a part of its own and the two
        // separators the templates made as two parts: the text is what V8 gives too, its parts are not
        // (see the table in NOTES-intl-small-four.md).
        let parts =
            parts("en", ListFormatType::Conjunction, ListFormatStyle::Long, &["A", "", "B"]);
        assert_eq!(kinds(&parts), ["element", "literal", "element", "literal", "element"]);
        assert_eq!(texts(&parts), ["A", ", ", "", ", and ", "B"]);
    }

    #[test]
    fn text_that_is_not_ascii_stays_in_one_part() {
        let parts = parts("en", ListFormatType::Conjunction, ListFormatStyle::Narrow, &["🎉", "a"]);

        assert_eq!(kinds(&parts), ["element", "literal", "element"]);
        assert_eq!(texts(&parts), ["🎉", ", ", "a"]);

        // A part boundary is a byte offset into the formatting, and a place of the list is written
        // whole, so an astral character cannot be split across two parts.
        assert_eq!(parts[0].value.as_bytes().len(), 4);
    }

    #[test]
    fn a_lone_surrogate_survives_the_formatting() {
        // 14.5.5 hands the String over as it is and the text of a part is the place's own rather than
        // anything read through UTF-8, so the half a program put in a list comes back out of it.
        let half = Wtf8String::from_bytes_unchecked(&[0xed, 0xa0, 0x80]);
        let list = vec![half.clone(), Wtf8String::from_str("B")];

        let parts = create_parts_from_list(
            &formatter("en", ListFormatType::Conjunction, ListFormatStyle::Long),
            &list,
        );

        assert_eq!(parts[0].value, half);
        assert_eq!(parts[2].value, Wtf8String::from_str("B"));
    }

    #[test]
    fn every_list_has_one_part_per_place_in_order_and_no_neighbouring_literals() {
        for locale in brimstone_intl_data::BUILT_IN_LOCALES {
            for kind in [
                ListFormatType::Conjunction,
                ListFormatType::Disjunction,
                ListFormatType::Unit,
            ] {
                for style in [
                    ListFormatStyle::Long,
                    ListFormatStyle::Short,
                    ListFormatStyle::Narrow,
                ] {
                    for len in 0..6 {
                        let parts = parts(locale, kind, style, &vec!["AB"; len]);

                        // 14.5.2's return: every place of the list is exactly one part.
                        assert_eq!(
                            parts
                                .iter()
                                .filter(|part| part.kind == ListPartKind::Element)
                                .count(),
                            len,
                            "{locale} {kind:?} {style:?} {len}"
                        );

                        // and there is no separator between two separators.
                        for pair in parts.windows(2) {
                            assert!(
                                !(pair[0].kind == ListPartKind::Literal
                                    && pair[1].kind == ListPartKind::Literal),
                                "{locale} {kind:?} {style:?} {len}"
                            );
                        }
                    }
                }
            }
        }
    }
}
