// @generated
include!("normalizer_nfd_tables_v1.rs.data");
include!("short_day_relative_v1.rs.data");
include!("long_second_relative_v1.rs.data");
include!("datetime_patterns_date_gregorian_v1.rs.data");
include!("datetime_patterns_time_v1.rs.data");
include!("short_quarter_relative_v1.rs.data");
include!("datetime_patterns_range_time_v1.rs.data");
include!("datetime_names_month_gregorian_v1.rs.data");
include!("short_week_relative_v1.rs.data");
include!("collation_reordering_v1.rs.data");
include!("datetime_names_dayperiod_v1.rs.data");
include!("segmenter_break_sentence_v1.rs.data");
include!("collation_diacritics_v1.rs.data");
include!("datetime_names_weekday_v1.rs.data");
include!("plurals_ordinal_v1.rs.data");
include!("decimal_symbols_v1.rs.data");
include!("long_minute_relative_v1.rs.data");
include!("currency_patterns_data_v1.rs.data");
include!("long_quarter_relative_v1.rs.data");
include!("short_month_relative_v1.rs.data");
include!("currency_extended_data_v1.rs.data");
include!("locale_likely_subtags_language_v1.rs.data");
include!("locale_parents_v1.rs.data");
include!("timezone_names_locations_root_v1.rs.data");
include!("narrow_hour_relative_v1.rs.data");
include!("timezone_names_specific_short_v1.rs.data");
include!("datetime_patterns_glue_v1.rs.data");
include!("narrow_week_relative_v1.rs.data");
include!("locale_aliases_v1.rs.data");
include!("timezone_names_cities_root_v1.rs.data");
include!("narrow_second_relative_v1.rs.data");
include!("long_week_relative_v1.rs.data");
include!("collation_jamo_v1.rs.data");
include!("segmenter_break_grapheme_cluster_v1.rs.data");
include!("short_minute_relative_v1.rs.data");
include!("collation_metadata_v1.rs.data");
include!("language_display_names_v1.rs.data");
include!("plurals_cardinal_v1.rs.data");
include!("short_hour_relative_v1.rs.data");
include!("datetime_patterns_range_date_gregorian_v1.rs.data");
include!("long_month_relative_v1.rs.data");
include!("long_year_relative_v1.rs.data");
include!("list_or_v1.rs.data");
include!("timezone_names_generic_short_v1.rs.data");
include!("region_display_names_v1.rs.data");
include!("list_and_v1.rs.data");
include!("datetime_patterns_range_glue_v1.rs.data");
include!("timezone_names_specific_long_v1.rs.data");
include!("plurals_ranges_v1.rs.data");
include!("collation_tailoring_v1.rs.data");
include!("decimal_digits_v1.rs.data");
include!("currency_essentials_v1.rs.data");
include!("short_year_relative_v1.rs.data");
include!("long_day_relative_v1.rs.data");
include!("timezone_names_essentials_v1.rs.data");
include!("long_hour_relative_v1.rs.data");
include!("timezone_names_cities_override_v1.rs.data");
include!("timezone_periods_v1.rs.data");
include!("short_second_relative_v1.rs.data");
include!("timezone_names_generic_long_v1.rs.data");
include!("list_unit_v1.rs.data");
include!("narrow_month_relative_v1.rs.data");
include!("narrow_day_relative_v1.rs.data");
include!("collation_special_primaries_v1.rs.data");
include!("normalizer_nfd_data_v1.rs.data");
include!("timezone_identifiers_iana_core_v1.rs.data");
include!("decimal_compact_short_v1.rs.data");
include!("segmenter_break_word_v1.rs.data");
include!("narrow_quarter_relative_v1.rs.data");
include!("timezone_names_standard_long_v1.rs.data");
include!("script_display_names_v1.rs.data");
include!("currency_symbols_v1.rs.data");
include!("locale_likely_subtags_extended_v1.rs.data");
include!("narrow_minute_relative_v1.rs.data");
include!("decimal_compact_long_v1.rs.data");
include!("locale_script_direction_v1.rs.data");
include!("datetime_names_year_gregorian_v1.rs.data");
include!("segmenter_break_word_override_v1.rs.data");
include!("segmenter_break_sentence_override_v1.rs.data");
include!("collation_root_v1.rs.data");
include!("currency_fractions_v1.rs.data");
include!("timezone_names_locations_override_v1.rs.data");
include!("narrow_year_relative_v1.rs.data");
include!("locale_likely_subtags_script_region_v1.rs.data");
/// Marks a type as a data provider. You can then use macros like
/// `impl_core_helloworld_v1` to add implementations.
///
/// ```ignore
/// struct MyProvider;
/// const _: () = {
///     include!("path/to/generated/macros.rs");
///     make_provider!(MyProvider);
///     impl_core_helloworld_v1!(MyProvider);
/// }
/// ```
#[doc(hidden)]
#[macro_export]
macro_rules! __make_provider {
    ($ name : ty) => {
        #[clippy::msrv = "1.88"]
        impl $name {
            #[allow(dead_code)]
            pub(crate) const MUST_USE_MAKE_PROVIDER_MACRO: () = ();
        }
        icu_provider::marker::impl_data_provider_never_marker!($name);
    };
}
#[doc(inline)]
pub use __make_provider as make_provider;
/// This macro requires the following crates:
/// * `alloc`
/// * `icu_collator`
/// * `icu_collections`
/// * `icu_datetime`
/// * `icu_decimal`
/// * `icu_experimental`
/// * `icu_list`
/// * `icu_locale`
/// * `icu_locale_core`
/// * `icu_locale_fallback`
/// * `icu_locale_fallback/compiled_data`
/// * `icu_normalizer`
/// * `icu_pattern`
/// * `icu_plurals`
/// * `icu_provider`
/// * `icu_provider/baked`
/// * `icu_segmenter`
/// * `icu_time`
/// * `zerotrie`
/// * `zerovec`
#[allow(unused_macros)]
macro_rules! impl_data_provider {
    ($ provider : ty) => {
        make_provider!($provider);
        impl_normalizer_nfd_tables_v1!($provider);
        impl_short_day_relative_v1!($provider);
        impl_long_second_relative_v1!($provider);
        impl_datetime_patterns_date_gregorian_v1!($provider);
        impl_datetime_patterns_time_v1!($provider);
        impl_short_quarter_relative_v1!($provider);
        impl_datetime_patterns_range_time_v1!($provider);
        impl_datetime_names_month_gregorian_v1!($provider);
        impl_short_week_relative_v1!($provider);
        impl_collation_reordering_v1!($provider);
        impl_datetime_names_dayperiod_v1!($provider);
        impl_segmenter_break_sentence_v1!($provider);
        impl_collation_diacritics_v1!($provider);
        impl_datetime_names_weekday_v1!($provider);
        impl_plurals_ordinal_v1!($provider);
        impl_decimal_symbols_v1!($provider);
        impl_long_minute_relative_v1!($provider);
        impl_currency_patterns_data_v1!($provider);
        impl_long_quarter_relative_v1!($provider);
        impl_short_month_relative_v1!($provider);
        impl_currency_extended_data_v1!($provider);
        impl_locale_likely_subtags_language_v1!($provider);
        impl_locale_parents_v1!($provider);
        impl_timezone_names_locations_root_v1!($provider);
        impl_narrow_hour_relative_v1!($provider);
        impl_timezone_names_specific_short_v1!($provider);
        impl_datetime_patterns_glue_v1!($provider);
        impl_narrow_week_relative_v1!($provider);
        impl_locale_aliases_v1!($provider);
        impl_timezone_names_cities_root_v1!($provider);
        impl_narrow_second_relative_v1!($provider);
        impl_long_week_relative_v1!($provider);
        impl_collation_jamo_v1!($provider);
        impl_segmenter_break_grapheme_cluster_v1!($provider);
        impl_short_minute_relative_v1!($provider);
        impl_collation_metadata_v1!($provider);
        impl_language_display_names_v1!($provider);
        impl_plurals_cardinal_v1!($provider);
        impl_short_hour_relative_v1!($provider);
        impl_datetime_patterns_range_date_gregorian_v1!($provider);
        impl_long_month_relative_v1!($provider);
        impl_long_year_relative_v1!($provider);
        impl_list_or_v1!($provider);
        impl_timezone_names_generic_short_v1!($provider);
        impl_region_display_names_v1!($provider);
        impl_list_and_v1!($provider);
        impl_datetime_patterns_range_glue_v1!($provider);
        impl_timezone_names_specific_long_v1!($provider);
        impl_plurals_ranges_v1!($provider);
        impl_collation_tailoring_v1!($provider);
        impl_decimal_digits_v1!($provider);
        impl_currency_essentials_v1!($provider);
        impl_short_year_relative_v1!($provider);
        impl_long_day_relative_v1!($provider);
        impl_timezone_names_essentials_v1!($provider);
        impl_long_hour_relative_v1!($provider);
        impl_timezone_names_cities_override_v1!($provider);
        impl_timezone_periods_v1!($provider);
        impl_short_second_relative_v1!($provider);
        impl_timezone_names_generic_long_v1!($provider);
        impl_list_unit_v1!($provider);
        impl_narrow_month_relative_v1!($provider);
        impl_narrow_day_relative_v1!($provider);
        impl_collation_special_primaries_v1!($provider);
        impl_normalizer_nfd_data_v1!($provider);
        impl_timezone_identifiers_iana_core_v1!($provider);
        impl_decimal_compact_short_v1!($provider);
        impl_segmenter_break_word_v1!($provider);
        impl_narrow_quarter_relative_v1!($provider);
        impl_timezone_names_standard_long_v1!($provider);
        impl_script_display_names_v1!($provider);
        impl_currency_symbols_v1!($provider);
        impl_locale_likely_subtags_extended_v1!($provider);
        impl_narrow_minute_relative_v1!($provider);
        impl_decimal_compact_long_v1!($provider);
        impl_locale_script_direction_v1!($provider);
        impl_datetime_names_year_gregorian_v1!($provider);
        impl_segmenter_break_word_override_v1!($provider);
        impl_segmenter_break_sentence_override_v1!($provider);
        impl_collation_root_v1!($provider);
        impl_currency_fractions_v1!($provider);
        impl_timezone_names_locations_override_v1!($provider);
        impl_narrow_year_relative_v1!($provider);
        impl_locale_likely_subtags_script_region_v1!($provider);
    };
}
