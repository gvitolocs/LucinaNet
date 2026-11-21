/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! When a navigation response is saved as a file instead of shown, and under which name.
//!
//! The rules follow Chromium (`net/http/http_content_disposition.cc` and the download file
//! name generation in `net/base/filename_util`), which is the behaviour users expect.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use http::{HeaderMap, header};
use mime::Mime;
use percent_encoding::percent_decode_str;
use servo_url::ServoUrl;

use crate::mime_classifier::{MediaType, MimeClassifier};

/// The longest file name written, in bytes: below the 255 of common file systems, with room
/// for ` (12).part`.
const MAX_FILE_NAME_BYTES: usize = 200;

/// An extension longer than this is more likely part of the name than a type, and is not kept
/// whole when a long name is shortened.
const MAX_KEPT_EXTENSION_BYTES: usize = 20;

/// Extensions that name one type together, so that a second `a.tar.gz` becomes `a (1).tar.gz`
/// rather than `a.tar (1).gz`.
const DOUBLE_EXTENSIONS: [&str; 4] = [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"];

const FALLBACK_FILE_NAME: &str = "download";

const JUNO_DOWNLOADS: &str = "/data/home/Downloads";

/// Whether a navigation response must be saved as a file instead of shown: the link had a
/// `download` attribute, the response is a `Content-Disposition` attachment, or its type is not
/// one Servo can show as a document. A response without a type is sniffed by the document
/// loader as before, so it is not a download.
pub fn navigation_response_is_download(
    headers: Option<&HeaderMap>,
    content_type: Option<&Mime>,
    download_attribute: Option<&str>,
) -> bool {
    if download_attribute.is_some() {
        return true;
    }
    if content_disposition(headers).is_some_and(|value| content_disposition_is_attachment(&value)) {
        return true;
    }
    let Some(content_type) = content_type else {
        return false;
    };
    // Parameters such as `charset` do not change whether a type can be shown.
    let Ok(essence) = content_type.essence_str().parse::<Mime>() else {
        return true;
    };
    if essence.type_() == mime::TEXT {
        return false;
    }
    !matches!(
        MimeClassifier::get_media_type(&essence),
        Some(media_type) if media_type != MediaType::Font
    )
}

/// True if the disposition type of a `Content-Disposition` value makes it an attachment.
/// As in Chromium, and as RFC 6266 section 4.2 asks, an unknown type counts as `attachment`;
/// a value that starts with a parameter has no type and does not.
pub fn content_disposition_is_attachment(value: &str) -> bool {
    let disposition_type = value.split(';').next().unwrap_or_default().trim();
    !disposition_type.is_empty()
        && !disposition_type.contains('=')
        && !disposition_type.eq_ignore_ascii_case("inline")
}

/// The file name of a `Content-Disposition` value: `filename*` (RFC 8187) wherever it is, then
/// `filename`. None if neither gives a non-empty name.
pub fn content_disposition_file_name(value: &str) -> Option<String> {
    let parameters = split_parameters(value);
    let extended = parameters
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("filename*"))
        .find_map(|(_, value)| decode_extended_value(value));
    extended.or_else(|| {
        parameters
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("filename"))
            .find_map(|(_, value)| {
                let value = unquote(value);
                // Many servers percent-encode a plain `filename`; Chromium decodes it when that
                // gives valid UTF-8, and keeps the value as sent otherwise.
                let decoded = match percent_decode_str(&value).decode_utf8() {
                    Ok(decoded) => decoded.into_owned(),
                    Err(_) => value,
                };
                (!decoded.is_empty()).then_some(decoded)
            })
    })
}

/// The name the response is saved under, already sanitised: the `Content-Disposition` file
/// name, else the `download` attribute, else the last segment of the URL's path, else
/// `download`.
pub fn download_file_name(
    headers: Option<&HeaderMap>,
    download_attribute: Option<&str>,
    url: &ServoUrl,
) -> String {
    let from_header = content_disposition(headers)
        .as_deref()
        .and_then(content_disposition_file_name);
    let from_attribute = download_attribute
        .map(str::trim)
        .filter(|attribute| !attribute.is_empty())
        .map(str::to_owned);
    let from_url = (url.scheme() != "data")
        .then(|| url.as_url().path_segments())
        .flatten()
        .and_then(|segments| segments.rev().find(|segment| !segment.is_empty()))
        .map(|segment| percent_decode_str(segment).decode_utf8_lossy().into_owned());

    [from_header, from_attribute, from_url]
        .into_iter()
        .flatten()
        .map(|candidate| sanitize_file_name(&candidate))
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| FALLBACK_FILE_NAME.to_owned())
}

/// Makes a name from the network safe as one file name in the downloads directory: no path
/// separators (so `../` cannot leave it), no control characters, no leading dots (no hidden
/// files, no `..`), and at most [`MAX_FILE_NAME_BYTES`] bytes with the extension kept.
/// Returns an empty string when nothing is left.
pub fn sanitize_file_name(name: &str) -> String {
    let replaced: String = name
        .chars()
        .filter(|character| !character.is_control())
        .map(|character| match character {
            '/' | '\\' => '_',
            other => other,
        })
        .collect();
    let name = replaced
        .trim()
        .trim_start_matches(|character: char| character == '.' || character.is_whitespace());
    if name.len() <= MAX_FILE_NAME_BYTES {
        return name.to_owned();
    }

    let extension = name
        .rfind('.')
        .filter(|&dot| dot > 0 && name.len() - dot <= MAX_KEPT_EXTENSION_BYTES)
        .map(|dot| &name[dot..])
        .unwrap_or_default();
    let stem = &name[..name.len() - extension.len()];
    let stem = truncate_to_char_boundary(stem, MAX_FILE_NAME_BYTES - extension.len())
        .trim_end_matches(|character: char| character == '.' || character.is_whitespace());
    format!("{stem}{extension}")
}

/// The downloads directory: `$LUCINANET_DOWNLOADS` if set, else `/data/home/Downloads` where it
/// exists (Juno), else `$HOME/Downloads`; the temporary directory if there is no home at all.
pub fn download_directory_from(
    downloads_env: Option<PathBuf>,
    home: Option<PathBuf>,
    juno_downloads_exists: bool,
) -> PathBuf {
    if let Some(directory) = downloads_env.filter(|path| !path.as_os_str().is_empty()) {
        return directory;
    }
    if juno_downloads_exists {
        return PathBuf::from(JUNO_DOWNLOADS);
    }
    match home.filter(|path| !path.as_os_str().is_empty()) {
        Some(home) => home.join("Downloads"),
        None => std::env::temp_dir(),
    }
}

/// [`download_directory_from`] for this process's environment.
pub fn download_directory() -> PathBuf {
    download_directory_from(
        std::env::var_os("LUCINANET_DOWNLOADS").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        Path::new(JUNO_DOWNLOADS).is_dir(),
    )
}

/// Creates `<dir>/<name>.part`, or `<stem> (1)<ext>.part`, `<stem> (2)<ext>.part` ... when the
/// name or its `.part` is taken, and creates `dir` first if needed. The `.part` file is made
/// with `create_new`, so two downloads running at once never share a file. Returns the final
/// path, the `.part` path and the open `.part` file.
pub fn reserve_download_file(dir: &Path, name: &str) -> std::io::Result<(PathBuf, PathBuf, File)> {
    std::fs::create_dir_all(dir)?;
    let (stem, extension) = split_extension(name);
    for number in 0..10_000 {
        let candidate = match number {
            0 => name.to_owned(),
            number => format!("{stem} ({number}){extension}"),
        };
        let final_path = dir.join(&candidate);
        if final_path.exists() {
            continue;
        }
        let part_path = dir.join(format!("{candidate}.part"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part_path)
        {
            Ok(file) => return Ok((final_path, part_path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        ErrorKind::AlreadyExists,
        format!("every name like {name} is taken"),
    ))
}

fn content_disposition(headers: Option<&HeaderMap>) -> Option<String> {
    let value = headers?.get(header::CONTENT_DISPOSITION)?;
    // Servers send UTF-8 in plain `filename`s although HTTP headers are not meant to carry it.
    Some(match value.to_str() {
        Ok(value) => value.to_owned(),
        Err(_) => String::from_utf8_lossy(value.as_bytes()).into_owned(),
    })
}

/// The `name=value` parameters after the disposition type. A `;` inside a quoted string does
/// not end the parameter.
fn split_parameters(value: &str) -> Vec<(String, String)> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            escaped = false;
        } else if in_quotes && character == '\\' {
            escaped = true;
        } else if character == '"' {
            in_quotes = !in_quotes;
        } else if character == ';' && !in_quotes {
            pieces.push(std::mem::take(&mut current));
            continue;
        }
        current.push(character);
    }
    pieces.push(current);

    pieces
        .into_iter()
        .skip(1)
        .filter_map(|piece| {
            let (name, value) = piece.split_once('=')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

/// A token, or the contents of a quoted string with its `\` escapes undone.
fn unquote(value: &str) -> String {
    let Some(quoted) = value.strip_prefix('"') else {
        return value.to_owned();
    };
    let mut unquoted = String::new();
    let mut characters = quoted.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => unquoted.extend(characters.next()),
            '"' => break,
            other => unquoted.push(other),
        }
    }
    unquoted
}

/// `charset'language'percent-encoded` (RFC 8187); UTF-8 and ISO-8859-1 are the charsets the
/// RFC requires, and the only ones decoded.
fn decode_extended_value(value: &str) -> Option<String> {
    let value = unquote(value);
    let mut parts = value.splitn(3, '\'');
    let charset = parts.next()?;
    let _language = parts.next()?;
    let bytes = percent_decode_str(parts.next()?);
    let decoded: String = if charset.eq_ignore_ascii_case("utf-8") {
        bytes.decode_utf8().ok()?.into_owned()
    } else if charset.eq_ignore_ascii_case("iso-8859-1") {
        bytes.map(char::from).collect()
    } else {
        return None;
    };
    (!decoded.is_empty()).then_some(decoded)
}

fn truncate_to_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Splits `name` into the part before the extension and the extension with its dot. A name
/// whose only dot is its first character has no extension.
fn split_extension(name: &str) -> (&str, &str) {
    let lowercase = name.to_ascii_lowercase();
    if let Some(double) = DOUBLE_EXTENSIONS
        .iter()
        .find(|extension| lowercase.ends_with(*extension) && lowercase.len() > extension.len())
    {
        return name.split_at(name.len() - double.len());
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    }
}
