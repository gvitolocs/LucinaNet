/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: the system font list without fontconfig. Juno is one static binary with no fontconfig
//! and no font configuration, so the list is what a directory holds. `LUCINANET_FONT_DIRS` names the
//! directories (`:` separated); families and styles come from each font's `name` and `OS/2` tables.

use std::path::Path;
use std::sync::LazyLock;

use fonts_traits::{FontTemplate, FontTemplateDescriptor, LocalFontIdentifier};
use icu_locale_core::subtags::language;
use servo_base::text::{UnicodeBlock, UnicodeBlockMethod};
use style::Atom;
use style::values::computed::font::GenericFontFamily;
use style::values::computed::{FontStyle, FontWeight, FontWidth};
use unicode_script::Script;

use super::font_file_bytes;
use crate::platform::add_noto_fallback_families;
use crate::{
    EmojiPresentationPreference, FallbackFontSelectionOptions, FontIdentifier,
    LowercaseFontFamilyName,
};

const DEFAULT_DIRS: &str =
    "/usr/share/fonts/dejavu:/usr/share/fonts/noto:/usr/share/fonts/truetype/dejavu";

struct Face {
    family: String,
    path: String,
    index: u16,
    weight: f32,
    width: f32,
    italic: bool,
}

static FACES: LazyLock<Vec<Face>> = LazyLock::new(|| {
    let started = std::time::Instant::now();
    let dirs = std::env::var("LUCINANET_FONT_DIRS").unwrap_or_else(|_| DEFAULT_DIRS.to_owned());
    let mut faces = Vec::new();
    for dir in dirs.split(':') {
        scan_directory(Path::new(dir), &mut faces);
    }
    if std::env::var_os("LUCINANET_TIMING").is_some() {
        eprintln!(
            "phase font list: {} faces in {} ms",
            faces.len(),
            started.elapsed().as_millis()
        );
    }
    log::debug!("{} font faces in {dirs}", faces.len());
    faces
});

fn scan_directory(dir: &Path, faces: &mut Vec<Face>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            scan_directory(&path, faces);
            continue;
        }
        let is_font = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["ttf", "otf", "ttc", "otc"]
                .iter()
                .any(|known| e.eq_ignore_ascii_case(known))
        });
        let (true, Some(name)) = (is_font, path.to_str()) else {
            continue;
        };
        // Mapped: the list needs only each face's tables, and CJK collections are 20 MB on a slow disk.
        let Ok(data) = font_file_bytes(&path) else {
            continue;
        };
        read_faces(&data, name, faces);
    }
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn read_faces(data: &[u8], path: &str, faces: &mut Vec<Face>) {
    let offsets: Vec<usize> = if data.get(..4) == Some(b"ttcf") {
        let count = be32(data, 8).unwrap_or(0) as usize;
        (0..count)
            .filter_map(|i| be32(data, 12 + i * 4).map(|offset| offset as usize))
            .collect()
    } else {
        vec![0]
    };
    for (index, offset) in offsets.into_iter().enumerate() {
        if let Some(face) = read_face(data, offset, path, index as u16) {
            faces.push(face);
        }
    }
}

fn table<'a>(data: &'a [u8], offset: usize, tag: &[u8; 4]) -> Option<&'a [u8]> {
    let count = be16(data, offset + 4)? as usize;
    for i in 0..count {
        let record = offset + 12 + i * 16;
        if data.get(record..record + 4)? == tag {
            let start = be32(data, record + 8)? as usize;
            let length = be32(data, record + 12)? as usize;
            return data.get(start..start.checked_add(length)?);
        }
    }
    None
}

/// The name with this id: Windows Unicode English first, as FreeType and fontconfig prefer it.
fn name(names: &[u8], id: u16) -> Option<String> {
    let count = be16(names, 2)? as usize;
    let strings = be16(names, 4)? as usize;
    let mut best: Option<(u32, String)> = None;
    for i in 0..count {
        let record = 6 + i * 12;
        if be16(names, record + 6)? != id {
            continue;
        }
        let platform = be16(names, record)?;
        let language = be16(names, record + 4)?;
        let length = be16(names, record + 8)? as usize;
        let start = strings + be16(names, record + 10)? as usize;
        let Some(bytes) = names.get(start..start + length) else {
            continue;
        };
        let (rank, text) = match platform {
            3 => {
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                let rank = if language == 0x409 { 3 } else { 1 };
                (rank, String::from_utf16_lossy(&units))
            },
            1 if language == 0 => (2, bytes.iter().map(|&b| b as char).collect()),
            _ => continue,
        };
        if best.as_ref().is_none_or(|(old, _)| rank > *old) {
            best = Some((rank, text));
        }
    }
    best.map(|(_, text)| text)
}

fn read_face(data: &[u8], offset: usize, path: &str, index: u16) -> Option<Face> {
    let names = table(data, offset, b"name")?;
    // Typographic family (16) groups a family's weights; the plain family (1) splits them.
    let family = name(names, 16).or_else(|| name(names, 1))?;
    let (weight, width, italic) = match table(data, offset, b"OS/2") {
        Some(os2) => (
            be16(os2, 4).unwrap_or(400) as f32,
            be16(os2, 6).unwrap_or(5),
            be16(os2, 62).unwrap_or(0) & 1 != 0,
        ),
        None => (400.0, 5, false),
    };
    let width = match width {
        1 => 50.0,
        2 => 62.5,
        3 => 75.0,
        4 => 87.5,
        6 => 112.5,
        7 => 125.0,
        8 => 150.0,
        9 => 200.0,
        _ => 100.0,
    };
    Some(Face {
        family,
        path: path.to_owned(),
        index,
        weight,
        width,
        italic,
    })
}

pub(crate) fn for_each_available_family<F>(mut callback: F)
where
    F: FnMut(&str),
{
    let mut last = None;
    for face in FACES.iter() {
        if last != Some(face.family.as_str()) {
            callback(&face.family);
            last = Some(face.family.as_str());
        }
    }
}

pub(crate) fn for_each_variation<F>(family_name: &str, mut callback: F)
where
    F: FnMut(FontTemplate),
{
    for face in FACES
        .iter()
        .filter(|face| face.family.eq_ignore_ascii_case(family_name))
    {
        let style = if face.italic {
            FontStyle::ITALIC
        } else {
            FontStyle::NORMAL
        };
        let descriptor = FontTemplateDescriptor::new(
            FontWeight::from_float(face.weight),
            FontWidth::from_percentage(face.width / 100.0),
            style,
        );
        let identifier = LocalFontIdentifier {
            path: Atom::from(face.path.as_str()),
            face_index: face.index,
            named_instance_index: 0,
        };
        callback(FontTemplate::new(
            FontIdentifier::Local(identifier),
            descriptor,
            None,
        ));
    }
}

// Based on gfxPlatformGtk::GetCommonFallbackFonts() in Gecko
pub fn fallback_font_families(options: FallbackFontSelectionOptions) -> Vec<&'static str> {
    let mut families = Vec::new();
    if options.presentation_preference == EmojiPresentationPreference::Emoji {
        families.push("Noto Color Emoji");
    }

    add_noto_fallback_families(options.clone(), &mut families);

    if let Some(block) = options.character.block() {
        match block {
            _ if options.language == language!("ja") => {
                families.push("TakaoPGothic");
            },
            _ if matches!(
                Script::from(options.character),
                Script::Bopomofo | Script::Han
            ) && options.language != language!("ja") =>
            {
                families.push("WenQuanYi Micro Hei");
            },
            UnicodeBlock::HalfwidthandFullwidthForms
            | UnicodeBlock::EnclosedIdeographicSupplement => families.push("WenQuanYi Micro Hei"),
            UnicodeBlock::Hiragana
            | UnicodeBlock::Katakana
            | UnicodeBlock::KatakanaPhoneticExtensions => {
                families.push("TakaoPGothic");
            },
            _ => {},
        }
    }

    families.push("DejaVu Serif");
    families.push("FreeSerif");
    families.push("DejaVu Sans");
    families.push("DejaVu Sans Mono");
    families.push("FreeSans");
    // LucinaNet: CJK punctuation is Script::Common and in no block handled above.
    families.push("Noto Sans CJK JP");
    families.push("Noto Sans CJK SC");
    families.push("Noto Sans CJK KR");
    families.push("Symbola");
    families.push("Droid Sans Fallback");

    families
}

pub(crate) fn default_system_generic_font_family(
    generic: GenericFontFamily,
) -> LowercaseFontFamilyName {
    let wanted = match generic {
        GenericFontFamily::None | GenericFontFamily::Serif => "DejaVu Serif",
        GenericFontFamily::SansSerif | GenericFontFamily::SystemUi => "DejaVu Sans",
        GenericFontFamily::Monospace => "DejaVu Sans Mono",
        GenericFontFamily::Cursive | GenericFontFamily::Fantasy => "DejaVu Serif",
    };
    // Juno's boot image has the Sans and Mono faces only.
    let have = |family: &str| FACES.iter().any(|face| face.family == family);
    if have(wanted) {
        wanted.into()
    } else {
        "DejaVu Sans".into()
    }
}
