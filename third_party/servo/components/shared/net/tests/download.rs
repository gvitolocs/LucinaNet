/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::fs;
use std::path::{Path, PathBuf};

use http::HeaderMap;
use http::header::{CONTENT_DISPOSITION, HeaderValue};
use mime::Mime;
use net_traits::download::{
    content_disposition_file_name, content_disposition_is_attachment, download_directory_from,
    download_file_name, navigation_response_is_download, reserve_download_file, sanitize_file_name,
};
use servo_url::ServoUrl;

fn disposition(value: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_DISPOSITION, HeaderValue::from_str(value).unwrap());
    headers
}

fn is_download(content_type: Option<&str>) -> bool {
    let mime: Option<Mime> = content_type.map(|content_type| content_type.parse().unwrap());
    navigation_response_is_download(None, mime.as_ref(), None)
}

fn url(text: &str) -> ServoUrl {
    ServoUrl::parse(text).unwrap()
}

#[test]
fn types_servo_shows_are_not_downloads() {
    for shown in [
        "text/html",
        "text/html; charset=utf-8",
        "application/xhtml+xml",
        "application/xml",
        "text/xml",
        "image/svg+xml",
        "text/plain",
        "text/plain; charset=utf-8",
        "text/csv",
        "text/css",
        "text/javascript",
        "application/json",
        "image/png",
        "image/webp",
        "video/mp4",
        "audio/mpeg",
    ] {
        assert!(!is_download(Some(shown)), "{shown} should be shown");
    }
}

#[test]
fn other_types_are_downloads() {
    for downloaded in [
        "application/pdf",
        "application/zip",
        "application/octet-stream",
        "application/x-msdownload",
        "font/woff2",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ] {
        assert!(
            is_download(Some(downloaded)),
            "{downloaded} should be saved"
        );
    }
}

#[test]
fn a_response_without_a_type_is_sniffed_not_downloaded() {
    assert!(!is_download(None));
}

#[test]
fn the_download_attribute_makes_any_response_a_download() {
    let html: Mime = "text/html".parse().unwrap();
    assert!(navigation_response_is_download(None, Some(&html), Some("")));
    assert!(navigation_response_is_download(
        None,
        Some(&html),
        Some("x.bin")
    ));
    assert!(navigation_response_is_download(None, None, Some("x.bin")));
}

#[test]
fn content_disposition_attachment_makes_any_response_a_download() {
    let text: Mime = "text/plain".parse().unwrap();
    for value in [
        "attachment",
        "ATTACHMENT; filename=a.txt",
        " attachment ;filename=\"a.txt\"",
        "foo",
    ] {
        assert!(
            navigation_response_is_download(Some(&disposition(value)), Some(&text), None),
            "{value}"
        );
    }
    for value in ["inline", "inline; filename=a.txt", "filename=a.txt", ""] {
        assert!(
            !navigation_response_is_download(Some(&disposition(value)), Some(&text), None),
            "{value}"
        );
    }
    // `inline` does not stop a type Servo cannot show from being saved.
    let pdf: Mime = "application/pdf".parse().unwrap();
    assert!(navigation_response_is_download(
        Some(&disposition("inline; filename=a.pdf")),
        Some(&pdf),
        None
    ));
}

#[test]
fn disposition_types() {
    assert!(content_disposition_is_attachment("attachment"));
    assert!(content_disposition_is_attachment("Attachment; filename=a"));
    assert!(content_disposition_is_attachment("foo"));
    assert!(!content_disposition_is_attachment("inline"));
    assert!(!content_disposition_is_attachment("INLINE; filename=a"));
    assert!(!content_disposition_is_attachment("filename=a.pdf"));
    assert!(!content_disposition_is_attachment("  "));
}

#[test]
fn extended_file_names() {
    assert_eq!(
        content_disposition_file_name("attachment; filename*=UTF-8''%E2%82%AC%20rates.pdf")
            .as_deref(),
        Some("€ rates.pdf")
    );
    assert_eq!(
        content_disposition_file_name("attachment; filename*=iso-8859-1'en'%A3%20rates").as_deref(),
        Some("£ rates")
    );
    // `filename*` wins wherever it is.
    assert_eq!(
        content_disposition_file_name(
            "attachment; filename=\"EURO rates.pdf\"; filename*=utf-8''%E2%82%AC%20rates.pdf"
        )
        .as_deref(),
        Some("€ rates.pdf")
    );
    // Invalid UTF-8 or an unknown charset falls back to `filename`.
    assert_eq!(
        content_disposition_file_name("attachment; filename*=UTF-8''%FF.pdf; filename=a.pdf")
            .as_deref(),
        Some("a.pdf")
    );
    assert_eq!(
        content_disposition_file_name("attachment; filename*=koi8-r''%C1.pdf; filename=a.pdf")
            .as_deref(),
        Some("a.pdf")
    );
}

#[test]
fn plain_file_names() {
    assert_eq!(
        content_disposition_file_name("attachment; filename=report.pdf").as_deref(),
        Some("report.pdf")
    );
    assert_eq!(
        content_disposition_file_name("attachment; FILENAME = \"a; b \\\"c\\\".txt\"").as_deref(),
        Some("a; b \"c\".txt")
    );
    assert_eq!(
        content_disposition_file_name("attachment; filename=\"%E2%82%AC.txt\"").as_deref(),
        Some("€.txt")
    );
    // Not valid UTF-8 once decoded: kept as sent.
    assert_eq!(
        content_disposition_file_name("attachment; filename=\"100%FF.txt\"").as_deref(),
        Some("100%FF.txt")
    );
    assert_eq!(
        content_disposition_file_name("attachment; filename=\"\""),
        None
    );
    assert_eq!(content_disposition_file_name("attachment"), None);
}

#[test]
fn raw_utf8_in_a_plain_file_name() {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_bytes("attachment; filename=\"résumé.pdf\"".as_bytes()).unwrap(),
    );
    assert_eq!(
        download_file_name(Some(&headers), None, &url("https://e.org/x")),
        "résumé.pdf"
    );
}

#[test]
fn sanitising() {
    assert_eq!(sanitize_file_name("../../etc/passwd"), "_.._etc_passwd");
    assert_eq!(sanitize_file_name("..\\..\\boot.ini"), "_.._boot.ini");
    assert_eq!(sanitize_file_name(".."), "");
    assert_eq!(sanitize_file_name(". . ."), "");
    assert_eq!(sanitize_file_name(".bashrc"), "bashrc");
    assert_eq!(sanitize_file_name("a\u{0}b\nc\u{7f}.txt"), "abc.txt");
    assert_eq!(sanitize_file_name("  report.pdf  "), "report.pdf");
    assert_eq!(sanitize_file_name("dir/name.txt"), "dir_name.txt");
}

#[test]
fn long_names_keep_their_extension() {
    let long = format!("{}.pdf", "a".repeat(300));
    let short = sanitize_file_name(&long);
    assert!(short.len() <= 200);
    assert!(short.ends_with(".pdf"));
    assert!(short.starts_with("aaaa"));

    // Two-byte characters: the cut must land on a character boundary.
    let long = format!("{}.txt", "é".repeat(150));
    let short = sanitize_file_name(&long);
    assert!(short.len() <= 200);
    assert!(short.ends_with(".txt"));
    assert_eq!(short.trim_end_matches(".txt").chars().count(), 98);

    // A long "extension" is part of the name.
    let long = format!("x.{}", "b".repeat(300));
    let short = sanitize_file_name(&long);
    assert_eq!(short.len(), 200);
}

#[test]
fn file_name_fallbacks() {
    let page = url("https://e.org/files/r%C3%A9sum%C3%A9.pdf?x=1#top");
    let headers = disposition("attachment; filename=\"from header.pdf\"");
    assert_eq!(
        download_file_name(Some(&headers), Some("attribute.pdf"), &page),
        "from header.pdf"
    );
    assert_eq!(
        download_file_name(
            Some(&disposition("attachment")),
            Some(" attribute.pdf "),
            &page
        ),
        "attribute.pdf"
    );
    assert_eq!(download_file_name(None, Some(""), &page), "résumé.pdf");
    assert_eq!(
        download_file_name(None, None, &url("https://e.org/a/b/")),
        "b"
    );
    assert_eq!(
        download_file_name(None, None, &url("https://e.org/")),
        "download"
    );
    assert_eq!(
        download_file_name(None, None, &url("data:application/pdf,abc")),
        "download"
    );
    // A name that sanitises to nothing falls through to the next one.
    assert_eq!(
        download_file_name(
            Some(&disposition("attachment; filename=\"..\"")),
            None,
            &page
        ),
        "résumé.pdf"
    );
    assert_eq!(
        download_file_name(
            Some(&disposition(
                "attachment; filename=\"../../.ssh/authorized_keys\""
            )),
            None,
            &page
        ),
        "_.._.ssh_authorized_keys"
    );
}

#[test]
fn directories() {
    let home = Some(PathBuf::from("/home/user"));
    assert_eq!(
        download_directory_from(Some(PathBuf::from("/tmp/dl")), home.clone(), true),
        PathBuf::from("/tmp/dl")
    );
    assert_eq!(
        download_directory_from(Some(PathBuf::new()), home.clone(), true),
        PathBuf::from("/data/home/Downloads")
    );
    assert_eq!(
        download_directory_from(None, home, false),
        PathBuf::from("/home/user/Downloads")
    );
    assert_eq!(
        download_directory_from(None, None, false),
        std::env::temp_dir()
    );
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("servo-download-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        TemporaryDirectory(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn reserve(dir: &Path, name: &str) -> (String, String) {
    let (final_path, part_path, _file) = reserve_download_file(dir, name).unwrap();
    assert!(part_path.exists());
    let file_name = |path: &Path| path.file_name().unwrap().to_str().unwrap().to_owned();
    (file_name(&final_path), file_name(&part_path))
}

#[test]
fn collisions_get_numbers_before_the_extension() {
    let directory = TemporaryDirectory::new("collisions");
    // The directory does not exist yet: it is created.
    let dir = directory.0.join("Downloads");

    assert_eq!(
        reserve(&dir, "a.pdf"),
        ("a.pdf".into(), "a.pdf.part".into())
    );
    // The first download is still running (its .part exists).
    assert_eq!(reserve(&dir, "a.pdf").0, "a (1).pdf");
    fs::write(dir.join("b.pdf"), b"done").unwrap();
    assert_eq!(reserve(&dir, "b.pdf").0, "b (1).pdf");
    assert_eq!(reserve(&dir, "b.pdf").0, "b (2).pdf");

    fs::write(dir.join("x.tar.gz"), b"").unwrap();
    assert_eq!(reserve(&dir, "x.tar.gz").0, "x (1).tar.gz");
    fs::write(dir.join("README"), b"").unwrap();
    assert_eq!(reserve(&dir, "README").0, "README (1)");
}
