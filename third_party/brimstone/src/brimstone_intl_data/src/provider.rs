// The generated data refers to ICU4X crates by name and expands imports that are not all used by
// every marker set, matching the handling of the generated Unicode data in `common/icu_data`.
#![allow(unused_imports)]
// The collation tables nest an `unsafe` constructor of each `ZeroVec` inside the `unsafe` block of
// the trie that holds it, which the compiler reports as redundant.
#![allow(unused_unsafe)]

/// The baked ICU4X data provider for Intl: the locale tables of `icu/intl_markers.txt` (aliases,
/// parents, likely subtags, script directions), the collation and NFD data that `Intl.Collator`
/// compares with, the decimal symbols and digits that `Intl.NumberFormat` renders with and the
/// cardinal, ordinal and ranges rules that `Intl.PluralRules` selects with. Load it with the
/// `_unstable` ICU4X constructors, e.g.
/// `LocaleCanonicalizer::try_new_extended_unstable(&IntlDataProvider)`.
pub struct IntlDataProvider;

include!("data/mod.rs");

const _: () = {
    impl_data_provider!(IntlDataProvider);
};

#[cfg(test)]
mod tests {
    use fixed_decimal::UnsignedDecimal;
    use icu_collator::{Collator, CollatorPreferences, options::CollatorOptions};
    use icu_locale::Locale;
    use icu_plurals::{
        PluralCategory, PluralOperands, PluralRuleType, PluralRules, PluralRulesOptions,
        PluralRulesPreferences, PluralRulesWithRanges,
    };
    use icu_segmenter::{
        GraphemeClusterSegmenter, SentenceSegmenter, WordSegmenter,
        options::{SentenceBreakOptions, WordBreakOptions},
    };

    use super::IntlDataProvider;
    use crate::BUILT_IN_LOCALES;

    /// Whether `left` sorts before `right` in `tag`, with all options at their defaults.
    fn sorts_before(tag: &str, left: &str, right: &str) -> bool {
        let locale = tag.parse::<Locale>().expect("a built in locale parses");
        let collator = Collator::try_new_unstable(
            &IntlDataProvider,
            CollatorPreferences::from(&locale),
            CollatorOptions::default(),
        )
        .unwrap_or_else(|error| panic!("no collator for {tag}: {error}"));

        collator
            .as_borrowed()
            .compare_utf8(left.as_bytes(), right.as_bytes())
            .is_lt()
    }

    #[test]
    fn every_built_in_locale_has_collation_data() {
        for tag in BUILT_IN_LOCALES {
            assert!(sorts_before(tag, "a", "b"), "{tag} compares the ordinary alphabet");
        }
    }

    #[test]
    fn the_danish_tailoring_is_baked_in() {
        // æ, ø and å sort after z, which only the `da` tailoring can say.
        assert!(sorts_before("da", "z", "æ"));
        assert!(sorts_before("da", "æ", "ø"));
        assert!(sorts_before("da", "ø", "å"));
    }

    #[test]
    fn the_italian_tailoring_leaves_the_alphabet_alone() {
        assert!(sorts_before("it", "a", "z"));

        // Root order expands æ to "ae", so it stays at the front of the Italian alphabet; the
        // Danish tailoring above is what moves it after z.
        assert!(sorts_before("it", "æ", "z"));
    }

    /// The category `tag`'s rules of `kind` give the number written as `digits`, through the same
    /// `_unstable` constructor `Intl.PluralRules` selects with.
    ///
    /// The digits are asked for as a decimal string because that is what UTS 35 counts operands on:
    /// `PluralOperands` reads `v`, `w`, `f` and `t` off the positions of a `fixed_decimal::UnsignedDecimal`, and
    /// `"1.0"` and `1` are two different answers for a locale whose rule asks for a fraction digit.
    fn plural_category(tag: &str, kind: PluralRuleType, digits: &str) -> PluralCategory {
        let decimal = digits
            .parse::<UnsignedDecimal>()
            .expect("a decimal the operands are read off");

        plural_rules(tag, kind).rules().category_for(&decimal)
    }

    /// The rules of `tag`, with the ranges table 17.5.3 needs alongside them.
    fn plural_rules(tag: &str, kind: PluralRuleType) -> PluralRulesWithRanges<PluralRules> {
        let locale = tag.parse::<Locale>().expect("a built in locale parses");

        PluralRulesWithRanges::try_new_unstable(
            &IntlDataProvider,
            PluralRulesPreferences::from(&locale),
            PluralRulesOptions::from(kind),
        )
        .unwrap_or_else(|error| panic!("no plural rules for {tag}: {error}"))
    }

    #[test]
    fn every_built_in_locale_has_plural_rules_of_both_kinds() {
        for tag in BUILT_IN_LOCALES {
            for kind in [PluralRuleType::Cardinal, PluralRuleType::Ordinal] {
                let categories: Vec<_> = plural_rules(tag, kind).rules().categories().collect();

                // 17.3.2 step 4: the list is never empty and always ends with the fallback of
                // 17.5.1.
                assert!(!categories.is_empty(), "{tag} {kind:?} has a category");
                assert_eq!(categories.last(), Some(&PluralCategory::Other));
            }
        }
    }

    #[test]
    fn the_english_cardinal_rules_single_on_a_bare_one() {
        let cardinal = |digits| plural_category("en", PluralRuleType::Cardinal, digits);

        assert_eq!(cardinal("1"), PluralCategory::One);
        assert_eq!(cardinal("2"), PluralCategory::Other);
        // The `v = 0` half of the rule: a 1 shown with a fraction digit is not `one`.
        assert_eq!(cardinal("1.0"), PluralCategory::Other);

        let ordinal = |digits| plural_category("en", PluralRuleType::Ordinal, digits);
        assert_eq!(ordinal("1"), PluralCategory::One);
        assert_eq!(ordinal("2"), PluralCategory::Two);
        assert_eq!(ordinal("3"), PluralCategory::Few);
        assert_eq!(ordinal("11"), PluralCategory::Other);
    }

    #[test]
    fn the_italian_rules_answer_many_for_a_power_of_ten() {
        let cardinal = |digits| plural_category("it", PluralRuleType::Cardinal, digits);

        assert_eq!(cardinal("1000000"), PluralCategory::Many);
        assert_eq!(cardinal("999999"), PluralCategory::Other);
        assert_eq!(cardinal("1"), PluralCategory::One);
    }

    #[test]
    fn the_danish_ranges_table_is_baked_in() {
        // `PluralsRangesV1`, which is what 17.5.3's PluralRuleSelectRange reads and what the
        // `unstable` feature of `icu_plurals` names: both locales have an entry for the pair `other`
        // to `one` - the range `0 to 1` - and they disagree, Danish saying `one` and English `other`,
        // which 17.5.3's fallback of the end of the range would not have said.
        let da = plural_rules("da", PluralRuleType::Cardinal);
        assert_eq!(
            da.resolve_range(PluralCategory::Other, PluralCategory::One),
            PluralCategory::One
        );

        let en = plural_rules("en", PluralRuleType::Cardinal);
        assert_eq!(
            en.resolve_range(PluralCategory::Other, PluralCategory::One),
            PluralCategory::Other
        );
    }

    /// The three segmenters of `tag`, built exactly as `Intl.Segmenter` builds them: rule-based, and
    /// with the locale passed as the *content* locale of the word and sentence engines.
    ///
    /// That `unwrap` is the point of this helper as much as of the test that uses it: the engine's own
    /// cache unwraps the same way, and it is only allowed to because every marker of
    /// `icu/intl_markers.txt` for the segmenters is baked for every built-in locale.
    fn segmenters(tag: &str) -> (GraphemeClusterSegmenter, WordSegmenter, SentenceSegmenter) {
        let locale = tag.parse::<Locale>().expect("a built in locale parses");

        // The options are `non_exhaustive`, so the one field that matters is written onto the
        // default rather than named in a struct literal, exactly as the engine's cache does it.
        let mut word_options = WordBreakOptions::default();
        word_options.content_locale = Some(&locale.id);

        let mut sentence_options = SentenceBreakOptions::default();
        sentence_options.content_locale = Some(&locale.id);

        (
            GraphemeClusterSegmenter::try_new_unstable(&IntlDataProvider).unwrap(),
            WordSegmenter::try_new_for_non_complex_scripts_unstable(
                &IntlDataProvider,
                word_options,
            )
            .unwrap(),
            SentenceSegmenter::try_new_unstable(&IntlDataProvider, sentence_options).unwrap(),
        )
    }

    #[test]
    fn every_built_in_locale_has_all_three_segmenters() {
        for tag in BUILT_IN_LOCALES {
            let _ = segmenters(tag);
        }
    }

    #[test]
    fn the_grapheme_table_joins_what_a_cluster_needs() {
        // UAX #29's grapheme rules are the same in every locale, which is why the engine's grapheme
        // segmenter is built without one: an `e` followed by a combining acute, a Danish flag as a
        // pair of regional indicators and a family as three people joined by ZWJ are each one
        // cluster. The two accent and ZWJ code points are named by escape because a precomposed
        // letter would exercise no rule at all and an invisible joiner in a literal is not a test.
        // The first and last assertions are the same ones the engine's own `debug_assert` makes, so
        // this is also where a change of ICU4X's boundary convention would be caught.
        let combining_acute = '\u{301}';
        let zwj = '\u{200D}';

        let letter = format!("e{combining_acute}");
        let family = format!("👨{zwj}👩{zwj}👧");
        let text = format!("{letter}🇩🇰{family}");
        let clusters = [letter.as_str(), "🇩🇰", family.as_str()];

        for tag in BUILT_IN_LOCALES {
            let (graphemes, _, _) = segmenters(tag);

            let boundaries: Vec<usize> = graphemes.as_borrowed().segment_str(&text).collect();
            assert_eq!(boundaries.first(), Some(&0), "the run starts at 0 in {tag}");
            assert_eq!(boundaries.last(), Some(&text.len()), "and ends at the length in {tag}");

            let found: Vec<&str> = boundaries
                .windows(2)
                .map(|pair| &text[pair[0]..pair[1]])
                .collect();
            assert_eq!(found, clusters, "three clusters in {tag}");
        }
    }

    #[test]
    fn the_word_table_separates_the_words_from_the_punctuation() {
        // The word-like answer 19.7.1 step 10 reports as `isWordLike`: the two words are word-like,
        // whatever the comma and the space between them are grouped into. `SegmenterBreakWordOverrideV1`
        // is what makes this a locale question at all, and this is the load on it: the English rule
        // engine finds the same two words in Danish and Italian, because there is nothing in the
        // override tables to disagree about here.
        let text = "Hello, world";

        for tag in BUILT_IN_LOCALES {
            let (_, words, _) = segmenters(tag);

            let mut word_like = Vec::new();
            let mut start = 0;
            for (end, kind) in words.as_borrowed().segment_str(text).iter_with_word_type() {
                // The leading boundary is the start of the first segment and reports no type, so the
                // type of a segment travels with the boundary that *ends* it.
                if end > start && kind.is_word_like() {
                    word_like.push(&text[start..end]);
                }
                start = end;
            }

            assert_eq!(word_like, ["Hello", "world"], "word segmentation in {tag}");
        }
    }
}
