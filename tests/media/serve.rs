// A tiny static file server for the media pages, in place of `python3 -m http.server` (no Python here). It answers a GET of a
// path under its root with the file and a content type, and nothing else: fetch() over HTTP is all the MSE pages need, the media
// element reads from a Blob URL rather than this server, so there are no range requests to answer.
//
//   rustc --edition 2021 -O tests/media/serve.rs -o $TMP/serve
//   $TMP/serve <port> <root>
//
// It serves forever until the process is killed, on one thread (a test fetches a handful of files).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "webm" => "video/webm",
        "mp4" | "m4s" | "m4v" => "video/mp4",
        "html" | "htm" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "wav" => "audio/wav",
        "png" => "image/png",
        _ => "application/octet-stream",
    }
}

// Resolve a request path under `root`, refusing anything that climbs out of it.
fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let trimmed = url_path.trim_start_matches('/');
    let mut p = root.to_path_buf();
    for seg in trimmed.split('/') {
        match seg {
            "" | "." => {},
            ".." => return None,
            s => p.push(s),
        }
    }
    p.exists().then_some(p)
}

fn handle(mut stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    // Drain the headers so the client is not left waiting to send.
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    let reply = |code: &str, body: &[u8], ctype: &str, s: &mut TcpStream| -> std::io::Result<()> {
        let header = format!("HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        s.write_all(header.as_bytes())?;
        s.write_all(body)
    };
    if parts.len() < 2 || (parts[0] != "GET" && parts[0] != "HEAD") {
        return reply("405 Method Not Allowed", b"", "text/plain", &mut stream);
    }
    // The request target carries the query and the fragment; the pages select a run with ?variant=…, and the file is only the part
    // before them. Serving the whole target as a path 404s every variant.
    let target = parts[1].split(|c| c == '?' || c == '#').next().unwrap_or(parts[1]);
    match resolve(root, target) {
        Some(path) if path.is_file() => {
            let mut body = Vec::new();
            std::fs::File::open(&path)?.read_to_end(&mut body)?;
            reply("200 OK", &body, content_type(&path), &mut stream)
        },
        Some(_) => reply("404 Not Found", b"directory", "text/plain", &mut stream),
        None => reply("403 Forbidden", b"no", "text/plain", &mut stream),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|s| s.parse().ok()).expect("usage: serve <port> <root>");
    let root = PathBuf::from(args.next().expect("usage: serve <port> <root>"));
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    eprintln!("serve: {} on http://127.0.0.1:{port}/", root.display());
    for conn in listener.incoming() {
        if let Ok(stream) = conn {
            let _ = handle(stream, &root);
        }
    }
}
