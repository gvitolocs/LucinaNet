//! The `Intl.Segmenter` instance (https://tc39.es/ecma402/#sec-properties-of-intl-segmenter-instances),
//! 19.4, the option resolution of 19.1.1, and the segmentation that 19.8.1 FindBoundary answers.
//!
//! An instance holds what 19.4 lists and nothing else: the resolved locale and
//! [[SegmenterGranularity]]. The ICU4X segmenters own buffers and cannot sit on a GC object
//! (`gc/heap_item.rs:158` refuses any heap item that needs a destructor), so they live in
//! [`super::cache::LocaleCache`] under the [`SegmenterSettings`] an instance keeps instead, exactly
//! as the collators and list formatters do.
//!
//! The clause number here is 19, not 18: in the draft this build follows (ECMA-402 14th edition,
//! 2026-10-23) RelativeTimeFormat took 18 and Segmenter moved to 19.

use std::rc::Rc;

use brimstone_intl_data::IntlDataProvider;
use icu_segmenter::{
    GraphemeClusterSegmenter, SentenceSegmenter, WordSegmenter,
    options::{SentenceBreakOptions, WordBreakOptions},
};

use super::cache::DataLocale;
use super::intl_object::canonicalize_locale_list_argument;
use super::locale_negotiation::{Matcher, resolve_locale};
use super::options::{coerce_options_to_object, get_enum_option, get_name_option};
use crate::extend_object;
use crate::runtime::{
    Context, Handle, Value,
    error::{range_error, type_error},
    eval_result::EvalResult,
    gc::{HeapItem, HeapPtr, HeapVisitor},
    intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue,
    ordinary_object::ObjectBuilder,
};

/// [[SegmenterGranularity]] (https://tc39.es/ecma402/#sec-intl-segmenter-constructor), 19.1.1
/// step 9, which step 8 read through GetOption over exactly these three names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmenterGranularity {
    Grapheme,
    Word,
    Sentence,
}

impl SegmenterGranularity {
    /// The three names 19.1.1 step 8 lists as GetOption's `values`, in that order, which is also
    /// the order the row of 19.8.1 and the probe cases below are written in.
    pub const NAMES: [&'static str; 3] = ["grapheme", "word", "sentence"];

    /// The String value 19.3.2 reports for it through Table 34, and the option value 19.1.1 step 8
    /// accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grapheme => "grapheme",
            Self::Word => "word",
            Self::Sentence => "sentence",
        }
    }

    /// 19.1.1 step 8 has already refused everything but the three names of this enum, so the only
    /// question this answers is which of them was said.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "grapheme" => Some(Self::Grapheme),
            "word" => Some(Self::Word),
            "sentence" => Some(Self::Sentence),
            _ => None,
        }
    }
}

/// The instance's resolved locale and granularity as the `Copy` key of the cache entry that holds
/// its ICU4X segmenter.
///
/// The data locale rather than the resolved tag, because a request for "en-US" segments on the "en"
/// data and the tag itself names no entry the break tables have: the same reason
/// [`super::cache::LocaleCache`] keys its decimal renderers by the data locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SegmenterSettings {
    pub locale: DataLocale,
    pub granularity: SegmenterGranularity,
}

impl SegmenterSettings {
    /// The ICU4X segmenter these options describe.
    ///
    /// Every one of the three is built rule-based and with the resolved locale as its *content*
    /// locale: 19.8.1 leaves boundary determination to the implementation with UTS #29's general
    /// algorithms and CLDR's tailorings recommended, and passing the locale is what makes the
    /// tailorings apply. Omitting it is the easy mistake to make and it silently changes word and
    /// sentence boundaries.
    ///
    /// The only way this fails is missing data, and the data is compiled into this binary, which is
    /// what lets the one cache entry per distinct settings unwrap here rather than pass an error up:
    /// `brimstone_intl_data`'s own tests build all three segmenters in every built-in locale.
    pub fn build_segmenter(&self) -> NativeSegmenter {
        let locale = self
            .locale
            .tag()
            .parse::<icu_locale::Locale>()
            .expect("a built in locale tag parses");

        match self.granularity {
            // Grapheme boundaries depend on no locale at all: UAX #29's grapheme cluster rules are
            // invariant, which is why this constructor takes no options.
            SegmenterGranularity::Grapheme => NativeSegmenter::Grapheme(
                GraphemeClusterSegmenter::try_new_unstable(&IntlDataProvider).unwrap(),
            ),
            // The rule-based word segmenter, which is the one this build ships: the `auto` and
            // `lstm` features are off in the workspace pin, so the dictionary and LSTM models for
            // Chinese, Japanese, Thai, Lao, Myanmar and Khmer are not even nameable here. What that
            // means for a string in one of those scripts is spelled out in
            // `NativeSegmenter::segment` and in the notes.
            SegmenterGranularity::Word => {
                // The options are `non_exhaustive`, so the one field this build sets is written
                // onto the default rather than named in a struct literal.
                let mut options = WordBreakOptions::default();
                options.content_locale = Some(&locale.id);

                NativeSegmenter::Word(
                    WordSegmenter::try_new_for_non_complex_scripts_unstable(
                        &IntlDataProvider,
                        options,
                    )
                    .unwrap(),
                )
            }
            SegmenterGranularity::Sentence => {
                let mut options = SentenceBreakOptions::default();
                options.content_locale = Some(&locale.id);

                NativeSegmenter::Sentence(
                    SentenceSegmenter::try_new_unstable(&IntlDataProvider, options).unwrap(),
                )
            }
        }
    }
}

/// The segmenter one [`SegmenterSettings`] names, as the object that actually breaks text.
///
/// The three ICU4X types have no common trait for the UTF-16 entry point, so the enum is what the
/// cache holds and what [`Self::segment`] dispatches on.
// The word engine's payload is the largest of the three and an enum variant is inline, but this is
// only ever reached through the `Rc` the cache hands out: boxing would buy a few hundred bytes of
// the cached object with an extra allocation per segmenter.
#[allow(clippy::large_enum_variant)]
pub enum NativeSegmenter {
    Grapheme(GraphemeClusterSegmenter),
    Word(WordSegmenter),
    Sentence(SentenceSegmenter),
}

/// The boundaries of one string, in UTF-16 code units: FindBoundary (19.8.1) answered for every
/// position at once rather than one at a time.
///
/// `boundaries` is the full increasing list, starting at 0 and ending at the string length, which
/// are exactly the two fallbacks 19.8.1 steps 3 and 9 give. The segments are the half-open ranges
/// `boundaries[i]..boundaries[i + 1]`, so its length is one more than the number of segments.
///
/// `word_like` answers 19.7.1 step 10's "a Boolean indicating whether the segment in string is
/// 'word-like'" for each segment, and is only ever filled for word granularity: for grapheme and
/// sentence the property must be absent from the segment data object rather than present and
/// `false`, which is what the `Option` on the Segments object is for.
pub struct Segmentation {
    pub boundaries: Vec<u32>,
    pub word_like: Vec<bool>,
}

impl NativeSegmenter {
    /// Every boundary of `units`, which is the string's UTF-16 code units, so the indices this
    /// produces are the ones ECMA-402 counts - 19.8.1 works in code units throughout, and 6.2.9's
    /// "the code unit index" is what the segment data object's `index` reports.
    ///
    /// Cost is one pass over the string, which is why the answer is stored on the Segments object
    /// instead of being recomputed: the break iterators borrow their input and ICU4X 2.3 gives no
    /// way to resume one from a position, so a `next()` that did not read the table would have to
    /// rescan from 0 and a full iteration would be quadratic. That is Boa's advance
    /// (`segmenter/iterator.rs:124-136`), which this build rejects on purpose.
    ///
    /// Complex scripts, which is the case a dictionary or an LSTM model would otherwise own: the
    /// rule engine hands a run of Thai, Lao, Khmer, Myanmar, Chinese or Japanese code points to
    /// `ComplexPayloadsBorrowed::segment_utf16`, this build has no model for any of them, and that
    /// function's answer for a script with no model is one boundary at the end of the run. So the
    /// whole run comes back as a single segment, which is what the notes record as a known limit -
    /// it is the segmenter's own documented fallback, not an invented word.
    pub fn segment(&self, units: &[u16]) -> Segmentation {
        let length = units.len();

        let (mut boundaries, word_like) = match self {
            Self::Grapheme(segmenter) => (
                segmenter
                    .as_borrowed()
                    .segment_utf16(units)
                    .map(|boundary| boundary as u32)
                    .collect(),
                Vec::new(),
            ),
            Self::Sentence(segmenter) => (
                segmenter
                    .as_borrowed()
                    .segment_utf16(units)
                    .map(|boundary| boundary as u32)
                    .collect(),
                Vec::new(),
            ),
            // The pairs are each boundary together with the type of the segment that *ends* at it,
            // so the leading 0 boundary reports nothing about a segment and its flag is dropped:
            // the flag of segment `i` is the one that arrived with `boundaries[i + 1]`.
            Self::Word(segmenter) => {
                let mut boundaries: Vec<u32> = Vec::new();
                let mut word_like = Vec::new();

                for (boundary, word_type) in segmenter
                    .as_borrowed()
                    .segment_utf16(units)
                    .iter_with_word_type()
                {
                    if boundaries.is_empty() {
                        boundaries.push(0);
                        continue;
                    }

                    word_like.push(word_type.is_word_like());
                    boundaries.push(boundary as u32);
                }

                (boundaries, word_like)
            }
        };

        // The two fallbacks of 19.8.1: a string always ends with a boundary at its own length, and
        // the empty string is the one case with a boundary and no segments.
        debug_assert_eq!(boundaries.first(), Some(&0), "segmentation starts at 0");
        if boundaries.last() != Some(&(length as u32)) {
            boundaries.push(length as u32);
        }

        Segmentation { boundaries, word_like }
    }
}

extend_object! {
    /// Properties of Intl.Segmenter Instances
    /// (https://tc39.es/ecma402/#sec-properties-of-intl-segmenter-instances), 19.4.
    ///
    /// Nothing here is an ICU4X object, which a GC object may not hold: the segmenter is the
    /// cache's, and what an instance keeps is the key it is cached under.
    pub struct SegmenterObject {
        /// What 19.1.1 steps 6 to 9 computed. Its absence is this engine's spelling of
        /// `[[InitializedSegmenter]]`, which 19.3.2 step 2 and 19.3.3 step 2 each require, and
        /// which a constructor that threw leaves unset.
        resolved: Option<ResolvedSegmenter>,
    }
}

/// The slots 19.1.1 resolves, carried with the options so that one `Option` on the instance says
/// whether it is initialized at all.
#[derive(Clone, Copy)]
pub struct ResolvedSegmenter {
    /// [[Locale]] (19.1.1 step 7), which 19.3.2 reports as `locale`. A `Value` rather than a string
    /// handle because a heap item is `Copy`: it is rooted on read, as the other services root theirs.
    pub locale: Value,
    /// [[Locale]] as the data locale the segmentation runs on, and [[SegmenterGranularity]]
    /// (19.1.1 step 9), which together are what the segmenter is cached under.
    pub settings: SegmenterSettings,
}

impl SegmenterObject {
    /// 19.1.1 steps 2 and 3: `OrdinaryCreateFromConstructor(newTarget,
    /// "%Intl.Segmenter.prototype%", « [[InitializedSegmenter]], [[Locale]],
    /// [[SegmenterGranularity]] »)`, every one of them empty. 19.1.1 step 1 refuses a call without
    /// `new`, so this constructor has no legacy call mode and no %Intl%.[[FallbackSymbol]] instance:
    /// `Reflect.construct` and a subclass's `Symbol.construct` are the only ways an instance is made
    /// without `new`.
    pub fn new(cx: Context, new_target: Handle<ObjectValue>) -> EvalResult<HeapPtr<Self>> {
        let mut segmenter = ObjectBuilder::<Self>::new(cx)
            .constructor_proto(new_target, Intrinsic::SegmenterPrototype)?
            .build()?;

        segmenter.resolved = None;

        Ok(segmenter)
    }

    /// `? RequireInternalSlot(segmenter, [[InitializedSegmenter]])` (19.3.2 step 2, 19.3.3 step 2).
    pub fn require_initialized(cx: Context, this_value: Handle<Value>) -> EvalResult<Handle<Self>> {
        if !this_value.is_object() {
            return type_error(cx, "Intl.Segmenter method called on incompatible receiver");
        }

        match this_value.as_object().as_opt::<Self>() {
            Some(instance) if instance.resolved.is_some() => Ok(instance),
            _ => type_error(cx, "Intl.Segmenter method called on an uninitialized instance"),
        }
    }

    /// The resolved options of an initialized instance: 19.3.2 reads them row by row through
    /// Table 34, and 19.3.3 works from them.
    pub fn resolved(&self) -> Option<ResolvedSegmenter> {
        self.resolved
    }

    /// 19.1.1 steps 4 to 9: ResolveOptions (9.2.8) and the reading of every option.
    ///
    /// The instance is a handle rather than `&mut self` on purpose, as in every other service here:
    /// every step below allocates, and a collection moves the block out from under a bare pointer,
    /// which the caller would then be handed back.
    pub fn initialize(
        mut instance: Handle<Self>,
        mut cx: Context,
        locales: Handle<Value>,
        options: Handle<Value>,
    ) -> EvalResult<()> {
        // Steps 4 and 5, which are 9.2.8 steps 1 and 3: the locale list is read before the options
        // bag, so a getter on the `locales` argument runs before one on the options.
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

        // Steps 6 and 7 through ResolveLocale (9.2.7), over 19.2.3's [[RelevantExtensionKeys]],
        // which 19.2.3 says is empty: nothing about segmentation depends on a `-u-` keyword, so no
        // keyword of a requested tag is kept and no option is looked up against a list of them.
        let resolved = resolve_locale(
            &requested_locales,
            if matcher == "lookup" {
                Matcher::Lookup
            } else {
                Matcher::BestFit
            },
        );

        // Step 7: [[Locale]] is the tag the request asked for, reported as the caller spelled it.
        let locale = cx.alloc_string(&resolved.locale)?.as_value();

        // 19.2.3's [[AvailableLocales]] is the set this build was generated with, and the fallback
        // of 9.2.7 step 4 is inside `resolve_locale`, so a tag outside that set means the data is
        // missing rather than that the caller can be answered in another locale.
        let Ok(data_locale) = DataLocale::try_from(resolved.data_locale.as_str()) else {
            return range_error(
                cx,
                &format!(
                    "Intl.Segmenter resolved the locale '{}' but this build has no data for it",
                    resolved.data_locale
                ),
            );
        };

        // Step 8, GetOption over the three names of 19.1.1 step 8 with "grapheme" as the default,
        // and step 9.
        let granularity = get_enum_option(
            cx,
            options,
            cx.names.granularity(),
            &SegmenterGranularity::NAMES,
            "grapheme",
            SegmenterGranularity::from_name,
        )?;

        instance.resolved = Some(ResolvedSegmenter {
            locale: *locale,
            settings: SegmenterSettings { locale: data_locale, granularity },
        });

        Ok(())
    }

    /// The segmenter this instance's options name, shared with every other instance of the same
    /// locale and granularity.
    pub fn segmenter(&self, cx: Context) -> Rc<NativeSegmenter> {
        let settings = self.settings().expect("required initialized");
        cx.locale_cache().segmenter(settings)
    }

    /// [[SegmenterGranularity]], 19.1.1 step 9 - the row of Table 34 that 19.3.2 reports and that
    /// 19.7.1 step 9 reads to decide whether `isWordLike` exists at all.
    pub fn granularity(&self) -> Option<SegmenterGranularity> {
        self.resolved.map(|resolved| resolved.settings.granularity)
    }

    fn settings(&self) -> Option<SegmenterSettings> {
        self.resolved.map(|resolved| resolved.settings)
    }
}

impl HeapItem for SegmenterObject {
    fn byte_size(segmenter: HeapPtr<Self>) -> usize {
        segmenter.object_byte_size()
    }

    fn visit_pointers(mut segmenter: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        segmenter.visit_object_pointers(visitor);

        if let Some(resolved) = &mut segmenter.resolved {
            visitor.visit_value(&mut resolved.locale);
        }
    }
}
