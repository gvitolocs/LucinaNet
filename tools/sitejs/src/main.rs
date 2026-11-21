use reqwest::blocking::Client;
use reqwest::Url;
use scraper::{Html, Selector};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

struct Page {
    name: &'static str,
    url: &'static str,
}

const PAGES: &[Page] = &[
    Page { name: "example", url: "https://example.com/" },
    Page { name: "wikipedia", url: "https://en.wikipedia.org/wiki/Rust_(programming_language)" },
    Page { name: "hn", url: "https://news.ycombinator.com/" },
    Page { name: "github", url: "https://github.com/servo/servo" },
    Page { name: "discord", url: "https://discord.com/login" },
];

const WIKI_EXTRA: &[(&str, &str)] = &[
    (
        "ext-rl-startup.js",
        "https://en.wikipedia.org/w/load.php?lang=en&modules=startup&only=scripts&raw=1&skin=vector-2022",
    ),
    (
        "ext-rl-jquery-base.js",
        "https://en.wikipedia.org/w/load.php?lang=en&modules=jquery%2Cmediawiki.base&skin=vector-2022",
    ),
];

#[derive(Serialize)]
struct ManifestEntry {
    file: String,
    url: Option<String>,
    kind: &'static str,
    is_module: bool,
    bytes: usize,
    sha256: String,
    http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn is_js_type(t: Option<&str>) -> bool {
    match t.map(|s| s.trim().to_ascii_lowercase()) {
        None => true,
        Some(t) => matches!(
            t.as_str(),
            "" | "text/javascript" | "application/javascript" | "module"
        ),
    }
}

fn sanitize_basename(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.');
    let mut cleaned: String = if cleaned.is_empty() { "script".to_string() } else { cleaned.to_string() };
    if cleaned.len() > 60 {
        cleaned.truncate(60);
    }
    if !cleaned.ends_with(".js") {
        cleaned.push_str(".js");
    }
    cleaned
}

fn fetch(client: &Client, url: &str) -> Result<(Vec<u8>, u16), (Option<u16>, String)> {
    match client.get(url).send() {
        Ok(resp) => {
            let status = resp.status().as_u16();
            match resp.bytes() {
                Ok(b) => Ok((b.to_vec(), status)),
                Err(e) => Err((Some(status), e.to_string())),
            }
        }
        Err(e) => {
            let status = e.status().map(|s| s.as_u16());
            Err((status, e.to_string()))
        }
    }
}

fn record(
    dir: &Path,
    file: String,
    url: Option<String>,
    kind: &'static str,
    is_module: bool,
    result: Result<(Vec<u8>, u16), (Option<u16>, String)>,
) -> ManifestEntry {
    match result {
        Ok((bytes, status)) => {
            let _ = fs::write(dir.join(&file), &bytes);
            let sha = format!("{:x}", Sha256::digest(&bytes));
            ManifestEntry {
                file,
                url,
                kind,
                is_module,
                bytes: bytes.len(),
                sha256: sha,
                http_status: Some(status),
                error: None,
            }
        }
        Err((status, msg)) => ManifestEntry {
            file,
            url,
            kind,
            is_module,
            bytes: 0,
            sha256: String::new(),
            http_status: status,
            error: Some(msg),
        },
    }
}

fn process_page(client: &Client, page: &Page, out_dir: &Path) -> (usize, usize) {
    let dir = out_dir.join(page.name);
    fs::create_dir_all(&dir).expect("create output dir");

    let (html_bytes, _status) = match fetch(client, page.url) {
        Ok(v) => v,
        Err((status, msg)) => {
            eprintln!("{}: failed to fetch page ({:?}, {})", page.name, status, msg);
            return (0, 0);
        }
    };
    fs::write(dir.join("page.html"), &html_bytes).expect("write page.html");
    let html = String::from_utf8_lossy(&html_bytes).into_owned();
    let doc = Html::parse_document(&html);
    let base = Url::parse(page.url).expect("valid page url");

    let sel = Selector::parse("script, link").unwrap();
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut idx: usize = 0;

    for el in doc.select(&sel) {
        let tag = el.value().name();
        if tag == "script" {
            let ty = el.value().attr("type");
            if !is_js_type(ty) {
                continue;
            }
            let is_module = ty.map(|t| t.trim().eq_ignore_ascii_case("module")).unwrap_or(false);
            let n = format!("{:02}", idx);
            idx += 1;
            if let Some(src) = el.value().attr("src") {
                let abs = base.join(src).map(|u| u.to_string()).unwrap_or_else(|_| src.to_string());
                let file = format!("ext-{}-{}", n, sanitize_basename(src));
                let r = fetch(client, &abs);
                entries.push(record(&dir, file, Some(abs), "external", is_module, r));
            } else {
                let text = el.text().collect::<String>();
                if !text.trim().is_empty() {
                    let file = format!("inline-{}.js", n);
                    let bytes = text.into_bytes();
                    let sha = format!("{:x}", Sha256::digest(&bytes));
                    let _ = fs::write(dir.join(&file), &bytes);
                    entries.push(ManifestEntry {
                        file,
                        url: None,
                        kind: "inline",
                        is_module,
                        bytes: bytes.len(),
                        sha256: sha,
                        http_status: None,
                        error: None,
                    });
                }
            }
        } else if tag == "link" {
            let rel = el.value().attr("rel").unwrap_or("").to_ascii_lowercase();
            let is_preload_script = rel == "modulepreload"
                || (rel == "preload" && el.value().attr("as").map(|a| a.eq_ignore_ascii_case("script")).unwrap_or(false));
            if !is_preload_script {
                continue;
            }
            if let Some(href) = el.value().attr("href") {
                let n = format!("{:02}", idx);
                idx += 1;
                let abs = base.join(href).map(|u| u.to_string()).unwrap_or_else(|_| href.to_string());
                let file = format!("pre-{}-{}", n, sanitize_basename(href));
                let r = fetch(client, &abs);
                entries.push(record(&dir, file, Some(abs), "preload", rel == "modulepreload", r));
            }
        }
    }

    if page.name == "wikipedia" {
        for (file, url) in WIKI_EXTRA {
            let r = fetch(client, url);
            entries.push(record(&dir, file.to_string(), Some(url.to_string()), "resourceloader", false, r));
        }
    }

    let manifest = serde_json::to_string_pretty(&entries).unwrap();
    fs::write(dir.join("manifest.json"), manifest).expect("write manifest");

    let total: usize = entries.iter().map(|e| e.bytes).sum();
    (entries.len(), total)
}

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sites".to_string());
    let out_dir = Path::new(&out_dir);
    fs::create_dir_all(out_dir).expect("create out dir");

    let client = Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build client");

    for page in PAGES {
        let (count, total) = process_page(&client, page, out_dir);
        println!("{} {} {}", page.name, count, total);
    }
}
