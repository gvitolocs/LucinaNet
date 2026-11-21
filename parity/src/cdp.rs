//! Just enough of the Chrome DevTools Protocol: open a headless Chromium, load a page, screenshot it, run a script.

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

/// The same questions as the engines get.
const DUMP_JS: &str = include_str!("../../crates/lucinanet-ui/src/dump.js");

pub struct Chrome {
    child: Child,
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    session: String,
    next: u64,
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Chrome {
    pub fn start(width: u32, height: u32) -> Chrome {
        let profile = std::env::temp_dir().join(format!("lucinanet-parity-{}", std::process::id()));
        let binary = std::env::var("CHROME").unwrap_or_else(|_| "/usr/bin/google-chrome".to_owned());
        // Chromium sees the engine's fonts, so that a difference is layout and not typefaces.
        let fonts = std::env::var("LUCINANET_FONT_DIRS").unwrap_or_default();
        let dirs: String = fonts.split(':').map(|d| format!("<dir>{d}</dir>")).collect();
        let config = profile.with_extension("fonts.conf");
        let aliases: String = ["serif", "sans-serif", "cursive", "fantasy", "system-ui"]
            .iter()
            .map(|g| format!("<alias binding=\"same\"><family>{g}</family><prefer><family>DejaVu Sans</family></prefer></alias>"))
            .collect();
        std::fs::write(&config, format!("<?xml version=\"1.0\"?><fontconfig>{dirs}{aliases}<alias binding=\"same\"><family>monospace</family><prefer><family>DejaVu Sans Mono</family></prefer></alias></fontconfig>")).expect("write fonts.conf");
        let mut child = Command::new(binary)
            .env("FONTCONFIG_FILE", &config)
            .args(["--headless=new", "--disable-gpu", "--no-first-run", "--hide-scrollbars", "--remote-debugging-port=0"])
            // The engines say this too: the same request should get the same page.
            .arg("--user-agent=Mozilla/5.0 (X11; Linux aarch64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 LucinaNet/0.1")
            .arg(format!("--window-size={width},{height}"))
            .arg(format!("--user-data-dir={}", profile.display()))
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("start Chromium");
        let stderr = child.stderr.take().expect("its stderr");
        let mut lines = BufReader::new(stderr).lines();
        let url = loop {
            let line = lines.next().expect("DevTools URL").expect("a line");
            if let Some(url) = line.strip_prefix("DevTools listening on ") {
                break url.to_owned();
            }
        };
        std::thread::spawn(move || for _ in lines {});
        let (socket, _) = tungstenite::connect(url).expect("connect to DevTools");
        let mut chrome = Chrome {
            child,
            socket,
            session: String::new(),
            next: 1,
        };
        let target = chrome.call("Target.createTarget", json!({"url": "about:blank"})).expect("a target");
        let attached = chrome
            .call("Target.attachToTarget", json!({"targetId": target["targetId"], "flatten": true}))
            .expect("attach");
        chrome.session = attached["sessionId"].as_str().expect("a session").to_owned();
        chrome.call("Page.enable", json!({})).expect("Page");
        chrome
            .call(
                "Emulation.setDeviceMetricsOverride",
                json!({"width": width, "height": height, "deviceScaleFactor": 1, "mobile": false}),
            )
            .expect("metrics");
        chrome
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if !self.session.is_empty() {
            msg["sessionId"] = json!(self.session);
        }
        self.socket.send(Message::text(msg.to_string())).map_err(|e| e.to_string())?;
        loop {
            let text = match self.socket.read().map_err(|e| e.to_string())? {
                Message::Text(t) => t,
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
            if v["id"] == id {
                return match v.get("error") {
                    Some(e) => Err(e.to_string()),
                    None => Ok(v["result"].clone()),
                };
            }
        }
    }

    /// Load `url`, wait for it to finish or `limit`, write the screenshot, return the dump.
    pub fn page(&mut self, url: &str, png: &Path, limit: Duration) -> Result<Value, String> {
        self.call("Page.navigate", json!({"url": url}))?;
        let started = Instant::now();
        // Polling keeps this free of event plumbing.
        while started.elapsed() < limit {
            let ready = self.call(
                "Runtime.evaluate",
                json!({"expression": "document.readyState", "returnByValue": true}),
            )?;
            if ready["result"]["value"] == "complete" {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        std::thread::sleep(Duration::from_millis(800));
        let shot = self.call("Page.captureScreenshot", json!({"format": "png"}))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(shot["data"].as_str().unwrap_or(""))
            .map_err(|e| e.to_string())?;
        std::fs::write(png, bytes).map_err(|e| e.to_string())?;
        let dump = self.call("Runtime.evaluate", json!({"expression": DUMP_JS, "returnByValue": true}))?;
        serde_json::from_str(dump["result"]["value"].as_str().unwrap_or("null")).map_err(|e| e.to_string())
    }
}
