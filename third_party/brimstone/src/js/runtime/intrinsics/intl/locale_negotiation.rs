//! The locale half of Intl: the abstract operations of ECMA-402 section 6.2 and section 9.2 that
//! work on language tags alone. They take and return plain Rust values, so that which locale a
//! request resolves to can be checked without a JS context.

use std::sync::LazyLock;

use brimstone_intl_data::{BUILT_IN_LOCALES, DEFAULT_LOCALE, IntlDataProvider};
use icu_locale::subtags::{Language, Region, Script};
use icu_locale::{LanguageIdentifier, Locale, LocaleCanonicalizer, LocaleExpander};
use icu_locale_fallback::provider::LocaleLikelySubtagsLanguageV1;
use icu_provider::prelude::{DataProvider, DataRequest};

/// What went wrong in a locale operation, and which exception that becomes (see
/// `throw_locale_error` in `intl_object.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocaleError {
    /// The tag is not a well-formed language tag (6.2.1), which CanonicalizeLocaleList step 7.3.5
    /// reports as a RangeError.
    InvalidTag,

    /// A list element is neither a String nor an Object, which CanonicalizeLocaleList step 7.3.2
    /// reports as a TypeError. Only the JS entry points see this, since a plain Rust caller has
    /// already handed over the strings.
    NotAString,
}

/// The locales this build supports, its Available Locales List
/// (https://tc39.es/ecma402/#available-locales-list).
#[derive(Debug, Clone, Copy)]
pub struct BuiltInLocales {
    tags: &'static [&'static str],
}

impl BuiltInLocales {
    /// The set the baked data of `brimstone_intl_data` was generated for. It is fixed at build
    /// time; `LUCINAJS_INTL_LOCALES` changes it (see that crate's README).
    pub const fn new() -> Self {
        Self { tags: BUILT_IN_LOCALES }
    }

    /// The locales, sorted. An Available Locales List holds canonical identifiers, and the
    /// generator writes them out sorted, so the order costs nothing to keep.
    pub const fn tags(&self) -> &'static [&'static str] {
        self.tags
    }
}

/// DefaultLocale (https://tc39.es/ecma402/#sec-defaultlocale), 6.2.3. This build always falls back
/// to English; `brimstone_intl_data` checks at compile time that it is one of its built in locales,
/// so the fallback locale is never one that Intl then claims not to support.
pub fn default_locale() -> &'static str {
    DEFAULT_LOCALE
}

/// The CLDR alias and likely-subtags tables, for callers without a context. `cache::LocaleCache`
/// builds the same value once per context, which is what the JS entry points use.
static CANONICALIZER: LazyLock<LocaleCanonicalizer> = LazyLock::new(|| {
    LocaleCanonicalizer::try_new_extended_unstable(&IntlDataProvider)
        // The tables are baked into the binary, so the only reason this fails is that the data
        // crate was generated without one of them, which is a bug in the build, not in a program.
        .unwrap()
});

/// `options.[[localeMatcher]]`, step 1 of ResolveLocale (9.2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matcher {
    Lookup,

    /// LookupMatchingLocaleByBestFit (9.2.4) is implementation-defined; see `lookup_locale`.
    BestFit,
}

/// The record returned by LookupMatchingLocaleByPrefix (9.2.3). `locale` is the element of the
/// available locales that matched, `extension` the Unicode locale extension sequence of the
/// requested tag it matched, empty when the request had none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleMatch {
    pub locale: String,

    /// The matched requested tag with its Unicode extension taken off, so the region and script the
    /// request named: this is what a service that reports the request rather than the available key
    /// it matched - `Intl.NumberFormat`'s [[Locale]] - writes in front of the keywords it accepts.
    /// Equal to [`Self::locale`] when the request already was that available locale, and to the
    /// default locale when nothing matched.
    pub requested_base: String,

    pub extension: String,
}

/// CanonicalizeUnicodeLocaleId (https://tc39.es/ecma402/#sec-canonicalizeunicodelocaleid), 6.2.2,
/// for callers without a context.
pub fn canonicalize_locale_tag(tag: &str) -> Result<String, LocaleError> {
    canonicalize_locale_tag_with(&CANONICALIZER, tag)
}

/// CanonicalizeUnicodeLocaleId (6.2.2), after the check of IsWellFormedLanguageTag (6.2.1) that
/// CanonicalizeLocaleList step 7.3.5 performs.
pub fn canonicalize_locale_tag_with(
    canonicalizer: &LocaleCanonicalizer,
    tag: &str,
) -> Result<String, LocaleError> {
    // Parsing checks the grammar of `unicode_locale_id` and rejects the backwards compatible
    // syntax, which is what 6.2.1 asks for: "" and "en_US" are errors here, and the empty String
    // is not special-cased away because no language tag is empty.
    let (mut locale, reserved) = parse_locale_tag(tag)?;

    // Steps 4-11.7, which the parser alone cannot make: it deduplicates a repeated variant instead
    // of refusing the tag that spelled it.
    if has_duplicate_variants(tag) {
        return Err(LocaleError::InvalidTag);
    }

    // Step 1 is the TR35 Annex C processing: canonical casing, then the CLDR alias table, which
    // also replaces a region or variant alias with its preferred value.
    canonicalizer.canonicalize(&mut locale);

    // Only the part of the tag in front of the Unicode extension sequence comes from that round
    // trip. Step 2 then rebuilds the sequence itself - each key once with its first value, sorted -
    // over the request's own spelling, because the tables Annex C's canonicalization of a *value*
    // reads are only the ones this build bakes, and they are the ones of the keys `Intl` names.
    // What step 2 does add, and what Annex C §3.2.1 requires of every sequence, is that a value of
    // "true" is written as the bare key: test262's
    // `Collator/missing-unicode-ext-value-defaults-to-true.js` and
    // `Intl/getCanonicalLocales/unicode-ext-canonicalize-yes-to-true.js` both check it.
    let (base, _) = take_unicode_extension(&locale.to_string());

    // The stand-in goes back out again. `parse_locale_tag` only got here by putting `und` at the
    // front, and no rule of the tables rewrites `und` itself, so the three letters are still what
    // comes at the front of what it made.
    let base = match &reserved {
        Some(language) => format!("{language}{}", &base[UND_STAND_IN.len()..]),
        None => base,
    };

    // Step 2, over the sequence of the request itself.
    let Some(extension) = canonical_unicode_extension(tag) else {
        return Ok(base);
    };

    Ok(insert_unicode_extension(&base, &extension))
}

/// The language `parse_locale_tag` stands in for a reserved one, and puts back afterwards.
const UND_STAND_IN: &str = "und";

/// Parse a tag for CanonicalizeUnicodeLocaleId step 1, with the reserved-length language of
/// `reserved_language` replaced by `und` if the tag has one: ICU4X refuses to parse it at all, while
/// 6.2.1 calls a tag that starts with it well-formed. `Some` of the second half of the pair is that
/// language, spelled as BCP 47 canonicalizes it, which the caller writes back over the stand-in once
/// the tables have had their say on everything else.
///
/// The stand-in is honest because of what the reserved form is: an assigned language subtag would be
/// read by the alias tables *as itself*, and none has ever been assigned, so the only part of the tag
/// that canonicalization can act on is the part the stand-in leaves in front of it.
fn parse_locale_tag(tag: &str) -> Result<(Locale, Option<String>), LocaleError> {
    if let Ok(locale) = tag.parse::<Locale>() {
        return Ok((locale, None));
    }

    let Some(language) = reserved_language(locale_base_name(tag)) else {
        return Err(LocaleError::InvalidTag);
    };

    let stand_in = match tag.strip_prefix(language) {
        Some(rest) => format!("{UND_STAND_IN}{rest}"),
        None => return Err(LocaleError::InvalidTag),
    };

    let locale = stand_in
        .parse::<Locale>()
        .map_err(|_| LocaleError::InvalidTag)?;

    Ok((locale, Some(language.to_ascii_lowercase())))
}

/// The second half of CanonicalizeUnicodeLocaleId (6.2.2) step 2: the Unicode locale extension
/// sequence of `tag` as UnicodeExtensionComponents (9.2.5) reads it, which gives the attributes in
/// order and each key once with its first value, sorted by key. `None` when the tag has no such
/// sequence, and the whole sequence is lowercased first because the casing step of TR35 Annex C
/// covers it, which the request may spell however it likes.
fn canonical_unicode_extension(tag: &str) -> Option<String> {
    let (_, sequence) = take_unicode_extension(tag);
    if sequence.is_empty() {
        return None;
    }

    let (attributes, keywords) = unicode_extension_components(&sequence.to_ascii_lowercase());

    // Steps 2.2-2.5: "-u", then the attributes, then every key with its value. A key that the
    // components left without one is written bare, which is the spelling of 9.2.5 for it.
    let mut extension = String::from("-u");
    for attribute in &attributes {
        extension.push('-');
        extension.push_str(attribute);
    }
    for (key, value) in &keywords {
        extension.push('-');
        extension.push_str(key);

        // Step 2.3 of CanonicalizeUnicodeLocaleId, which TR35's CanonicalizeUValue answers for the
        // keys Intl reads: the value goes in as that operation writes it. Any other key keeps what
        // the request spelled, lowercased, except for the one rule Annex C states for every value of
        // every key - see below.
        let value = if CANONICALIZED_KEYS.contains(&key.as_str()) {
            canonical_u_value(key, value)
        } else {
            // "Any type or tfield value 'true' is removed", which TR35 §3.2.1
            // (https://unicode.org/reports/tr35/#Unicode_Language_and_Locale_Identifiers) states for
            // the whole sequence and test262's `Intl/getCanonicalLocales/unicode-ext-canonicalize-yes-to-true.js`
            // checks for the keys `Intl` itself does not read. For the keys it does read,
            // CanonicalizeUValue above arrives at the same empty String by its own route, and
            // `Collator/missing-unicode-ext-value-defaults-to-true.js` is what asks for it.
            if value == "true" {
                String::new()
            } else {
                value.clone()
            }
        };

        if !value.is_empty() {
            extension.push('-');
            extension.push_str(&value);
        }
    }

    Some(extension)
}

/// The Unicode BCP 47 keys whose values `Intl` reads, so that CanonicalizeUValue
/// (https://unicode.org/reports/tr35/#CanonicalizeUValue) is applied to them and to nothing else:
/// the keys of Locale's own slots (15.2.2) and the keys every service's
/// [[RelevantExtensionKeys]] names (10.2.3, 13.2.3, 14.2.3 and 16.2.3) are all here.
const CANONICALIZED_KEYS: &[&str] = &["ca", "co", "fw", "hc", "kf", "kn", "nu"];

/// CanonicalizeUValue (https://tc39.es/ecma402/#sec-canonicalizeuvalue), 9.2.2: the canonical and
/// case-regularized form of `value` *as a value of `key`*, which is TR35 Annex C's alias processing
/// over CLDR's `common/bcp47` keyword tables. It is what MakeLocaleRecord step 4.5.1 and
/// CalendarsOfLocale step 9.1 call.
///
/// A value on its own is not a language identifier, so it is canonicalized inside one: `und` is
/// CLDR's own root language, and the alias table rewrites the keyword whatever the language in
/// front of it is. That is what makes the tables - which this build already bakes for
/// CanonicalizeUnicodeLocaleId - the only locale data this operation needs: the `ca` row that turns
/// CLDR's `gregorian` into the registry's `gregory`, and the `kf` and `kn` rows that turn BCP 47's
/// `true` into the *empty* value, which is what 15.1.1 step 42.1 reads a numeric extension as and
/// what test262's `Locale/getters.js` expects an engine to write back into the tag.
pub(crate) fn canonical_u_value(key: &str, value: &str) -> String {
    // Steps 1-2, for a key the spec's own tables name; anything else is only case-regularized.
    let lowered = value.to_ascii_lowercase();

    if !CANONICALIZED_KEYS.contains(&key) {
        return lowered;
    }

    // CLDR's alias table for the values of these keys is not baked data: it is the keyword type of
    // `icu_locale_core`, which parses a value and knows the canonical spelling of what it read. That
    // is the half of CanonicalizeUValue the route below cannot do, because the generated
    // `LocaleAliases` marker carries no keyword rows at all - only language, script, region and
    // variant ones.
    if let Some(canonical) = keyword_value(key, &lowered) {
        return canonical;
    }

    let Ok(mut locale) = format!("und-u-{key}-{lowered}").parse::<Locale>() else {
        return lowered;
    };

    CANONICALIZER.canonicalize(&mut locale);

    // A keyword the table left with no value is the empty String, which is what 9.2.2 returns for
    // it; a keyword the table dropped altogether is answered with the lowercase form, because
    // there is nothing more to say about a value the data does not know.
    unicode_extension_value(&locale.to_string(), key).unwrap_or(lowered)
}

/// CanonicalizeUValue (9.2.2) over the keyword values whose list CLDR defines and ICU4X compiles in.
/// `None` is a value the list does not name, which includes every value of a key this has no type for
/// - `kf` and `kn`, whose canonical form CLDR writes as the bare key, stay with the route above.
fn keyword_value(key: &str, value: &str) -> Option<String> {
    use icu_locale::extensions::unicode::Value;
    // The canonical spelling is a method of the trait, so it has to be in scope to be called.
    use icu_locale::preferences::PreferenceKey;
    use icu_locale::preferences::extensions::unicode::keywords::{
        CalendarAlgorithm, CollationType, FirstDay, HourCycle,
    };

    let value = Value::try_from_str(value).ok()?;

    let canonical = match key {
        "ca" => CalendarAlgorithm::try_from(&value)
            .ok()
            .and_then(|keyword| keyword.unicode_extension_value()),
        "co" => CollationType::try_from(&value)
            .ok()
            .and_then(|keyword| keyword.unicode_extension_value()),
        "fw" => FirstDay::try_from(&value)
            .ok()
            .and_then(|keyword| keyword.unicode_extension_value()),
        "hc" => HourCycle::try_from(&value)
            .ok()
            .and_then(|keyword| keyword.unicode_extension_value()),
        _ => None,
    }?;

    Some(canonical.to_string())
}

/// InsertUnicodeExtensionAndCanonicalize steps 5-6 and CanonicalizeUnicodeLocaleId step 2.7: the
/// sequence goes after the subtags of `base`, but in front of a private use sequence, which BCP 47
/// requires to stay last.
fn insert_unicode_extension(base: &str, extension: &str) -> String {
    match base.find("-x-") {
        // `base` is either canonical already or has been through ICU4X, so the only `-x-` in it
        // begins its private use sequence.
        Some(index) => {
            let mut tag = String::with_capacity(base.len() + extension.len());
            tag.push_str(&base[..index]);
            tag.push_str(extension);
            tag.push_str(&base[index..]);
            tag
        }
        None => {
            let mut tag = String::with_capacity(base.len() + extension.len());
            tag.push_str(base);
            tag.push_str(extension);
            tag
        }
    }
}

/// CanonicalizeLocaleList (https://tc39.es/ecma402/#sec-canonicalizelocalelist), 9.2.1, for the
/// elements that are already strings. Steps 1-7.3.4 of the operation belong to the JS side: reading
/// the argument, `ToString` of each element and the `[[InitializedLocale]]` branch of step 7.3.3.
pub fn canonicalize_locale_list(locales: &[String]) -> Result<Vec<String>, LocaleError> {
    canonicalize_locale_list_with(&CANONICALIZER, locales)
}

/// CanonicalizeLocaleList (9.2.1) steps 7.3.5-7.3.7 for a list of already stringified elements.
pub fn canonicalize_locale_list_with(
    canonicalizer: &LocaleCanonicalizer,
    locales: &[String],
) -> Result<Vec<String>, LocaleError> {
    // Step 2's `seen`. It keeps the order of the requests, so the first locale a caller named
    // stays the first one Intl negotiates.
    let mut seen = Vec::with_capacity(locales.len());

    for locale in locales {
        let loc = canonicalize_locale_tag_with(canonicalizer, locale)?;

        // Step 7.3.7: a tag that canonicalizes to one already listed is dropped, so that "en" and
        // "EN" are one request rather than two.
        if !seen.iter().any(|already| already == &loc) {
            seen.push(loc);
        }
    }

    Ok(seen)
}

/// LookupMatchingLocaleByPrefix (https://tc39.es/ecma402/#sec-lookupmatchinglocalebyprefix), 9.2.3,
/// over the whole list of requests. The first request that matches decides the match.
pub fn lookup_supported(
    available_locales: &[&str],
    requested_locales: &[String],
) -> Option<LocaleMatch> {
    // Step 1: for each requested locale.
    requested_locales
        .iter()
        .find_map(|locale| lookup_prefix(available_locales, locale))
}

/// LookupMatchingLocaleByPrefix (9.2.3) for a single requested tag. 15.5.10 step 2 and 15.5.12
/// step 2 call it with one-element lists of their own.
pub(crate) fn lookup_prefix(available_locales: &[&str], locale: &str) -> Option<LocaleMatch> {
    // Steps 1.1-1.2: the Unicode extension is carried through the match but takes no part in it.
    // `requested_base` is the tag minus that extension, the whole the service that reports the
    // request echoes, while `matched` below walks it back to an available locale.
    let (requested_base, extension) = take_unicode_extension(locale);

    // Steps 1.3-1.4: try the tag, then each of its parent tags.
    let mut matched = requested_base.clone();
    while !matched.is_empty() {
        if available_locales.contains(&matched.as_str()) {
            return Some(LocaleMatch { locale: matched, requested_base, extension });
        }

        // Step 1.4.b: cut the tag at its last hyphen, or drop it down to nothing.
        let mut position = matched.rfind('-').unwrap_or(0);

        // Step 1.4.c: a run of single character subtags is cut out whole, so "en-x-icu" falls to
        // "en" rather than to the malformed "en-x".
        while position >= 2 && matched.as_bytes()[position - 2] == b'-' {
            position -= 2;
        }

        // Step 1.4.d.
        matched.truncate(position);
    }

    // Step 2.
    None
}

/// ResolveLocale (https://tc39.es/ecma402/#sec-resolvelocale), 9.2.7, for a service that has no
/// relevant Unicode extension keys. It never fails: a request for an unsupported locale gets the
/// default one, which is what step 4 says rather than what a stricter engine might do.
pub fn resolve_locale(requested_locales: &[String], matcher: Matcher) -> ResolvedLocale {
    resolve_locale_with_keys(requested_locales, matcher, &[])
}

/// A relevant Unicode extension key of one service, as ResolveLocale step 12 (9.2.7) reads them.
#[derive(Debug, Clone)]
pub struct RelevantExtensionKey {
    /// The key, as it appears in a `-u-` extension and in `localeData`.
    pub key: &'static str,

    /// `localeData[[<foundLocale>]][[<key>]]`: the values the implementation supports for the
    /// locale, the first of which is that locale's default (steps 12.1-12.3). It is one list per
    /// key rather than one per locale because every locale this build bakes supports the same
    /// values for the keys it has.
    pub supported_values: &'static [&'static str],

    /// `options[[<key>]]` as ResolveOptions step 6 (9.2.8) read it from the constructor's option
    /// of the same name, already canonicalized; `None` when that option was absent, and when it
    /// was present but blanked, which is the next field.
    pub option_value: Option<String>,

    /// Whether the resolution record carries this key with a null value - which step 12.9 allows
    /// beside a String and an undefined, and which a service puts there when one option answers the
    /// same question as another: 11.1.2 step 3.3 blanks the `hc` of a `DateTimeFormat` whenever a
    /// `hour12` was given, whatever the `hourCycle` option or the tag said.
    ///
    /// It is a third state rather than `option_value: None`, because the two resolve differently. An
    /// absent option leaves whatever the request spelled on the tag (step 12.11 does not fire); a
    /// null takes it off, because the only key with a null among its locale data values is "hc",
    /// whose list is « null, "h11", "h12", "h23", "h24" » (11.2.3) - so the null is a supported value
    /// that differs from the keyword's, which is exactly step 12.11. The service then answers the
    /// hour cycle from its own locale data, as 11.1.2 steps 14 to 16 do.
    pub option_is_null: bool,
}

/// ResolveLocale (9.2.7) for a service with relevant extension keys. Like `resolve_locale` it never
/// fails: a key whose requested value is not supported falls back to that locale's default, which
/// is what steps 12.6 and 12.11 do.
pub fn resolve_locale_with_keys(
    requested_locales: &[String],
    matcher: Matcher,
    relevant_keys: &[RelevantExtensionKey],
) -> ResolvedLocale {
    let available_locales = BuiltInLocales::new().tags();

    // Steps 1-3, then step 4 for the fallback.
    let found_match =
        lookup_locale(available_locales, requested_locales, matcher).unwrap_or(LocaleMatch {
            locale: default_locale().to_owned(),
            requested_base: default_locale().to_owned(),
            extension: String::new(),
        });

    // Step 5. The data is looked up by the available locale that matched; the tag a NumberFormat
    // reports is the request itself (step 5's found locale with the region and script the request
    // named put back), which is what makes "en-US" report "en-US" while formatting on "en" data.
    let found_locale = found_match.locale;
    let requested_base = found_match.requested_base;

    // Steps 9-11: the keywords of the requested tag's extension, which the lookup carried through
    // untouched. An empty extension has no keywords, so step 10's empty list is the same thing.
    let keywords = if found_match.extension.is_empty() {
        Vec::new()
    } else {
        unicode_extension_components(&found_match.extension).1
    };

    // Step 11's `supportedKeywords` and the `result[[key]]` of step 12.13, which are different
    // lists: a value that came from the option rather than from the extension is resolved but not
    // written onto the tag (step 12.11.2).
    let mut extensions = Vec::new();
    let mut values = Vec::new();

    // Step 12.
    for relevant_key in relevant_keys {
        // Steps 12.1-12.4: the default is the first value the locale data lists, which the
        // constraints of 9.1 and 16.2.3 guarantee is there.
        let default_value = relevant_key
            .supported_values
            .first()
            .copied()
            .unwrap_or_default();
        let mut value: &str = default_value;

        // Step 12.5.
        let mut supported_keyword: Option<(String, String)> = None;

        // Steps 12.6.1-12.6.2: the value the request asked for under this key, if it asked.
        let requested_value = keywords
            .iter()
            .find(|(key, _)| *key == relevant_key.key)
            .map(|(_, value)| value.as_str());

        if let Some(requested_value) = requested_value {
            // Step 12.6.3: a value the data does not list is dropped without a word, which is the
            // standing decision of this build rather than a shortcut: `en-u-nu-arab` formats in
            // Latin digits and reports "latn". Step 12.6.4, which turns a valueless keyword into
            // "true", is for the keys a Collator and a DateTimeFormat read; "nu" has no "true".
            if !requested_value.is_empty()
                && relevant_key.supported_values.contains(&requested_value)
            {
                value = requested_value;
                supported_keyword = Some((relevant_key.key.to_owned(), value.to_owned()));
            }
        }

        // Steps 12.8 to 12.11, the null first: the record carries the key with nothing to take from
        // it, and for "hc" alone that nothing is one of the values the data has, so it beats the
        // keyword the request spelled and takes it off the tag. The value of step 12.13 is left at
        // what the tag said, because no service of this build reads `values` for a key it can blank.
        if relevant_key.option_is_null {
            supported_keyword = None;
        }

        // Steps 12.8 to 12.11 the other way: the option beats the extension, and the keyword then
        // comes off the tag rather than being rewritten, which is why `{ numberingSystem: "latn" }`
        // resolves the locale to "en" and not to "en-u-nu-latn". The value arrives already
        // canonicalized.
        if let Some(option_value) = relevant_key.option_value.as_deref() {
            if option_value != value && relevant_key.supported_values.contains(&option_value) {
                value = option_value;
                supported_keyword = None;
            }
        }

        // Step 12.12.
        if let Some(keyword) = supported_keyword {
            extensions.push(keyword);
        }

        // Step 12.13.
        values.push((relevant_key.key.to_owned(), value.to_owned()));
    }

    // Steps 13-14: InsertUnicodeExtensionAndCanonicalize with no attributes. With the keys this
    // build has, that is the tag with `-u-` and each accepted key and value appended; the keys come
    // out of `relevant_keys`, which a service lists in the order 9.2.7 step 12 reads them, and
    // "nu" alone is already in the canonical order.
    let mut locale = requested_base;
    if !extensions.is_empty() {
        locale.push_str("-u");
        for (key, value) in &extensions {
            locale.push('-');
            locale.push_str(key);
            if !value.is_empty() {
                locale.push('-');
                locale.push_str(value);
            }
        }
    }

    ResolvedLocale { locale, data_locale: found_locale, extensions, values }
}

/// The record ResolveLocale (9.2.7) returns. `locale` is step 14's `[[Locale]]`, the tag the
/// service reports as its resolved locale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLocale {
    /// The tag a constructor's `locale` reports, with the accepted Unicode extension keys.
    pub locale: String,

    /// The locale whose data backs `locale`: step 5's found locale, before step 13.2 put the
    /// accepted keywords onto it. Data lookups must not see the extension.
    pub data_locale: String,

    /// The keyword and value pairs step 13's `supportedKeywords` writes onto `locale`, in the order
    /// the keys were read. Empty for a service with no relevant keys, and for a value that arrived
    /// from the option rather than from the requested extension.
    pub extensions: Vec<(String, String)>,

    /// `result[[key]]` of step 12.13: the resolved value of every relevant extension key, whether
    /// or not it is spelled in `locale`. The pairs are in the order the keys were read.
    pub values: Vec<(String, String)>,
}

/// FilterLocales (https://tc39.es/ecma402/#sec-filterlocales), 9.2.9 steps 3-4, over a list that
/// CanonicalizeLocaleList has already produced. Step 4.3 appends the requested tag, not the
/// available one it matched, so "en-GB" is listed as "en-GB" even though it resolves to "en".
pub fn supported_locales_of(requested_locales: &[String], matcher: Matcher) -> Vec<String> {
    let available_locales = BuiltInLocales::new().tags();

    requested_locales
        .iter()
        .filter(|locale| {
            lookup_locale(available_locales, std::slice::from_ref(locale), matcher).is_some()
        })
        .cloned()
        .collect()
}

/// Steps 1-3 of ResolveLocale and 4.1-4.2 of FilterLocales, which both choose between the two
/// matchers the same way.
///
/// "best fit" means LookupMatchingLocaleByBestFit (9.2.4), whose result is
/// implementation-defined and which must be at least as good as the prefix lookup. With one locale
/// per language built in, nothing can be better than that lookup, so both spellings take it.
fn lookup_locale(
    available_locales: &[&str],
    requested_locales: &[String],
    matcher: Matcher,
) -> Option<LocaleMatch> {
    match matcher {
        Matcher::Lookup | Matcher::BestFit => {
            lookup_supported(available_locales, requested_locales)
        }
    }
}

/// One relevant Unicode extension key of a service, as ResolveLocale (9.2.7) needs it.
#[derive(Debug, Clone, Copy)]
pub struct ExtensionKey<'a> {
    /// The two character key itself, "co", "kf" or "kn" for a collator.
    pub key: &'a str,

    /// `[[LocaleData]].[[<locale>]].[[<key>]]` (9.1): the values this build accepts for the key in
    /// the locale being resolved, its default first, and the spec's `null` written as `None`. A
    /// service whose accepted values are the same in every built in locale - which is every service
    /// of this build - can pass one table, and says so where it builds it.
    pub values: &'a [Option<&'a str>],

    /// `options.[[<key>]]`, the record ResolveOptions (9.2.8 step 6.6) fills from the options
    /// object. `None` covers both undefined and null, which step 12.k skips.
    pub requested: Option<&'a str>,
}

/// The locale ResolveLocale (9.2.7) settles on for a service that has relevant extension keys,
/// together with the resolved value of each of those keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceLocale {
    /// `result.[[Locale]]`, step 14: the tag the service reports, carrying the accepted keywords.
    pub locale: String,

    /// Step 5's found locale before step 13.2 put the accepted keywords onto it: the locale whose
    /// data backs `locale`, and the only one a data lookup may see. It is one of the built in
    /// locales, held by its static tag so that a service can key its own data cache on it.
    pub data_locale: &'static str,

    /// `result.[[<key>]]` of step 12.n for each key, in the order of the `keys` argument. `None` is
    /// the spec's `null`: what a service with no built in specialization resolves "co" to.
    pub values: Vec<Option<String>>,
}

/// ResolveLocale (https://tc39.es/ecma402/#sec-resolvelocale), 9.2.7, for a service that does have
/// relevant Unicode extension keys, which the two argument `resolve_locale` above is the empty case
/// of. `keys` is `relevantExtensionKeys` (step 12) with the locale data (step 8.1) and the option
/// values (step 12.8) of each key beside it.
///
/// It takes the canonicalizer of CanonicalizeLocaleList step 7.3.6, which `cache::LocaleCache`
/// holds per context, because step 13.2 rebuilds the tag through
/// InsertUnicodeExtensionAndCanonicalize (9.2.6).
pub fn resolve_locale_with_extension_keys(
    canonicalizer: &LocaleCanonicalizer,
    requested_locales: &[String],
    matcher: Matcher,
    keys: &[ExtensionKey<'_>],
) -> ServiceLocale {
    let available_locales = BuiltInLocales::new().tags();

    // Steps 1-3, then step 4 for the fallback: an unsupported request still resolves, to the
    // default locale, and carries no extension of its own. The collator reports the available key it
    // matched (`found_locale`), not the request, so its `requested_base` is ignored.
    let LocaleMatch { locale: found_locale, requested_base: _, extension } =
        lookup_locale(available_locales, requested_locales, matcher).unwrap_or(LocaleMatch {
            locale: default_locale().to_owned(),
            requested_base: default_locale().to_owned(),
            extension: String::new(),
        });

    // Step 5, kept apart from step 14's `[[Locale]]` because the data of a service is looked up by
    // the available locale alone.
    let data_locale = available_locales
        .iter()
        .find(|tag| **tag == found_locale)
        .copied()
        // Every locale of a `LocaleMatch` is an element of `available_locales`, and the fallback is
        // `default_locale`, which `brimstone_intl_data` keeps one of them.
        .expect("the found locale is one of the built in locales");

    // Steps 9-10: the keywords of the extension sequence of the request that matched, if it had
    // one. 9.2.5 keeps the first record of each key and sorts them, so a key can be found by name.
    let keywords = if extension.is_empty() {
        Vec::new()
    } else {
        unicode_extension_components(&extension).1
    };

    // Step 11: supportedKeywords, the keywords that survive step 12 and are written back onto the
    // tag by step 13. Each is a key and the value to write, `None` for the bare spelling of step
    // 12.g.4 ("-u-kn" on its own).
    let mut supported: Vec<(String, Option<String>)> = Vec::with_capacity(keys.len());
    let mut values: Vec<Option<String>> = Vec::with_capacity(keys.len());

    for key in keys {
        // Steps 12.1-12.3: the locale's own default for the key.
        let mut value: Option<String> = key.values[0].map(str::to_owned);

        // Step 12.5: supportedKeyword is empty until a request asks for the key and the data
        // accepts what was asked.
        let mut keyword: Option<(String, Option<String>)> = None;

        if let Some((_, requested)) = keywords.iter().find(|(name, _)| name.as_str() == key.key) {
            if !requested.is_empty() {
                // Steps 12.6.3.1: the requested value only counts when the locale data has it.
                if key.values.contains(&Some(requested.as_str())) {
                    value = Some(requested.clone());
                    keyword = Some((key.key.to_owned(), Some(requested.clone())));
                }
            } else if key.values.contains(&Some("true")) {
                // Steps 12.6.4: a key with no value means "true" where "true" is one of the values
                // the data has, and is written back onto the tag without one.
                value = Some("true".to_owned());
                keyword = Some((key.key.to_owned(), None));
            }
        }

        // Steps 12.7-12.10: what the options object asked for beats what the tag asked for.
        if let Some(requested) = key.requested {
            // Steps 12.10.1-12.10.2: CanonicalizeUValue (9.2.2) of the ASCII-lowercase of the key.
            // The canonical form of every value this build accepts is its ASCII-lowercase spelling,
            // because the tables of `icu/intl_markers.txt` hold no CLDR keyword value aliases and
            // the accepted values are taken from CLDR's own canonical types. A value that would
            // canonicalize to something else is therefore not accepted, which is step 12.11's other
            // branch.
            let lowered = requested.to_ascii_lowercase();
            let mut options_value = lowered.as_str();

            // Step 12.10.3: an empty value is the bare spelling, which means "true".
            if options_value.is_empty() {
                options_value = "true";
            }

            // Step 12.11: only a value the data accepts changes anything, and when the options
            // override the tag the keyword is dropped from the tag.
            let is_available = key.values.contains(&Some(options_value));
            if is_available && Some(options_value) != value.as_deref() {
                value = Some(options_value.to_owned());
                keyword = None;
            }
        }

        // Step 12.12.
        if let Some(keyword) = keyword {
            supported.push(keyword);
        }

        // Step 12.13.
        values.push(value);
    }

    // Steps 13-14: the tag the service reports is the found locale with the supported keywords put
    // back onto it, which is what makes `resolvedOptions().locale` say "en-u-kn-true" for a request
    // that asked for numeric ordering in its tag.
    let locale = if supported.is_empty() {
        found_locale
    } else {
        insert_unicode_extension_and_canonicalize(canonicalizer, &found_locale, &[], &supported)
            // The `!` of step 13.2: `found_locale` is an element of the Available Locales List and
            // every keyword came out of that locale's own data, so the tag is canonical already and
            // a failure here would mean a table of this build is not.
            .expect("the resolved locale is a well-formed language tag")
    };

    ServiceLocale { locale, data_locale, values }
}

/// InsertUnicodeExtensionAndCanonicalize
/// (https://tc39.es/ecma402/#sec-insert-unicode-extension-and-canonicalize), 9.2.6, for the
/// keywords ResolveLocale step 12.12 collected. `attributes` is the list step 4 writes in front of
/// them, which is empty for 9.2.7 - the note of step 13.1 - and is the request's own for MakeLocale
/// Record (15.1.3 step 6.1).
///
/// The error is step 6's canonicalization refusing the tag the two halves make. ResolveLocale can
/// only be handed a canonical tag and asserts that away; MakeLocale Record cannot, because 15.1.2 put
/// subtags a program wrote in front of the extension.
fn insert_unicode_extension_and_canonicalize(
    canonicalizer: &LocaleCanonicalizer,
    locale: &str,
    attributes: &[String],
    keywords: &[(String, Option<String>)],
) -> Result<String, LocaleError> {
    // Steps 2-4: "-u", then the attributes, and then "-{key}" or "-{key}-{value}" for each keyword.
    let mut extension = String::from("-u");

    for attribute in attributes {
        extension.push('-');
        extension.push_str(attribute);
    }

    for (key, value) in keywords {
        extension.push('-');
        extension.push_str(key);

        if let Some(value) = value {
            extension.push('-');
            extension.push_str(value);
        }
    }

    // Step 5, which is not a plain concatenation: BCP 47 keeps a private use sequence last, so the
    // extension goes in front of the `-x-` of `locale` rather than after it. test262's
    // `Locale/constructor-tag.js` checks it with "en-u-baz-a-bar-x-u-foo".
    let tag = insert_unicode_extension(locale, &extension);

    // Step 6: CanonicalizeUnicodeLocaleId, which is the only thing of the two halves that can fail.
    canonicalize_locale_tag_with(canonicalizer, &tag)
}

/// The offsets of the Unicode locale extension sequence of `tag`, which ECMA-402 (section 6.2)
/// defines as the longest substring matched by `unicode_locale_extensions` that is not part of a
/// `-x-` private use sequence. Returns `None` when the tag has no such sequence.
fn unicode_extension_range(tag: &str) -> Option<(usize, usize)> {
    let mut start = None;
    let mut end = None;
    let mut position = 0;

    for subtag in tag.split('-') {
        if subtag.len() == 1 {
            // The sequence begins at the hyphen in front of the singleton `u`, which a request may
            // spell in either case.
            if subtag.eq_ignore_ascii_case("u") && start.is_none() && position > 0 {
                start = Some(position - 1);
            } else if start.is_none() {
                // A singleton that is not the `u` of a Unicode extension is some other extension
                // sequence, and BCP 47's canonical order does not promise the `-u-` one comes first:
                // "en-a-bar-u-baz" has both. Only a private use sequence ends the search, because
                // everything after its `-x-` is not an extension at all (section 6.2.1 step 8's
                // note).
                if subtag.eq_ignore_ascii_case("x") {
                    break;
                }
            } else {
                // Any singleton after the sequence - another extension or the `-x-` of a private use
                // one - is where it ends.
                break;
            }
        } else if start.is_some() {
            end = Some(position + subtag.len());
        }

        position += subtag.len() + 1;
    }

    match (start, end) {
        (Some(start), Some(end)) => Some((start, end)),
        _ => None,
    }
}

/// Split `tag` into itself without its Unicode locale extension sequence and that sequence, which
/// is empty when there is none. Steps 1.1-1.2 of LookupMatchingLocaleByPrefix (9.2.3).
fn take_unicode_extension(tag: &str) -> (String, String) {
    let Some((start, end)) = unicode_extension_range(tag) else {
        return (tag.to_owned(), String::new());
    };

    let mut base = String::with_capacity(tag.len() - (end - start));
    base.push_str(&tag[..start]);
    base.push_str(&tag[end..]);

    (base, tag[start..end].to_owned())
}

/// UnicodeExtensionComponents (https://tc39.es/ecma402/#sec-unicodeextensioncomponents), 9.2.5,
/// for a sequence that starts with "-u-". Attributes come before the first key and keywords are
/// two-character subtags followed by their value.
fn unicode_extension_components(extension: &str) -> (Vec<String>, Vec<(String, String)>) {
    // Steps 3-5.
    let mut attributes: Vec<String> = Vec::new();
    let mut keywords: Vec<(String, String)> = Vec::new();

    // The index in `keywords` of the keyword record the subtags are being read into, which step 5
    // leaves undefined until the first key.
    let mut keyword: Option<usize> = None;

    // Step 7 starts at the subtags after "-u-".
    for subtag in extension[3..].split('-') {
        if subtag.len() == 2 {
            // Step 8.7: a new key, whose value the following subtags fill in.
            keywords.push((subtag.to_owned(), String::new()));
            keyword = Some(keywords.len() - 1);
        } else if let Some(index) = keyword {
            // Steps 8.8-8.9: the value of a key is its following subtags, joined by hyphens.
            let value = &mut keywords[index].1;
            if value.is_empty() {
                *value = subtag.to_owned();
            } else {
                value.push('-');
                value.push_str(subtag);
            }
        } else if !attributes.iter().any(|attribute| attribute == subtag) {
            // Step 8.6: a subtag before the first key is an attribute, and only once.
            attributes.push(subtag.to_owned());
        }
    }

    // Step 8.7.2 keeps only the first record for a key, so a repeated "nu-latn ... nu-digit"
    // resolves to "nu-latn".
    let mut unique: Vec<(String, String)> = Vec::with_capacity(keywords.len());
    for keyword in keywords {
        if !unique.iter().any(|(key, _)| *key == keyword.0) {
            unique.push(keyword);
        }
    }

    // TR35 canonical order sorts the attributes and the keys of the sequence, which is what lets
    // two requests that differ only in order be the same locale to Intl.
    attributes.sort();
    unique.sort_by(|first, second| first.0.cmp(&second.0));

    (attributes, unique)
}

// The language-tag algebra of clause 15. `Intl.Locale` reads a tag by its subtags rather than by
// looking anything up in the locale data, so these are the pieces of the `unicode_locale_id`
// grammar (6.2) that its abstract operations are written in terms of. They work on plain strings so
// that which subtag a piece of a tag is can be checked without a JS context, like the rest of this
// module.

/// The CLDR likely-subtags tables, for Add Likely Subtags and Remove Likely Subtags (15.3.9,
/// 15.3.10 and RegionPreference step 2.1 below). The extended table, which is the one that answers
/// for a bare script and for the languages below CLDR's coverage; the `und` row of the plain one is
/// read by [`UND_ROW`] because `LocaleExpander` never consults it on its own.
static EXPANDER: LazyLock<LocaleExpander> = LazyLock::new(|| {
    LocaleExpander::try_new_extended_unstable(&IntlDataProvider)
        // Baked data, so a failure here is a generator that left the marker out rather than
        // something a program can cause. See `brimstone_intl_data`'s own data tests.
        .unwrap()
});

/// `unicode_language_subtag`: 2-3 or 5-8 alphabetical characters. The 4-character form the
/// production reserves for future use is refused, and so is "root", which TR35 keeps out of the
/// Unicode BCP 47 locale identifiers
/// (https://unicode.org/reports/tr35/#unicode_bcp47_locale_id), which is what `Intl.Locale`'s
/// `language` option refuses both of them for.
pub(crate) fn language_subtag(subtag: &str) -> bool {
    let bytes = subtag.as_bytes();

    (matches!(bytes.len(), 2 | 3) || (5..=8).contains(&bytes.len()))
        && bytes.iter().all(u8::is_ascii_alphabetic)
        && !subtag.eq_ignore_ascii_case("root")
}

/// `unicode_script_subtag`: 4 alphabetical characters.
pub(crate) fn script_subtag(subtag: &str) -> bool {
    let bytes = subtag.as_bytes();

    bytes.len() == 4 && bytes.iter().all(u8::is_ascii_alphabetic)
}

/// `unicode_region_subtag`: 2 alphabetical characters, as ISO 3166-1 writes them, or 3 digits, as
/// UN M.49 writes them.
pub(crate) fn region_subtag(subtag: &str) -> bool {
    let bytes = subtag.as_bytes();

    (bytes.len() == 2 && bytes.iter().all(u8::is_ascii_alphabetic))
        || (bytes.len() == 3 && bytes.iter().all(u8::is_ascii_digit))
}

/// `unicode_variant_subtag`: 5-8 alphanumeric characters, or 4 of which the first is a digit.
pub(crate) fn variant_subtag(subtag: &str) -> bool {
    let bytes = subtag.as_bytes();
    let alphanumeric = bytes.iter().all(u8::is_ascii_alphanumeric);

    match bytes.len() {
        5..=8 => alphanumeric,
        // The 4-character form is only ever a variant when it begins with a digit, which keeps the
        // productions of the grammar disjoint, as the note of 15.5.3 step 2 relies on.
        4 => bytes[0].is_ascii_digit() && alphanumeric,
        _ => false,
    }
}

/// The `type` production
/// (https://unicode.org/reports/tr35/#UnicodeTimeIdentifier's `type` sequence): one or more pieces
/// of 3-8 alphanumeric characters, separated by the `-` the BCP 47 grammar writes or the `_` the
/// CLDR one allows. 15.1.1 steps 17, 20, 23 and 33 match the `ca`, `co`, `fw` and `nu` option
/// values against it, and it is the same check ResolveOptions (9.2.8) step 6.4.2 performs on a
/// Collator's or a DateTimeFormat's `co`-style option.
pub fn type_value(value: &str) -> bool {
    !value.is_empty()
        && value.split(['-', '_']).all(|piece| {
            let bytes = piece.as_bytes();

            (3..=8).contains(&bytes.len()) && bytes.iter().all(u8::is_ascii_alphanumeric)
        })
}

/// The `subdivision` production
/// (https://unicode.org/reports/tr35/#UnicodeSubdivisionIdentifier): `unicode_subdivision_id` is a
/// `unicode_region_subtag` followed by a `unicode_subdivision_suffix` of 1-4 alphanumeric
/// characters, and the two are *fused* - the grammar writes no separator between them, which is why
/// the real values read `usca`, `no50`, `frges` and `cz110` and not `us-ca`. 15.5.7 step 3 matches
/// the value of the `sd` and `rg` keys against it, and answers undefined rather than throwing for a
/// value that does not match.
pub(crate) fn subdivision_value(value: &str) -> bool {
    let bytes = value.as_bytes();

    if bytes.is_empty() {
        return false;
    }

    // The production is the two `unicode_region_subtag` branches, each immediately followed by the
    // 1-4 character suffix. Trying both is what makes the choice of the grammar unambiguous: the
    // digits branch cannot start an alphabetic one, so at most one of them ever matches.
    let alphabetic_region = bytes.len() >= 3 && bytes[..2].iter().all(u8::is_ascii_alphabetic);
    let numeric_region = bytes.len() >= 4 && bytes[..3].iter().all(u8::is_ascii_digit);

    [2, 3]
        .into_iter()
        .filter(|&region_len| {
            (region_len == 2 && alphabetic_region) || (region_len == 3 && numeric_region)
        })
        .any(|region_len| {
            let suffix = &bytes[region_len..];

            (1..=4).contains(&suffix.len()) && suffix.iter().all(u8::is_ascii_alphanumeric)
        })
}

/// GetLocaleBaseName (https://tc39.es/ecma402/#sec-getlocalebasename), 15.5.1: the longest prefix
/// of `tag` matched by `unicode_language_id`, which is everything up to but not including the first
/// subtag that is not a language, script, region or variant. The productions are disjoint, so the
/// prefix is found by classifying each subtag in turn; a singleton, which begins a `-u-`, `-t-` or
/// `-x-` sequence, matches none of them and so ends the base name.
pub(crate) fn locale_base_name(tag: &str) -> &str {
    let mut end = 0;

    for (index, subtag) in tag.split('-').enumerate() {
        let matched = if index == 0 {
            language_subtag(subtag)
        } else {
            script_subtag(subtag) || region_subtag(subtag) || variant_subtag(subtag)
        };

        if !matched {
            break;
        }

        // The hyphen in front of the subtag, which the first one does not have.
        end += subtag.len() + usize::from(index > 0);
    }

    &tag[..end]
}

/// GetLocaleLanguage (https://tc39.es/ecma402/#sec-getlocalelanguage), 15.5.2.
pub(crate) fn locale_language(tag: &str) -> &str {
    // Steps 1-4: the first subtag of the base name, which 15.5.2 step 3 asserts is a language.
    locale_base_name(tag).split('-').next().unwrap_or_default()
}

/// GetLocaleScript (https://tc39.es/ecma402/#sec-getlocalescript), 15.5.3.
pub(crate) fn locale_script(tag: &str) -> Option<&str> {
    // Steps 1-5: the second subtag, and only when the script production matches it.
    locale_base_name(tag)
        .split('-')
        .nth(1)
        .filter(|subtag| script_subtag(subtag))
}

/// GetLocaleRegion (https://tc39.es/ecma402/#sec-getlocaleregion), 15.5.4.
pub(crate) fn locale_region(tag: &str) -> Option<&str> {
    let mut subtags = locale_base_name(tag).split('-');

    // Step 4: the language, which is never the region.
    subtags.next()?;

    // Steps 5 and 6: the region sits straight after the language, or after the script between them.
    let first = subtags.next().filter(|subtag| region_subtag(subtag));
    let second = subtags.next().filter(|subtag| region_subtag(subtag));

    first.or(second)
}

/// GetLocaleVariants (https://tc39.es/ecma402/#sec-getlocalevariants), 15.5.5: the base name from
/// its first variant to the end, which is the whole run of them because no other production can
/// come between two variants.
pub(crate) fn locale_variants(tag: &str) -> Option<&str> {
    let base = locale_base_name(tag);

    // The offset of the subtag being looked at, which step 6 takes past the language before the
    // loop starts.
    let mut offset = 0;

    for (index, subtag) in base.split('-').enumerate() {
        // Steps 7.1-7.1.1.
        if index > 0 && variant_subtag(subtag) {
            return Some(&base[offset..]);
        }

        // Step 7.3, including the hyphen that follows the subtag.
        offset += subtag.len() + 1;
    }

    // Step 8.
    None
}

/// UnicodeExtensionValue (https://tc39.es/ecma402/#sec-unicodeextensionvalue), 15.5.6. The `Option`
/// is the spec's "a String or empty": `None` is the empty of step 2, and an empty `Some` is a key
/// the tag wrote without a value, which 9.2.5 reports as the empty String.
///
/// `tag` is a canonicalized identifier, which is what 15.5.6 asks for and what [[Locale]] always
/// is, so the sequence needs no further casing or sorting here:
/// `canonical_unicode_extension` sorted it when the tag was canonicalized.
pub(crate) fn unicode_extension_value(tag: &str, key: &str) -> Option<String> {
    // Steps 1.1-1.3.
    let (_, sequence) = take_unicode_extension(tag);
    if sequence.is_empty() {
        return None;
    }

    let (_, keywords) = unicode_extension_components(&sequence);

    // Step 1.4.
    keywords
        .into_iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

/// CanonicalUnicodeSubdivision (https://tc39.es/ecma402/#sec-canonicalunicodesubdivision), 15.5.7,
/// for the `sd` and `rg` keys. Unlike the option checks of 15.1.1 and 15.1.2, a value that does not
/// match the production is answered here with `None` rather than with a RangeError: 15.5.7 is
/// step 3's "return undefined".
pub(crate) fn canonical_unicode_subdivision(tag: &str, key: &str) -> Option<String> {
    // Steps 1-2.
    let subdivision = unicode_extension_value(tag, key)?;
    if subdivision.is_empty() {
        return None;
    }

    // Step 3.
    if !subdivision_value(&subdivision) {
        return None;
    }

    // Step 4: the longest prefix of the subdivision that `unicode_region_subtag` matches, which the
    // fused grammar makes the first two letters or the first three digits.
    let region = subdivision_region(&subdivision)?;

    // Steps 5-7: the region on its own, under the root language, canonicalized so that an alias such
    // as "UK" becomes the region CLDR prefers, and read back out of it.
    let region_locale = canonicalize_locale_tag(&format!("und-{region}")).ok()?;

    locale_region(&region_locale).map(str::to_owned)
}

/// CanonicalUnicodeSubdivision (15.5.7) step 4: the `unicode_region_subtag` that a matched
/// `unicode_subdivision_id` begins with. The suffix of the id runs on from the region without a
/// separator, so the prefix is taken by length alone and the two branches of the production are
/// told apart by what the characters are.
fn subdivision_region(subdivision: &str) -> Option<&str> {
    let bytes = subdivision.as_bytes();

    if bytes.len() >= 4 && bytes[..3].iter().all(u8::is_ascii_digit) {
        return Some(&subdivision[..3]);
    }

    if bytes.len() >= 3 && bytes[..2].iter().all(u8::is_ascii_alphabetic) {
        return Some(&subdivision[..2]);
    }

    None
}

/// RegionPreference (https://tc39.es/ecma402/#sec-regionpreference), 15.5.8: the region whose data a
/// Locale Info getter reads, with the `rg` override that takes precedence over it.
pub(crate) struct RegionPreference {
    pub region: String,
    pub region_override: Option<String>,
}

pub(crate) fn region_preference(tag: &str) -> RegionPreference {
    // Steps 1-2.
    let region = match locale_region(tag) {
        Some(region) => region.to_owned(),
        None => match canonical_unicode_subdivision(tag, "sd") {
            Some(subdivision) => subdivision,

            // Steps 2.1-2.4: whatever region the tag's likely subtags imply, and failing that the
            // whole world.
            None => {
                let maximal = add_likely_subtags(tag).unwrap_or_else(|| tag.to_owned());
                let maximal = canonicalize_locale_tag(&maximal).unwrap_or(maximal);

                locale_region(&maximal).unwrap_or("001").to_owned()
            }
        },
    };

    RegionPreference {
        region,
        // Step 3.
        region_override: canonical_unicode_subdivision(tag, "rg"),
    }
}

/// The day of week Table 26 (15.5.15) gives each of its spellings, in table order.
const WEEKDAYS: [(&str, &str, u8); 8] = [
    ("0", "sun", 7),
    ("1", "mon", 1),
    ("2", "tue", 2),
    ("3", "wed", 3),
    ("4", "thu", 4),
    ("5", "fri", 5),
    ("6", "sat", 6),
    ("7", "sun", 7),
];

/// WeekdayToUValue (https://tc39.es/ecma402/#sec-weekdaytouvalue), 15.5.15: the numeric spelling of
/// a day becomes the CLDR spelling of it, and anything else is returned as it came. The result is
/// then matched against the `type` production by 15.1.1 step 23.2.
pub(crate) fn weekday_to_u_value(fw: &str) -> String {
    // Step 1.
    for (weekday, string, _) in WEEKDAYS {
        if fw == weekday {
            // Step 1.3.
            return string.to_owned();
        }
    }

    // Step 2.
    fw.to_owned()
}

/// WeekdayUValueToNumber (https://tc39.es/ecma402/#sec-weekdayuvaluetonumber), 15.5.16: the ISO 8601
/// ordinal of a CLDR day name, Monday 1 through Sunday 7.
pub(crate) fn weekday_u_value_to_number(fw: &str) -> Option<u8> {
    // Steps 1-1.3. Table order does not matter here: the names the table gives are unique, so at
    // most one row can answer.
    WEEKDAYS
        .iter()
        .find(|(_, string, _)| *string == fw)
        .map(|(_, _, value)| *value)
}

/// Add Likely Subtags (https://unicode.org/reports/tr35/#Likely_Subtags), which 15.3.9 step 3 and
/// 15.5.8 step 2.1 apply to a whole tag. `None` is the error the spec's "if an error is signaled"
/// branch covers, which is what happens to a tag whose subtags are not in the tables at all.
///
/// The extension sequences take no part in the lookup and are written back onto the result, which is
/// what makes `"en-u-nu-arab"` maximize to `"en-Latn-US-u-nu-arab"` rather than lose its numbering
/// system.
pub(crate) fn add_likely_subtags(tag: &str) -> Option<String> {
    likely_subtags(tag, true)
}

/// Remove Likely Subtags (https://tc39.es/ecma402/#LikelySubtags), 15.3.10 step 3.
pub(crate) fn remove_likely_subtags(tag: &str) -> Option<String> {
    likely_subtags(tag, false)
}

/// Both directions of the likely-subtags algorithm, which are the same walk: take the extensions off,
/// run the table over what is left, and put them back.
fn likely_subtags(tag: &str, add: bool) -> Option<String> {
    let (base, extensions) = split_at_first_extension(tag);

    if reserved_language(base).is_some() {
        // A language ICU4X's tables cannot hold has never been assigned, so no row of them reads the
        // tag, and TR35 answers an unmatched lookup with the locale it was given.
        return Some(tag.to_owned());
    }

    let mut identifier = base.parse::<LanguageIdentifier>().ok()?;

    // Removal is defined over the maximized form (TR35's Remove Likely Subtags step 1), so that
    // happens here first and is not left to the one `minimize` runs inside itself: that one is the
    // same walk but without the `und` row below, and would maximize `und` to nothing.
    maximize_base_name(&mut identifier);

    if !add {
        EXPANDER.minimize(&mut identifier);
    }

    let mut result = identifier.to_string();
    result.push_str(extensions);

    Some(result)
}

/// Add Likely Subtags (TR35 C.1) over a base name, with the `und` row filling whatever the tables
/// left out.
fn maximize_base_name(identifier: &mut LanguageIdentifier) {
    EXPANDER.maximize(identifier);

    if identifier.language.is_unknown() {
        let (language, script, region) = *UND_ROW;

        identifier.language = language;
        identifier.script = identifier.script.or(Some(script));
        identifier.region = identifier.region.or(Some(region));
    }
}

/// The `und` row of the same baked likely-subtags table [`EXPANDER`] reads: the language, script and
/// region CLDR writes for the fields a tag leaves out. `LocaleExpander` consults that row only where
/// the region or the script of a tag is itself a key of its tables, and so leaves `und`, `und-150`
/// and `und-AQ` maximized as they came; TR35's lookup ends with the empty key, which is that row, and
/// answers an unmatched field with it. V8 follows TR35, which is what test262's
/// `Locale/likely-subtags.js` checks.
static UND_ROW: LazyLock<(Language, Script, Region)> = LazyLock::new(|| {
    DataProvider::<LocaleLikelySubtagsLanguageV1>::load(&IntlDataProvider, DataRequest::default())
        // Baked data, so a failure here is a generator that left the marker out rather than
        // something a program can cause. See `brimstone_intl_data`'s own data tests.
        .unwrap()
        .payload
        .get()
        .und
});

/// The five to eight letter language subtag at the front of a base name, which is the alternative
/// `unicode_language_subtag` reserves besides the two and three letter one, spelled out in full.
/// `Language` of `icu_locale_core` is a `TinyStr` of two or three letters and calls the reserved form
/// a syntax error, so this is the one well-formed production ICU4X cannot read. None of it has ever
/// been assigned, which is what lets `und` stand in for it wherever the tables have to be consulted:
/// see `parse_locale_tag`.
fn reserved_language(base: &str) -> Option<&str> {
    let (language, _) = base.split_once('-').unwrap_or((base, ""));

    if (5..=8).contains(&language.len()) && language.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        Some(language)
    } else {
        None
    }
}

/// The split of a tag into the base name - language, script, region, extlangs and variants, which is
/// all the likely-subtags tables read - and the sequences that follow it. RFC 5646 (section 2.1)
/// begins an extension sequence with a one-character subtag, and no production of the base name is
/// one character long, so the first singleton is where the base name ends. That covers the `-u-`
/// extension, the `-t-` and `-b-` extensions, an attribute's `-a-` sequence and the `-x-` private use
/// sequence alike, and keeps them in the order the tag wrote them.
fn split_at_first_extension(tag: &str) -> (&str, &str) {
    // The offset of the subtag the walk is on, which is the hyphen in front of it for every subtag
    // but the first.
    let mut offset = 0;

    for subtag in tag.split('-') {
        if subtag.len() == 1 && offset > 0 {
            return (&tag[..offset - 1], &tag[offset - 1..]);
        }

        offset += subtag.len() + 1;
    }

    (tag, "")
}

/// IsWellFormedLanguageTag (6.2.1) steps 4-11: the two checks the grammar itself cannot make,
/// because ICU4X's parser normalizes a repeated variant away instead of refusing it. Both of them
/// look at the variants of a run of subtags, once for the base name (steps 5-6) and once for the
/// tlang of the `-t-` extension (steps 11.5-11.7).
pub(crate) fn has_duplicate_variants(tag: &str) -> bool {
    let lower = tag.to_ascii_lowercase();

    // Steps 4-6.
    if duplicate_variants_in(locale_base_name(&lower)) {
        return true;
    }

    // Steps 7-11.7: only the `-t-` extension has a language of its own to check.
    transformed_language(&lower).is_some_and(duplicate_variants_in)
}

/// Whether two of the subtags of `run` that the variant production matches are the same. The caller
/// has already lowercased, so the comparison is plain equality.
fn duplicate_variants_in(run: &str) -> bool {
    let mut seen: Vec<&str> = Vec::new();

    for subtag in run.split('-').skip(1) {
        if !variant_subtag(subtag) {
            continue;
        }

        if seen.contains(&subtag) {
            return true;
        }

        seen.push(subtag);
    }

    false
}

/// The tlang of the `transformed_extensions` sequence of `tag`: the subtags after `-t-` up to the
/// next singleton, which is where 6.2.1 step 11.5's longest prefix matched by `tlang` ends. Returns
/// `None` when the tag has no `-t-` sequence.
fn transformed_language(tag: &str) -> Option<&str> {
    // Step 10: whatever comes after a private use sequence takes part in no check at all.
    let tag = match tag.find("-x-") {
        Some(index) => &tag[..index],
        None => tag,
    };

    let start = tag.find("-t-").map(|index| index + 3)?;

    // Steps 11.5: the run ends at the first singleton subtag, which begins another extension.
    let mut end = start;
    for subtag in tag[start..].split('-') {
        if subtag.len() == 1 {
            break;
        }

        end += subtag.len() + 1;
    }

    // The loop counted the hyphen after the last subtag it took, which is not part of the tlang.
    Some(&tag[start..end.saturating_sub(1).max(start)])
}

/// The record of 15.1.3 step 8: the tag that 15.1.1 step 36 stores as [[Locale]], and the value each
/// key of 15.2.2 ended up with, `None` for a keyword the request left out and the empty String for
/// one it wrote without a value.
type LocaleRecord = (String, Vec<(String, Option<String>)>);

/// MakeLocaleRecord (https://tc39.es/ecma402/#sec-makelocalerecord), 15.1.3, for the keys
/// `Intl.Locale` reads (15.2.2). Returns the record's [[locale]] with the value each key ended up
/// with: `None` is the spec's undefined, and the empty String is a key the tag wrote without a
/// value, which is what 15.5.6 and 15.4 both keep.
///
/// The two are returned together because they cannot disagree: step 5 takes the extension off the
/// tag and step 6 writes every keyword the record ended up with back onto it, so a slot is always
/// readable from [[Locale]] alone - which is what lets an instance store one string rather than
/// eight.
///
/// The error is step 6's canonicalization refusing the tag, which 15.1.1 turns into the RangeError of
/// [`throw_locale_error`](super::locale_object). The clause writes the step as infallible because it
/// assumes CanonicalizeUnicodeLocaleId accepts every well-formed tag, and Annex C does; this engine's
/// canonicalization is ICU4X's, which takes the `unicode_language_subtag` of 4 to 8 letters that
/// 6.2.1's grammar allows and calls it a syntax error. `{ language: "english" }` is that case.
pub(crate) fn make_locale_record(
    tag: &str,
    locale_extension_keys: &[&str],
    overrides: &[(&str, Option<&str>)],
) -> Result<LocaleRecord, LocaleError> {
    // Steps 1.1-1.4.
    let (_, sequence) = take_unicode_extension(tag);
    let (attributes, mut keywords) = if sequence.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        unicode_extension_components(&sequence.to_ascii_lowercase())
    };

    // Step 3, and the loop of step 4 over the keys 15.2.2 gives the constructor.
    let mut values: Vec<(String, Option<String>)> = Vec::with_capacity(locale_extension_keys.len());

    for key in locale_extension_keys {
        // Steps 4.1-4.2: the tag's own value, if it wrote one.
        let entry = keywords.iter().position(|(name, _)| name == key);
        let mut value = entry.map(|index| keywords[index].1.clone());

        // Steps 4.3-4.5: an option overrides it, canonicalized the way the key's data wants.
        let override_value = overrides
            .iter()
            .find(|(name, _)| name == key)
            .and_then(|(_, value)| value.as_deref());

        if let Some(override_value) = override_value {
            let canonical = canonical_u_value(key, override_value);

            match entry {
                // Step 4.5.2: the tag wrote the key, so its entry changes in place.
                Some(index) => keywords[index].1.clone_from(&canonical),
                // Step 4.5.3: it did not, so the key joins them.
                None => keywords.push(((*key).to_owned(), canonical.clone())),
            }

            value = Some(canonical);
        }

        // Step 4.6.
        values.push(((*key).to_owned(), value));
    }

    // Steps 5-7.
    let (base, _) = take_unicode_extension(tag);
    let locale = if attributes.is_empty() && keywords.is_empty() {
        canonicalize_locale_tag(&base)?
    } else {
        // Step 6.1's keywords are written as the values 9.2.5 read them, so an empty one goes back
        // as the bare spelling.
        let keywords = keywords
            .iter()
            .map(|(key, value)| (key.clone(), (!value.is_empty()).then(|| value.clone())))
            .collect::<Vec<_>>();

        insert_unicode_extension_and_canonicalize(&CANONICALIZER, &base, &attributes, &keywords)?
    };

    Ok((locale, values))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|tag| (*tag).to_owned()).collect()
    }

    fn canonical(tags: &[&str]) -> Vec<String> {
        canonicalize_locale_list(&list(tags)).unwrap()
    }

    #[test]
    fn the_default_locale_is_built_in() {
        assert!(
            BuiltInLocales::new().tags().contains(&default_locale()),
            "the fallback locale {} is not one of the built in locales",
            default_locale()
        );
    }

    #[test]
    fn a_built_in_locale_canonicalizes_to_itself() {
        for tag in BuiltInLocales::new().tags() {
            assert_eq!(canonicalize_locale_tag(tag).unwrap(), *tag);
        }
    }

    #[test]
    fn a_type_value_of_true_is_written_as_the_bare_key() {
        // TR35 §3.2.1 puts this among the canonical *syntax* of a sequence - "Any type or tfield
        // value 'true' is removed" - so the key stands alone, and CanonicalizeUnicodeLocaleId
        // (6.2.2) step 1 therefore answers "en-u-kn" for "en-u-kn-true". This is what a service's
        // resolved locale reports: test262's
        // `Collator/prototype/resolvedOptions/resolved-numeric-unicode-extensions-and-options.js`
        // and `Collator/missing-unicode-ext-value-defaults-to-true.js` both check the spelling.
        assert_eq!(canonicalize_locale_tag("en-u-kn-true").unwrap(), "en-u-kn");
        assert_eq!(canonicalize_locale_tag("en-u-kn-false").unwrap(), "en-u-kn-false");
        assert_eq!(canonicalize_locale_tag("en-u-kn").unwrap(), "en-u-kn");

        // The casing of the sequence is regularized and its keys sorted, and a private use sequence
        // stays where BCP 47 puts it: last.
        assert_eq!(canonicalize_locale_tag("EN-U-KN-TRUE").unwrap(), "en-u-kn");
        assert_eq!(canonicalize_locale_tag("en-u-kf-lower-kn-TRUE").unwrap(), "en-u-kf-lower-kn");
        assert_eq!(canonicalize_locale_tag("en-u-kn-true-x-icu").unwrap(), "en-u-kn-x-icu");

        // A key `Intl` itself does not read gets no CanonicalizeUValue - the data of its aliases is
        // not baked - but the rule above is about the syntax of any sequence, so it covers that key
        // as well.
        assert_eq!(canonicalize_locale_tag("en-u-kb-true").unwrap(), "en-u-kb");
        assert_eq!(canonicalize_locale_tag("en-u-kb-yes").unwrap(), "en-u-kb-yes");
    }

    #[test]
    fn case_is_regularized() {
        assert_eq!(canonical(&["EN"]), ["en"]);
        assert_eq!(canonical(&["en-gb"]), ["en-GB"]);
        assert_eq!(canonical(&["IT-it"]), ["it-IT"]);
        assert_eq!(canonical(&["zh-hant-tw"]), ["zh-Hant-TW"]);
        assert_eq!(canonical(&["ES-419"]), ["es-419"]);
    }

    #[test]
    fn unicode_extensions_are_kept() {
        assert_eq!(canonical(&["en-US-u-hc-h23"]), ["en-US-u-hc-h23"]);
        assert_eq!(canonical(&["it-IT-u-nu-latn"]), ["it-IT-u-nu-latn"]);
        assert_eq!(canonical(&["da-DK-u-hc-h24"]), ["da-DK-u-hc-h24"]);
    }

    #[test]
    fn unicode_extension_keys_are_sorted_and_deduplicated() {
        let forwards = canonicalize_locale_tag("en-u-ca-gregory-nu-latn").unwrap();
        let backwards = canonicalize_locale_tag("en-u-nu-latn-ca-gregory").unwrap();

        assert_eq!(forwards, backwards);
        assert_eq!(forwards, "en-u-ca-gregory-nu-latn");

        // The first value for a key wins (9.2.5), and the key is written only once.
        assert_eq!(
            canonicalize_locale_tag("en-u-nu-latn-hc-h23-nu-digit").unwrap(),
            "en-u-hc-h23-nu-latn"
        );

        // A key followed straight by another key has no value of its own (9.2.5 steps 8.7-8.8).
        assert_eq!(canonicalize_locale_tag("en-u-nu-ca-gregory").unwrap(), "en-u-ca-gregory-nu");
    }

    #[test]
    fn aliased_subtags_are_replaced() {
        // CLDR prefers "fil" for "tl"; the alias table is the data that makes this true.
        assert_eq!(canonical(&["tl"]), ["fil"]);
    }

    #[test]
    fn duplicates_are_removed_keeping_the_first() {
        assert_eq!(canonical(&["en", "en-GB", "EN"]), ["en", "en-GB"]);
        assert_eq!(canonical(&["en-US-u-hc-h23", "en-us-u-HC-h23"]), ["en-US-u-hc-h23"]);
    }

    #[test]
    fn a_malformed_tag_is_an_error() {
        assert_eq!(canonicalize_locale_tag("").unwrap_err(), LocaleError::InvalidTag);
        assert_eq!(canonicalize_locale_tag("en_US").unwrap_err(), LocaleError::InvalidTag);
        assert_eq!(canonicalize_locale_tag("en-").unwrap_err(), LocaleError::InvalidTag);
        assert_eq!(canonicalize_locale_tag("en-US-").unwrap_err(), LocaleError::InvalidTag);
        assert_eq!(canonicalize_locale_tag("i-navajo").unwrap_err(), LocaleError::InvalidTag);
    }

    #[test]
    fn the_unknown_locale_is_a_tag_of_its_own() {
        assert_eq!(canonical(&["und"]), ["und"]);
    }

    #[test]
    fn lookup_matches_the_longest_prefix() {
        let available = BuiltInLocales::new().tags();

        assert_eq!(
            lookup_prefix(available, "en-GB").unwrap(),
            LocaleMatch {
                locale: "en".to_owned(),
                requested_base: "en-GB".to_owned(),
                extension: String::new(),
            }
        );
        assert_eq!(lookup_prefix(available, "it-IT").unwrap().locale, "it",);
        assert_eq!(
            lookup_prefix(available, "en-US-u-hc-h23")
                .unwrap()
                .extension,
            "-u-hc-h23"
        );
        assert_eq!(
            lookup_prefix(available, "it-IT-u-nu-latn")
                .unwrap()
                .extension,
            "-u-nu-latn"
        );

        // Single character subtags are cut out whole, not left dangling.
        assert_eq!(lookup_prefix(available, "en-x-icu").unwrap().locale, "en");
    }

    #[test]
    fn lookup_gives_up_on_an_unsupported_language() {
        let available = BuiltInLocales::new().tags();

        assert_eq!(lookup_prefix(available, "de"), None);
        assert_eq!(lookup_prefix(available, "zh-Hant-TW"), None);
        assert_eq!(lookup_prefix(available, "und"), None);
        assert_eq!(
            lookup_supported(available, &list(&["de", "it"]))
                .unwrap()
                .locale,
            "it"
        );
        assert_eq!(lookup_supported(available, &list(&["de", "fr"])), None);
    }

    #[test]
    fn resolution_falls_back_instead_of_failing() {
        // 9.2.7 step 4: an unsupported request still resolves, to the default locale.
        assert_eq!(resolve_locale(&list(&["de"]), Matcher::Lookup).locale, default_locale());
        assert_eq!(
            resolve_locale(&list(&["zh-Hant-TW"]), Matcher::BestFit).locale,
            default_locale()
        );
        assert_eq!(resolve_locale(&[], Matcher::Lookup).locale, default_locale());

        // The reported tag is the request ("it-IT"), the data locale the available key it matched
        // ("it"): a NumberFormat names the region the caller asked for while formatting on the
        // parent language's data, as V8 and SpiderMonkey do.
        assert_eq!(
            resolve_locale(&list(&["it-IT"]), Matcher::Lookup),
            ResolvedLocale {
                locale: "it-IT".to_owned(),
                data_locale: "it".to_owned(),
                extensions: Vec::new(),
                values: Vec::new(),
            }
        );
    }

    #[test]
    fn a_relevant_key_is_resolved_from_the_extension_then_the_option() {
        // The one key NumberFormat (16.2.3) has, whose data supports no value but Latin digits.
        let key = |option_value: Option<&str>| RelevantExtensionKey {
            key: "nu",
            supported_values: &["latn"],
            option_value: option_value.map(str::to_owned),
            option_is_null: false,
        };
        let resolve = |tags: &[&str], option_value: Option<&str>| {
            resolve_locale_with_keys(
                &canonical(tags),
                Matcher::Lookup,
                std::slice::from_ref(&key(option_value)),
            )
        };

        // 9.2.7 steps 12.1-12.4 and 12.13: a request that says nothing about the key gets the
        // locale's default, and step 13 writes nothing onto the tag.
        let plain = resolve(&["en"], None);
        assert_eq!(plain.locale, "en");
        assert!(plain.extensions.is_empty());
        assert_eq!(plain.values, [("nu".to_owned(), "latn".to_owned())]);

        // Steps 12.6.1-12.6.3: a supported value of the extension is taken and copied onto the tag.
        let latin = resolve(&["en-u-nu-latn"], None);
        assert_eq!(latin.locale, "en-u-nu-latn");
        assert_eq!(latin.data_locale, "en", "the data must not see the extension");
        assert_eq!(latin.extensions, [("nu".to_owned(), "latn".to_owned())]);

        // Step 12.6.3 again: an unsupported value is dropped quietly, both from the tag and from
        // the resolved value, which is the standing product decision made visible.
        let arab = resolve(&["en-u-nu-arab"], None);
        assert_eq!(arab.locale, "en");
        assert_eq!(arab.values, [("nu".to_owned(), "latn".to_owned())]);

        // Steps 12.8-12.11: an option that names the value the key already resolved to changes
        // nothing, and step 12.11's rewrite of `supportedKeyword` needs an option value that is
        // supported and *different* from the value reached through the extension. With one
        // supported value there is no such thing, so this build never takes that branch.
        let from_option = resolve(&["en-u-nu-arab"], Some("latn"));
        assert_eq!(from_option.locale, "en");
        assert_eq!(from_option.values, [("nu".to_owned(), "latn".to_owned())]);

        // Step 12.11 again: an unsupported option cannot strip a keyword the extension had a
        // supported value for.
        let overridden = resolve(&["en-u-nu-latn"], Some("arab"));
        assert_eq!(overridden.locale, "en-u-nu-latn");
        assert_eq!(overridden.values, [("nu".to_owned(), "latn".to_owned())]);

        // 9.2.7 step 4: an unsupported language falls back whole, extension included, so the
        // keyword of a request for "de" never reaches the tag of the locale that answers it.
        let fallback = resolve(&["de-u-nu-latn"], None);
        assert_eq!(fallback.locale, "en");
        assert_eq!(fallback.data_locale, "en");
    }

    #[test]
    fn a_blank_option_takes_the_keyword_off_the_tag() {
        // "hc" is the one key whose locale data has a null among its values (11.2.3), so a
        // resolution record that carries the key blank beats the `-u-hc-` of the request and takes it
        // off the tag, which is 9.2.7 step 12.11 read with `optionsValue` null. A `DateTimeFormat`
        // blanks it whenever a `hour12` was given, whatever the `hourCycle` option said beside it
        // (11.1.2 step 3.3).
        let asked = |option_value: Option<&str>, option_is_null: bool| RelevantExtensionKey {
            key: "hc",
            supported_values: &["h11", "h12", "h23", "h24"],
            option_value: option_value.map(str::to_owned),
            option_is_null,
        };
        let resolve_hc = |option_value: Option<&str>, option_is_null: bool| {
            resolve_locale_with_keys(
                &canonical(&["en-u-hc-h11"]),
                Matcher::Lookup,
                &[asked(option_value, option_is_null)],
            )
        };

        // Steps 12.6.3 and 12.13, with the record saying nothing: the keyword the request spelled is
        // both resolved and reported.
        let untouched = resolve_hc(None, false);
        assert_eq!(untouched.locale, "en-u-hc-h11");
        assert_eq!(untouched.values, [("hc".to_owned(), "h11".to_owned())]);

        // Step 12.11, with a null: taken off the tag.
        assert_eq!(resolve_hc(None, true).locale, "en");

        // Step 12.11, with a supported value of its own: off the tag as well, rather than rewritten,
        // and the resolved value is the option's.
        let replaced = resolve_hc(Some("h23"), false);
        assert_eq!(replaced.locale, "en");
        assert_eq!(replaced.values, [("hc".to_owned(), "h23".to_owned())]);

        // Step 12.11's other branch: an option that names the value the tag already had changes
        // nothing, so the keyword is still reported.
        assert_eq!(resolve_hc(Some("h11"), false).locale, "en-u-hc-h11");

        // And the blank does not invent a keyword the request never asked for.
        assert_eq!(
            resolve_locale_with_keys(&canonical(&["en"]), Matcher::Lookup, &[asked(None, true)],)
                .locale,
            "en"
        );
    }

    #[test]
    fn supported_locales_are_the_requested_ones() {
        // 9.2.9 step 4.3: the tag that was asked for, in the order it was asked for.
        assert_eq!(
            supported_locales_of(&canonical(&["en-GB", "de", "it-IT", "da-DK"]), Matcher::Lookup),
            ["en-GB", "it-IT", "da-DK"]
        );
        assert!(
            supported_locales_of(&canonical(&["de-DE"]), Matcher::BestFit).is_empty(),
            "an unsupported locale is not supported"
        );
    }

    /// ResolveLocale of `requested` for a service whose `relevantExtensionKeys` are `keys`.
    fn resolve(requested: &[&str], keys: &[ExtensionKey]) -> ServiceLocale {
        resolve_locale_with_extension_keys(
            &CANONICALIZER,
            &canonical(requested),
            Matcher::BestFit,
            keys,
        )
    }

    /// The collation key of a collator: the spec's `[null]`, since no collation type is baked in.
    const COLLATION: &[Option<&str>] = &[None];

    /// The numeric key, whose CLDR values are "false" and "true" with "false" the default.
    const NUMERIC: &[Option<&str>] = &[Some("false"), Some("true")];

    /// The case first key, whose values 10.2.3 names for a collator.
    const CASE_FIRST: &[Option<&str>] = &[Some("false"), Some("upper"), Some("lower")];

    #[test]
    fn a_requested_extension_key_is_taken_when_the_data_has_it() {
        let numeric = [ExtensionKey { key: "kn", values: NUMERIC, requested: None }];

        let resolved = resolve(&["en-u-kn-true"], &numeric);

        // 9.2.7 step 13.2 ends in CanonicalizeUnicodeLocaleId, whose Annex C syntax removes a type
        // value of "true" (see `a_type_value_of_true_is_written_as_the_bare_key`), so the resolved
        // tag says the key alone. That is exactly what the two cases of test262's
        // `Collator/prototype/resolvedOptions/resolved-numeric-unicode-extensions-and-options.js`
        // expect, and it is the difference between "-kn-true" and "-kn" running out of a request.
        assert_eq!(resolved.locale, "en-u-kn");
        assert_eq!(resolved.data_locale, "en");
        assert_eq!(resolved.values, [Some("true".to_owned())]);
    }

    #[test]
    fn a_bare_extension_key_means_true_and_keeps_the_short_spelling() {
        // "-u-kn" asks for the same ordering as "-u-kn-true" (9.2.7 step 12.6.4), and the resolved
        // tag says it the way the request did, which is what `resolvedOptions().locale` shows.
        let numeric = [ExtensionKey { key: "kn", values: NUMERIC, requested: None }];

        let resolved = resolve(&["da-u-kn"], &numeric);
        assert_eq!(resolved.locale, "da-u-kn");
        assert_eq!(resolved.values, [Some("true".to_owned())]);
    }

    #[test]
    fn an_option_beats_the_tag_and_the_tag_is_then_left_alone() {
        // Steps 12.10-12.11: the options value wins, and the keyword the request contributed is
        // dropped from the tag because it no longer says what was resolved.
        let numeric = [ExtensionKey { key: "kn", values: NUMERIC, requested: Some("false") }];

        let resolved = resolve(&["en-u-kn-true"], &numeric);
        assert_eq!(resolved.locale, "en");
        assert_eq!(resolved.values, [Some("false".to_owned())]);
    }

    #[test]
    fn an_option_value_the_tag_did_not_ask_for_is_not_written_onto_it() {
        // Step 12.12 writes only the keywords of step 12.6, and a request that never mentioned the
        // key contributed none: `numeric: true` resolves to plain "en", and `[[Numeric]]` is what
        // carries the answer.
        let numeric = [ExtensionKey { key: "kn", values: NUMERIC, requested: Some("true") }];

        let resolved = resolve(&["en"], &numeric);
        assert_eq!(resolved.locale, "en");
        assert_eq!(resolved.values, [Some("true".to_owned())]);
    }

    #[test]
    fn an_empty_option_value_means_true() {
        // Step 12.10.3, which ResolveOptions step 6.4.2 keeps the JS layer from reaching with "".
        let numeric = [ExtensionKey { key: "kn", values: NUMERIC, requested: Some("") }];

        let resolved = resolve(&["en"], &numeric);
        assert_eq!(resolved.values, [Some("true".to_owned())]);
    }

    #[test]
    fn a_key_the_locale_has_no_data_for_is_null_and_never_written() {
        // A build with no collation types resolves "co" to the spec's null, which `[[collation]]`
        // reports as "default", and neither the tag nor an option can ask for anything else.
        let collation = [ExtensionKey { key: "co", values: COLLATION, requested: Some("emoji") }];

        let resolved = resolve(&["en-u-co-standard"], &collation);
        assert_eq!(resolved.locale, "en");
        assert_eq!(resolved.values, [None]);
    }

    #[test]
    fn an_unsupported_locale_loses_the_extension_it_asked_for() {
        // ResolveLocale step 4 takes the default locale, whose extension is empty, so ordering
        // asked for in German never reaches the English collator.
        let numeric = [ExtensionKey { key: "kn", values: NUMERIC, requested: None }];

        let resolved = resolve(&["de-u-kn-true"], &numeric);
        assert_eq!(resolved.locale, "en");
        assert_eq!(resolved.data_locale, "en");
        assert_eq!(resolved.values, [Some("false".to_owned())]);
    }

    #[test]
    fn an_option_of_an_unsupported_locale_still_applies_to_the_fallback() {
        // The options are not tied to a locale, so `numeric` is read whatever was requested.
        let numeric = [ExtensionKey { key: "kn", values: NUMERIC, requested: Some("TRUE") }];

        let resolved = resolve(&["de-u-kn-true"], &numeric);
        assert_eq!(resolved.values, [Some("true".to_owned())]);
    }

    #[test]
    fn the_written_keywords_are_sorted_whatever_order_the_keys_came_in() {
        // Step 13.1-13.2 rebuilds the tag through CanonicalizeUnicodeLocaleId, which orders the
        // keywords by key rather than by the order the service lists its relevant keys in.
        let keys = [
            ExtensionKey { key: "co", values: COLLATION, requested: None },
            ExtensionKey { key: "kn", values: NUMERIC, requested: None },
            ExtensionKey { key: "kf", values: CASE_FIRST, requested: None },
        ];

        let resolved = resolve(&["en-u-kf-upper-kn-true"], &keys);
        assert_eq!(resolved.locale, "en-u-kf-upper-kn");
        assert_eq!(resolved.values, [None, Some("true".to_owned()), Some("upper".to_owned())]);
    }

    /// The base name and the four subtag getters, over the tags test262's
    /// `Locale/getters-missing.js` and `Locale/getters.js` walk through.
    #[test]
    fn a_base_name_is_the_tag_up_to_its_first_extension() {
        assert_eq!(locale_base_name("sv"), "sv");
        assert_eq!(locale_base_name("sv-Latn"), "sv-Latn");
        assert_eq!(locale_base_name("sv-SE"), "sv-SE");
        assert_eq!(locale_base_name("sv-Latn-SE"), "sv-Latn-SE");
        assert_eq!(locale_base_name("sv-Latn-SE-1996-fonupa"), "sv-Latn-SE-1996-fonupa");
        assert_eq!(locale_base_name("sv-Latn-SE-u-ca-buddhist"), "sv-Latn-SE");
        assert_eq!(locale_base_name("sv-Latn-SE-x-foo"), "sv-Latn-SE");
        assert_eq!(locale_base_name("de-419"), "de-419");

        // A 3-letter subtag is none of script, region or variant, so it ends the base name: the
        // extended language form of BCP 47 is not part of `unicode_language_id`
        // (https://unicode.org/reports/tr35/#unicode_language_id).
        assert_eq!(locale_base_name("zh-yue"), "zh");
    }

    #[test]
    fn the_subtag_getters_read_the_base_name_only() {
        assert_eq!(locale_language("sv-Latn-SE-1996-u-ca-buddhist"), "sv");
        assert_eq!(locale_script("sv-Latn-SE-1996"), Some("Latn"));
        assert_eq!(locale_script("sv-SE-1996"), None);
        assert_eq!(locale_script("sv-u-ca-buddhist"), None);

        // 15.5.4 steps 5 and 6: the region is the subtag after the language, or after the script
        // between them.
        assert_eq!(locale_region("sv-Latn-SE-1996"), Some("SE"));
        assert_eq!(locale_region("sv-SE-1996"), Some("SE"));
        assert_eq!(locale_region("sv-1996"), None);
        assert_eq!(locale_region("de-419-u-nu-latn"), Some("419"));
        assert_eq!(locale_region("sv-u-ca-buddhist-x-foo"), None);

        // 15.5.5 returns the whole run from the first variant to the end of the base name.
        assert_eq!(locale_variants("sv-Latn-SE-1996-fonupa"), Some("1996-fonupa"));
        assert_eq!(locale_variants("sv-Latn-SE-fonupa-1996"), Some("fonupa-1996"));
        assert_eq!(locale_variants("sv-Latn-SE"), None);
        assert_eq!(locale_variants("sv-Latn-SE-1996-u-ca-buddhist"), Some("1996"));
    }

    #[test]
    fn the_subtag_productions_are_disjoint() {
        // The lengths and the digit rules that keep a subtag in exactly one production.
        assert!(language_subtag("en") && language_subtag("yue") && language_subtag("abcde"));
        assert!(!language_subtag("a"));
        assert!(!language_subtag("ab7"));
        assert!(!language_subtag("notalanguage"));
        assert!(!language_subtag("undefined"));
        assert!(!language_subtag("abgh"));
        assert!(!language_subtag("root"));
        assert!(!language_subtag("ROOT"));
        assert!(!language_subtag("İ"));

        assert!(script_subtag("Latn") && script_subtag("Cyrl"));
        assert!(!script_subtag("abc"));
        assert!(!script_subtag("abc7"));
        assert!(!script_subtag("Latn-SA"));

        assert!(region_subtag("US") && region_subtag("419") && region_subtag("001"));
        assert!(!region_subtag("abc"));
        assert!(!region_subtag("a7"));
        assert!(!region_subtag("notaregion"));

        assert!(variant_subtag("1996") && variant_subtag("fonupa") && variant_subtag("oxendict"));

        // Both branches of the production cover a value of 5 that begins with a digit, which is
        // still a variant either way.
        assert!(variant_subtag("19965"));
        assert!(!variant_subtag("abcd"));
        assert!(!variant_subtag("abcdefghi"));
        assert!(!variant_subtag("oed"));
        assert!(!variant_subtag("12"));
    }

    #[test]
    fn the_value_productions_of_the_option_checks() {
        // `type`, which the calendar, collation and firstDayOfWeek options are matched against.
        assert!(type_value("buddhist"));
        assert!(type_value("islamic-civil"));
        assert!(type_value("mon"));
        assert!(type_value("sunday"));
        assert!(!type_value(""));
        assert!(!type_value("a"));
        assert!(!type_value("ab"));
        assert!(!type_value("abcdefghi"));
        assert!(!type_value("abc-abcdefghi"));
        assert!(!type_value("latn-"));
        assert!(!type_value("-latn"));

        // `unicode_subdivision_id` is a region subtag and a suffix of 1-4 alphanumeric characters
        // with no separator between them (TR35 §3.2), which is why the real codes read "usca" and
        // "cz110" rather than "us-ca".
        assert!(subdivision_value("usca"));
        assert!(subdivision_value("no50"));
        assert!(subdivision_value("frges"));
        assert!(subdivision_value("cz110"));
        assert!(subdivision_value("fra"));
        assert!(subdivision_value("1234"));

        // The suffix is required, so a bare region is not a subdivision, and a separator makes the
        // value a different string that the production does not match.
        assert!(!subdivision_value("US"));
        assert!(!subdivision_value("us"));
        assert!(!subdivision_value("US-CA"));
        assert!(!subdivision_value("us-ca"));
        assert!(!subdivision_value("GB-ENG"));
        assert!(!subdivision_value("cz11000"));
        assert!(!subdivision_value("123"));
    }

    #[test]
    fn a_unicode_extension_value_is_the_first_one_written_for_the_key() {
        // 15.5.6, over a canonicalized tag.
        assert_eq!(unicode_extension_value("en-u-ca-buddhist", "ca").as_deref(), Some("buddhist"));
        assert_eq!(
            unicode_extension_value("en-u-nu-latn-ca-buddhist", "ca").as_deref(),
            Some("buddhist")
        );
        assert_eq!(unicode_extension_value("en-u-ca", "ca").as_deref(), Some(""));
        assert_eq!(unicode_extension_value("en-u-nu-latn", "ca"), None);
        assert_eq!(unicode_extension_value("en", "ca"), None);
        assert_eq!(unicode_extension_value("en-x-u-ca-buddhist", "ca"), None);
        assert_eq!(
            unicode_extension_value("en-u-ca-ethiopic-amite", "ca").as_deref(),
            Some("ethiopic-amite")
        );

        // 9.2.5 keeps the first value of a repeated key.
        assert_eq!(
            unicode_extension_value("en-u-ca-buddhist-ca-gregory", "ca").as_deref(),
            Some("buddhist")
        );
    }

    #[test]
    fn a_unicode_value_is_canonicalized_as_a_value_of_its_key() {
        // CanonicalizeUValue (9.2.2) is TR35 Annex C over CLDR's `common/bcp47` keyword tables. The
        // data this build carries for it is the boolean keys - a "true" type is the empty value,
        // which is what 15.1.1 step 42.1 reads a numeric extension as and what the bare key of a
        // resolved tag says - and, for the four keys whose type list is written into `icu_locale` as
        // a preference enum, that enum's own canonical spelling.
        assert_eq!(canonical_u_value("kn", "true"), "");
        assert_eq!(canonical_u_value("kf", "TRUE"), "");
        assert_eq!(canonical_u_value("ca", "buddhist"), "buddhist");
        assert_eq!(canonical_u_value("co", "standard"), "standard");
        assert_eq!(canonical_u_value("hc", "H23"), "h23");
        assert_eq!(canonical_u_value("fw", "mon"), "mon");

        // The type aliases the enums carry, which V8 applies the same way.
        assert_eq!(canonical_u_value("ca", "islamicc"), "islamic-civil");
        assert_eq!(canonical_u_value("ca", "ethiopic-amete-alem"), "ethioaa");

        // A value the tables know nothing about is only case-regularized, and so is a key the spec
        // does not read. `gregorian` is CLDR's deprecated spelling of the Gregorian calendar, but no
        // table this build reads holds the alias, which is why CalendarsOfLocale matches a preference
        // row against the calendar identifiers this build really has and, per 15.5.9 step 10, answers
        // the registry's `gregory` when nothing else did.
        assert_eq!(canonical_u_value("ca", "Gregorian"), "gregorian");
        assert_eq!(canonical_u_value("ca", "gregorian"), "gregorian");
        assert_eq!(canonical_u_value("co", "Phonebook"), "phonebook");
        assert_eq!(canonical_u_value("nu", "LATN"), "latn");
        assert_eq!(canonical_u_value("cu", "USD"), "usd");

        // A value that is not a `type` at all - too short for the production - still comes back
        // lowercase: it is the option check of 15.1.1 that refuses it, not this operation.
        assert_eq!(canonical_u_value("fw", "6"), "6");
    }

    #[test]
    fn a_weekday_is_a_name_and_a_number() {
        // Table 26, in both directions.
        for (string, name, number) in WEEKDAYS {
            assert_eq!(weekday_to_u_value(string), name);
            assert_eq!(weekday_u_value_to_number(name), Some(number));
        }

        assert_eq!(weekday_to_u_value("sunday"), "sunday");
        assert_eq!(weekday_to_u_value("venerdi"), "venerdi");
        assert_eq!(weekday_u_value_to_number("1"), None);
        assert_eq!(weekday_u_value_to_number("001"), None);
    }

    #[test]
    fn a_repeated_variant_is_not_a_well_formed_tag() {
        // 6.2.1 steps 5-6, which `Locale::from_str` swallows instead of refusing.
        assert!(has_duplicate_variants("en-emodeng-emodeng"));
        assert!(has_duplicate_variants("en-Emodeng-emodeng"));
        assert!(has_duplicate_variants("en-variant-emodeng-emodeng"));
        assert!(has_duplicate_variants("en-emodeng-variant-emodeng"));
        assert!(canonicalize_locale_tag("en-emodeng-emodeng").is_err());
        assert!(canonicalize_locale_tag("en-emodeng-variant-emodeng").is_err());
        assert!(canonicalize_locale_tag("en-1996-1996").is_err());

        // 6.2.1 steps 11.5-11.7, the same rule over the tlang of a `-t-` extension.
        assert!(has_duplicate_variants("de-t-en-emodeng-emodeng"));
        assert!(has_duplicate_variants("de-t-en-variant-emodeng-emodeng"));
        assert!(!has_duplicate_variants("de-t-en-emodeng"));
        assert!(canonicalize_locale_tag("de-t-en-emodeng-emodeng").is_err());

        // A private use sequence is ignored (step 8's note), and so is the content of a `-u-`
        // extension, whose values are never variants of a base name.
        assert!(!has_duplicate_variants("en-x-emodeng-emodeng"));
        assert!(!has_duplicate_variants("en-u-ca-emodeng-emodeng"));
        assert!(!has_duplicate_variants("en-emodeng-1996"));

        // Two different variants of the same base name.
        assert!(!has_duplicate_variants("en-1996-fonupa"));
    }

    #[test]
    fn likely_subtags_keep_the_extension_and_the_private_use_sequence() {
        assert_eq!(add_likely_subtags("en").as_deref(), Some("en-Latn-US"));

        // TR35 §3.2.1 defines the maximal form of a tag "excluding und", but the CLDR likely-subtags
        // data does hold a row for it, and 15.3.9 step 3 answers the row: test262's
        // `Locale/likely-subtags.js` reads `new Intl.Locale("und").maximize()` as "en-Latn-US".
        assert_eq!(add_likely_subtags("und").as_deref(), Some("en-Latn-US"));
        assert_eq!(add_likely_subtags("en-GB").as_deref(), Some("en-Latn-GB"));
        assert_eq!(add_likely_subtags("en-Shaw").as_deref(), Some("en-Shaw-GB"));
        assert_eq!(add_likely_subtags("it-Kana-CA").as_deref(), Some("it-Kana-CA"));
        assert_eq!(add_likely_subtags("en-u-nu-arab").as_deref(), Some("en-Latn-US-u-nu-arab"));
        assert_eq!(add_likely_subtags("en-x-icu").as_deref(), Some("en-Latn-US-x-icu"));

        // An attribute or a transformed language is as much outside the tables as the `-u-` extension
        // is, which test262's `Locale/likely-subtags.js` checks with `en-a-not-assigned`.
        assert_eq!(
            add_likely_subtags("en-a-not-assigned").as_deref(),
            Some("en-Latn-US-a-not-assigned")
        );
        assert_eq!(
            add_likely_subtags("en-a-bar-u-nu-arab-x-icu").as_deref(),
            Some("en-Latn-US-a-bar-u-nu-arab-x-icu")
        );
        assert_eq!(
            add_likely_subtags("en-t-it-Latn-US").as_deref(),
            Some("en-Latn-US-t-it-Latn-US")
        );

        assert_eq!(remove_likely_subtags("en-Latn-US").as_deref(), Some("en"));

        // Minimization removes the script and region that a variant implies, but not the variant.
        assert_eq!(remove_likely_subtags("en-Shaw-GB").as_deref(), Some("en-Shaw"));
        assert_eq!(remove_likely_subtags("en-Latn-US-u-nu-arab").as_deref(), Some("en-u-nu-arab"));
        assert_eq!(remove_likely_subtags("en-Latn-US-a-bar").as_deref(), Some("en-a-bar"));

        // A tag the tables know nothing about is left as it is, which is what 15.3.9 step 3's
        // "if an error is signaled" branch then answers with.
        assert_eq!(add_likely_subtags("it-Kana-CA").as_deref(), Some("it-Kana-CA"));
        assert_eq!(remove_likely_subtags("qq-QQ").as_deref(), Some("qq-QQ"));
    }

    #[test]
    fn the_region_a_locale_info_getter_reads() {
        // Step 1: the region the tag names.
        assert_eq!(region_preference("en-US").region, "US");
        assert_eq!(region_preference("da-DK").region, "DK");

        // Steps 2.1-2.3: no region at all, so the likely subtags answer for it.
        assert_eq!(region_preference("en").region, "US");
        assert_eq!(region_preference("da").region, "DK");
        assert_eq!(region_preference("it").region, "IT");

        // Steps 2.1-2.3: no region at all, so the likely subtags answer for it - including for the
        // root language, whose row step 2.2 reads is the one that makes `und` "en-Latn-US".
        assert_eq!(region_preference("und").region, "US");

        // Step 2.4: a language the tables know nothing about has no region either, so the whole world
        // is what a Locale Info getter reads for it.
        assert_eq!(region_preference("qq").region, "001");

        // A tag whose language has no likely region keeps the region it wrote.
        assert_eq!(region_preference("qq-QQ").region, "QQ");

        // Step 3: the override is read from the `rg` key, canonicalized through its own region.
        assert_eq!(
            region_preference("en-u-rg-inzzzz")
                .region_override
                .as_deref(),
            Some("IN")
        );
        assert_eq!(
            region_preference("en-US-u-rg-de01")
                .region_override
                .as_deref(),
            Some("DE")
        );
        assert_eq!(region_preference("en-US").region_override, None);

        // A value that is not a subdivision id is no override at all: the suffix of the production
        // is required, so a bare region does not name a subdivision either.
        assert_eq!(region_preference("en-u-rg-1").region_override, None);
        assert_eq!(region_preference("en-u-rg-in").region_override, None);
        assert_eq!(region_preference("en-u-rg-longerThan8Chars").region_override, None);

        // Step 2.1.2: `sd` gives the region when the tag has none of its own.
        assert_eq!(region_preference("en-u-sd-usca").region.as_str(), "US");
        assert_eq!(region_preference("it-u-sd-it32").region.as_str(), "IT");

        // The CLDR spelling of a subdivision is not the BCP 47 one, so `sd` says nothing here and
        // it is step 2.3's likely subtags that give the region.
        assert_eq!(region_preference("en-u-sd-us-ca").region.as_str(), "US");

        // Step 2.4: nothing in the tag, the subdivisions or the tables gives a region.
        assert_eq!(region_preference("zh-yue").region.as_str(), "001");
    }

    #[test]
    fn a_subdivision_reports_the_region_it_is_inside_of() {
        // A subdivision id names its region as its prefix, and steps 5-6 canonicalize that prefix on
        // its own - which is what makes an alias table, not this operation, the thing that decides
        // what region a code is inside of.
        assert_eq!(canonical_unicode_subdivision("en-u-rg-usca", "rg").as_deref(), Some("US"));
        assert_eq!(canonical_unicode_subdivision("en-u-rg-inzzzz", "rg").as_deref(), Some("IN"));
        assert_eq!(canonical_unicode_subdivision("it-u-sd-it32", "sd").as_deref(), Some("IT"));
        assert_eq!(canonical_unicode_subdivision("en-u-sd-12345", "sd").as_deref(), Some("123"));

        // The suffix of the production is required and its separator is nothing, so these values are
        // not subdivisions at all and step 3 returns undefined.
        assert_eq!(canonical_unicode_subdivision("en-u-rg-gb", "rg"), None);
        assert_eq!(canonical_unicode_subdivision("en-u-rg-uk", "rg"), None);
        assert_eq!(canonical_unicode_subdivision("en-u-rg-us-ca", "rg"), None);
        assert_eq!(canonical_unicode_subdivision("en-u-sd-us-ca", "sd"), None);
        assert_eq!(canonical_unicode_subdivision("en-u-rg-zz", "rg"), None);
        assert_eq!(canonical_unicode_subdivision("en-u-rg-longerThan8Chars", "rg"), None);
        assert_eq!(canonical_unicode_subdivision("en-u-rg", "rg"), None);
        assert_eq!(canonical_unicode_subdivision("en", "rg"), None);

        // `sd` and `rg` are read apart: one key's value is not the other's.
        assert_eq!(canonical_unicode_subdivision("en-u-rg-usca", "sd"), None);
        assert_eq!(canonical_unicode_subdivision("en-u-sd-usca", "rg"), None);
    }
}
