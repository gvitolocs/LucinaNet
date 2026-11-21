/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use app_units::Au;
use fonts::platform::font::PlatformFont;
use fonts::{
    Font, FontData, FontDescriptor, FontIdentifier, FontTemplate, FontTemplateRef,
    PlatformFontMethods, ShapingFlags, ShapingOptions,
};
use icu_locale_core::subtags::Language;
use servo_url::ServoUrl;
use style::Zero;
use style::computed_values::font_optical_sizing::T as FontOpticalSizing;
use style::computed_values::font_variant_position::T as FontVariantPosition;
use style::properties::longhands::font_variant_caps::computed_value::T as FontVariantCaps;
use style::values::computed::{
    FontFeatureSettings, FontStyle, FontSynthesis, FontVariantEastAsian, FontVariantLigatures,
    FontVariantNumeric, FontWeight, FontWidth,
};
use unicode_script::Script;

fn make_font(path: PathBuf) -> Font {
    let mut bytes = Vec::new();
    File::open(path.clone())
        .expect("Couldn't open font file!")
        .read_to_end(&mut bytes)
        .unwrap();
    let data = FontData::from_bytes(&bytes);

    let identifier = FontIdentifier::Web(ServoUrl::from_file_path(path).unwrap());
    let platform_font =
        PlatformFont::new_from_data(identifier.clone(), &data, None, false).unwrap();

    let template = FontTemplate::new(identifier, platform_font.descriptor(), None);
    let descriptor = FontDescriptor {
        weight: FontWeight::normal(),
        width: FontWidth::hundred(),
        style: FontStyle::normal(),
        variant: FontVariantCaps::Normal,
        pt_size: Au::from_px(24),
        variation_settings: vec![],
        synthesis_weight: FontSynthesis::Auto,
        optical_sizing: FontOpticalSizing::Auto,
    };
    Font::new(FontTemplateRef::new(template), descriptor, Some(data), None).unwrap()
}

#[test]
fn test_font_can_do_fast_shaping() {
    let dejavu_sans = make_font(
        [
            env!("CARGO_MANIFEST_DIR"),
            "tests",
            "support",
            "dejavu-fonts-ttf-2.37",
            "ttf",
            "DejaVuSans.ttf",
        ]
        .iter()
        .collect(),
    );

    let dejavu_sans_fast_shapeable = make_font(
        [
            env!("CARGO_MANIFEST_DIR"),
            "tests",
            "support",
            "dejavu-fonts-ttf-2.37",
            "ttf",
            "DejaVuSansNoGSUBNoGPOS.ttf",
        ]
        .iter()
        .collect(),
    );

    // Fast shaping requires a font with a kern table and no GPOS or GSUB tables.
    let shaping_options = ShapingOptions {
        letter_spacing: Au::zero(),
        word_spacing: Au::zero(),
        script: Script::Latin,
        language: Language::UNKNOWN,
        flags: ShapingFlags::empty(),
        ligatures: FontVariantLigatures::NORMAL,
        numeric: FontVariantNumeric::NORMAL,
        east_asian: FontVariantEastAsian::NORMAL,
        feature_settings: FontFeatureSettings::normal(),
        position: FontVariantPosition::Normal,
        alternates: Default::default(),
    };
    assert!(!dejavu_sans.can_do_fast_shaping("WAVE", &shaping_options));
    assert!(dejavu_sans_fast_shapeable.can_do_fast_shaping("WAVE", &shaping_options));

    // Non-Latin script should never have fast shaping.
    let shaping_options = ShapingOptions {
        letter_spacing: Au::zero(),
        word_spacing: Au::zero(),
        script: Script::Cherokee,
        language: Language::UNKNOWN,
        flags: ShapingFlags::empty(),
        ligatures: FontVariantLigatures::NORMAL,
        numeric: FontVariantNumeric::NORMAL,
        east_asian: FontVariantEastAsian::NORMAL,
        feature_settings: FontFeatureSettings::normal(),
        position: FontVariantPosition::Normal,
        alternates: Default::default(),
    };
    assert!(!dejavu_sans.can_do_fast_shaping("WAVE", &shaping_options));
    assert!(!dejavu_sans_fast_shapeable.can_do_fast_shaping("WAVE", &shaping_options));

    // Right-to-left text should never use fast shaping.
    let shaping_options = ShapingOptions {
        letter_spacing: Au::zero(),
        word_spacing: Au::zero(),
        script: Script::Latin,
        language: Language::UNKNOWN,
        flags: ShapingFlags::RTL_FLAG,
        ligatures: FontVariantLigatures::NORMAL,
        numeric: FontVariantNumeric::NORMAL,
        east_asian: FontVariantEastAsian::NORMAL,
        feature_settings: FontFeatureSettings::normal(),
        position: FontVariantPosition::Normal,
        alternates: Default::default(),
    };
    assert!(!dejavu_sans.can_do_fast_shaping("WAVE", &shaping_options));
    assert!(!dejavu_sans_fast_shapeable.can_do_fast_shaping("WAVE", &shaping_options));
}

fn dejavu_sans_path() -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "support",
        "dejavu-fonts-ttf-2.37",
        "ttf",
        "DejaVuSans.ttf",
    ]
    .iter()
    .collect()
}

fn latin_shaping_options() -> ShapingOptions {
    ShapingOptions {
        letter_spacing: Au::zero(),
        word_spacing: Au::zero(),
        script: Script::Latin,
        language: Language::UNKNOWN,
        flags: ShapingFlags::empty(),
        ligatures: FontVariantLigatures::NORMAL,
        numeric: FontVariantNumeric::NORMAL,
        east_asian: FontVariantEastAsian::NORMAL,
        feature_settings: FontFeatureSettings::normal(),
        position: FontVariantPosition::Normal,
        alternates: Default::default(),
    }
}

fn advances(font: &Font, text: &str) -> Vec<Au> {
    let shaped = font.shape_text(text, &latin_shaping_options());
    shaped.glyphs().map(|glyph| glyph.advance()).collect()
}

/// Makes a font, makes it build its shaper, and moves it out of this frame.
fn font_with_shaper() -> Font {
    let font = make_font(dejavu_sans_path());
    let _ = font.baseline();
    assert!(!font.can_do_fast_shaping("WAVE", &latin_shaping_options()));
    let _ = advances(&font, "WAVE");
    font
}

/// The shaper must not keep the address of the `Font` it was built for: a `Font` is a value
/// that can move after its shaper exists.
#[test]
fn test_font_shapes_after_it_moved() {
    let moved = Box::new(font_with_shaper());
    let fresh = make_font(dejavu_sans_path());

    let baselines = |font: &Font| {
        font.baseline().map(|baseline| {
            (
                baseline.ideographic_baseline,
                baseline.alphabetic_baseline,
                baseline.hanging_baseline,
            )
        })
    };
    assert_eq!(baselines(&moved), baselines(&fresh));
    // Not shaped before the move, so this is not served from the shaped text cache.
    assert_eq!(advances(&moved, "AVATAR"), advances(&fresh, "AVATAR"));
    assert!(!advances(&moved, "AVATAR").is_empty());
}
