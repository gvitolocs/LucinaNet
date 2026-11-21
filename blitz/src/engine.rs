//! The engine thread on Blitz: one document, painted by Vello CPU into a buffer. A wheel notch or a key is
//! handled in the document at once; the screen sees it on the next paint. Painting is throttled while
//! a page's resources arrive: each one must not cost a whole frame on one CPU.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::task::{Context, Wake};
use std::time::{Duration, Instant};

use anyrender::{ImageRenderer, PaintScene};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{Document, DocumentConfig, FontContext, util::Color};
use blitz_html::HtmlDocument;
use blitz_net::Provider;
use blitz_paint::paint_scene;
use blitz_traits::events::{
    BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, BlitzWheelDelta, BlitzWheelEvent, KeyState,
    MouseEventButton, MouseEventButtons, PointerCoords, UiEvent,
};
use blitz_traits::net::Request;
use blitz_traits::navigation::{NavigationOptions, NavigationProvider};
use blitz_traits::shell::{ColorScheme, ShellProvider, Viewport};
use keyboard_types::{Code, Key, Location, Modifiers};
use linebender_resource_handle::Blob;
use lucinanet_ui::{Cmd, News, Page, Starting, Waker, rss_kib};
use peniko::Fill;
use peniko::kurbo::Rect;
use url::Url;

const USER_AGENT_FONTS: &str = "/usr/share/fonts/dejavu:/usr/share/fonts/noto:/usr/share/fonts/truetype/dejavu";
/// While resources arrive, at most this often.
const LOADING_PAINT: Duration = Duration::from_millis(200);

struct Handle {
    commands: mpsc::Sender<Cmd>,
    waker: Waker,
}

impl lucinanet_ui::Engine for Handle {
    fn send(&self, cmd: Cmd) {
        // The engine outlives the window; if it has gone, the app is ending.
        let _ = self.commands.send(cmd);
        self.waker.wake();
    }
}

pub fn start(starting: Starting) -> Box<dyn lucinanet_ui::Engine> {
    let (commands, inbox) = mpsc::channel();
    let waker = Waker::default();
    let handle = Handle {
        commands,
        waker: waker.clone(),
    };
    std::thread::Builder::new()
        .name("engine".into())
        // Stylo and layout recurse with the DOM's depth.
        .stack_size(8 << 20)
        .spawn(move || run(starting, inbox, waker))
        .expect("the engine thread");
    Box::new(handle)
}

struct Shell {
    dirty: AtomicBool,
    waker: Waker,
    title: Mutex<Option<String>>,
}

impl ShellProvider for Shell {
    fn request_redraw(&self) {
        self.dirty.store(true, Ordering::Relaxed);
        self.waker.wake();
    }

    fn set_window_title(&self, title: String) {
        *self.title.lock().unwrap() = Some(title);
        self.waker.wake();
    }
}

struct Nav {
    wanted: Mutex<Vec<Url>>,
    waker: Waker,
}

impl NavigationProvider for Nav {
    fn navigate_to(&self, options: NavigationOptions) {
        self.wanted.lock().unwrap().push(options.url);
        self.waker.wake();
    }
}

struct TaskWake(Waker);

impl Wake for TaskWake {
    fn wake(self: Arc<Self>) {
        self.0.wake();
    }
}

fn fonts() -> FontContext {
    use parley::fontique::{Collection, CollectionOptions, SourceCache};
    let mut fonts = FontContext {
        source_cache: SourceCache::new_shared(),
        collection: Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        }),
    };
    fonts
        .collection
        .register_fonts(Blob::new(Arc::new(blitz_dom::BULLET_FONT) as _), None);
    let dirs = std::env::var("LUCINANET_FONT_DIRS").unwrap_or_else(|_| USER_AGENT_FONTS.to_owned());
    let mut families = Vec::new();
    for dir in dirs.split(':') {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for path in entries.flatten().map(|e| e.path()) {
            let known = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| ["ttf", "otf", "ttc"].contains(&e));
            if !known {
                continue;
            }
            // Mapped: CJK collections are 20 MB on a slow disk, and only the glyphs in use get read.
            if let Ok(data) = std::fs::File::open(&path).and_then(|file| unsafe { memmap2::Mmap::map(&file) }) {
                for (family, _) in fonts.collection.register_fonts(Blob::new(Arc::new(data) as _), None) {
                    families.push(family);
                }
            }
        }
    }
    // The generic names and the fallback for every script go to what there is.
    use parley::fontique::{FallbackKey, GenericFamily};
    for generic in [
        GenericFamily::Serif,
        GenericFamily::SansSerif,
        GenericFamily::SystemUi,
        GenericFamily::Cursive,
        GenericFamily::Fantasy,
    ] {
        let list = fonts.collection.family_id("DejaVu Sans");
        fonts.collection.set_generic_families(generic, list.into_iter());
    }
    let mono = fonts.collection.family_id("DejaVu Sans Mono");
    fonts
        .collection
        .set_generic_families(GenericFamily::Monospace, mono.into_iter());
    let mut cjk = |languages: &[&str]| -> Vec<_> {
        languages
            .iter()
            .filter_map(|language| fonts.collection.family_id(&format!("Noto Sans CJK {language}")))
            .collect()
    };
    let all_cjk = cjk(&["SC", "TC", "JP", "KR"]);
    let scripts = [
        (b"Hani", cjk(&["SC", "TC", "JP", "KR"])),
        (b"Bopo", cjk(&["TC"])),
        (b"Hira", cjk(&["JP"])),
        (b"Kana", cjk(&["JP"])),
        (b"Hang", cjk(&["KR"])),
    ];
    for (script, list) in scripts {
        let key = FallbackKey::new(parley::fontique::Script::from_bytes(*script), None);
        fonts.collection.set_fallbacks(key, list.into_iter());
    }
    families.retain(|family| !all_cjk.contains(family));
    for script in b"LatnGrekCyrlArabHebrZyyy".as_chunks::<4>().0.iter() {
        let key = FallbackKey::new(parley::fontique::Script::from_bytes(*script), None);
        fonts.collection.set_fallbacks(key, families.iter().copied());
    }
    fonts
}

struct Loaded {
    url: Url,
    html: String,
}

struct Tab {
    doc: Box<dyn Document>,
    buttons: MouseEventButtons,
    pointer: (f32, f32),
}

fn run(starting: Starting, inbox: Receiver<Cmd>, waker: Waker) {
    let Starting {
        width,
        height,
        scale,
        first,
        page,
        news,
    } = starting;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("a runtime");
    let _inside = runtime.enter();
    let started = Instant::now();

    let net_waker = waker.clone();
    let net = Arc::new(Provider::new(Some(Arc::new(move |_: usize| net_waker.wake()))));
    let shell = Arc::new(Shell {
        dirty: AtomicBool::new(false),
        waker: waker.clone(),
        title: Mutex::new(None),
    });
    let nav = Arc::new(Nav {
        wanted: Mutex::new(Vec::new()),
        waker: waker.clone(),
    });
    let font_ctx = fonts();
    let task_waker = std::task::Waker::from(Arc::new(TaskWake(waker.clone())));
    let (loaded_tx, loaded) = mpsc::channel::<Result<Loaded, String>>();

    let mut renderer = VelloCpuImageRenderer::new(width, height);
    let mut rgba = vec![0u8; width as usize * height as usize * 4];
    let mut tab: Option<Tab> = None;
    let mut history: Vec<Url> = Vec::new();
    let mut at = 0usize;
    let mut loading_since: Option<Instant> = Some(Instant::now());
    let mut net_was_busy = false;
    let mut dirty = false;
    let mut last_paint = Instant::now() - LOADING_PAINT;
    let trace = std::env::var_os("LUCINA_APP_TRACE").is_some();

    let fetch = |url: Url| {
        let net = net.clone();
        let tx = loaded_tx.clone();
        let waker = waker.clone();
        runtime.spawn(async move {
            let result = net.fetch_async(Request::get(url.clone())).await;
            let _ = tx.send(match result {
                Ok((resolved, bytes)) => Ok(Loaded {
                    url: resolved.parse().unwrap_or(url),
                    html: String::from_utf8_lossy(&bytes).into_owned(),
                }),
                Err(e) => Err(format!("{e:?}")),
            });
            waker.wake();
        });
    };
    fetch(first.clone());
    history.push(first);
    news(News::Loading(true));
    eprintln!("lucinanet: engine up in {} ms, rss {} KiB", started.elapsed().as_millis(), rss_kib());

    loop {
        let mut go: Option<Url> = None;
        loop {
            match inbox.try_recv() {
                Ok(Cmd::Load(url)) => go = Some(url),
                Ok(Cmd::Back) if at > 0 => {
                    at -= 1;
                    loading_since = Some(Instant::now());
                    news(News::Loading(true));
                    fetch(history[at].clone());
                },
                Ok(Cmd::Forward) if at + 1 < history.len() => {
                    at += 1;
                    loading_since = Some(Instant::now());
                    news(News::Loading(true));
                    fetch(history[at].clone());
                },
                Ok(Cmd::Reload) => {
                    loading_since = Some(Instant::now());
                    news(News::Loading(true));
                    fetch(history[at].clone());
                },
                Ok(Cmd::Back | Cmd::Forward) => {},
                Ok(Cmd::Dump(path)) => {
                    if let Some(tab) = &tab {
                        dump(&*tab.doc, &path);
                    }
                    news(News::Dumped);
                },
                Ok(input) => {
                    if let Some(tab) = &mut tab {
                        apply(tab, input, scale);
                        dirty = true;
                    }
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if let Some(url) = nav.wanted.lock().unwrap().pop() {
            go = Some(url);
        }
        if let Some(url) = go {
            history.truncate(at + 1);
            history.push(url.clone());
            at = history.len() - 1;
            loading_since = Some(Instant::now());
            news(News::Loading(true));
            news(News::History { back: at > 0, forward: false });
            fetch(url);
        }

        while let Ok(result) = loaded.try_recv() {
            match result {
                Ok(page_loaded) => {
                    let t = Instant::now();
                    let config = DocumentConfig {
                        base_url: Some(page_loaded.url.to_string()),
                        net_provider: Some(net.clone() as _),
                        shell_provider: Some(shell.clone() as _),
                        navigation_provider: Some(nav.clone() as _),
                        font_ctx: Some(font_ctx.clone()),
                        viewport: Some(Viewport::new(width, height, scale, ColorScheme::Light)),
                        ..Default::default()
                    };
                    let doc = HtmlDocument::from_html(&page_loaded.html, config);
                    if let Some(title) = doc.find_title_node() {
                        news(News::Title(title.text_content()));
                    }
                    news(News::Url(page_loaded.url.to_string()));
                    news(News::History {
                        back: at > 0,
                        forward: at + 1 < history.len(),
                    });
                    eprintln!(
                        "lucinanet: {} parsed in {} ms ({} KiB of HTML)",
                        page_loaded.url,
                        t.elapsed().as_millis(),
                        page_loaded.html.len() / 1024
                    );
                    tab = Some(Tab {
                        doc: Box::new(doc),
                        buttons: MouseEventButtons::None,
                        pointer: (0.0, 0.0),
                    });
                    dirty = true;
                },
                Err(error) => {
                    eprintln!("lucinanet: load failed: {error}");
                    let html = format!(
                        "<!doctype html><title>Could not load</title><body style='font: 16px sans-serif; margin: 48px'><h1>This page could not be loaded</h1><p>{}</p><p style='color:#666'>{}</p>",
                        history.get(at).map(Url::as_str).unwrap_or(""),
                        error.replace('<', "&lt;")
                    );
                    let config = DocumentConfig {
                        net_provider: Some(net.clone() as _),
                        shell_provider: Some(shell.clone() as _),
                        navigation_provider: Some(nav.clone() as _),
                        font_ctx: Some(font_ctx.clone()),
                        viewport: Some(Viewport::new(width, height, scale, ColorScheme::Light)),
                        ..Default::default()
                    };
                    tab = Some(Tab {
                        doc: Box::new(HtmlDocument::from_html(&html, config)),
                        buttons: MouseEventButtons::None,
                        pointer: (0.0, 0.0),
                    });
                    dirty = true;
                },
            }
        }

        let busy = !net.is_empty();
        if let Some(tab) = &mut tab {
            let cx = Context::from_waker(&task_waker);
            if tab.doc.poll(Some(cx)) {
                dirty = true;
            }
            if busy || net_was_busy {
                dirty = true;
            }
            if shell.dirty.swap(false, Ordering::Relaxed) {
                dirty = true;
            }
            let wait_for_more = busy && last_paint.elapsed() < LOADING_PAINT;
            if dirty && !wait_for_more {
                let (t0, mut inner) = (Instant::now(), tab.doc.inner_mut());
                inner.resolve(started.elapsed().as_secs_f64());
                let resolved = t0.elapsed();
                let t1 = Instant::now();
                // The context keeps the last frame's paints otherwise, and those name images a closed page dropped.
                renderer.reset();
                renderer.render(
                    |scene| {
                        scene.fill(
                            Fill::NonZero,
                            Default::default(),
                            Color::WHITE,
                            Default::default(),
                            &Rect::new(0.0, 0.0, width as f64, height as f64),
                        );
                        paint_scene(scene, &mut inner, scale as f64, width, height, 0, 0);
                    },
                    &mut rgba,
                );
                let painted = t1.elapsed();
                drop(inner);
                {
                    let mut page = page.lock().unwrap();
                    copy_rgba(&rgba, width, height, &mut page);
                }
                if trace {
                    eprintln!(
                        "lucinanet: resolve {} ms, paint {} ms",
                        resolved.as_millis(),
                        painted.as_millis()
                    );
                }
                news(News::Frame);
                dirty = false;
                last_paint = Instant::now();
            }
            if !busy && !dirty {
                if let Some(since) = loading_since.take() {
                    eprintln!(
                        "lucinanet: load complete in {} ms, rss {} KiB",
                        since.elapsed().as_millis(),
                        rss_kib()
                    );
                    news(News::Loading(false));
                }
            }
        }
        net_was_busy = busy;
        let timeout = if dirty || busy { Duration::from_millis(50) } else { Duration::from_millis(500) };
        waker.wait(timeout);
    }
}

fn copy_rgba(rgba: &[u8], w: u32, h: u32, page: &mut Page) {
    page.w = w;
    page.h = h;
    page.px.resize(w as usize * h as usize, 0);
    for (dst, src) in page.px.iter_mut().zip(rgba.chunks_exact(4)) {
        *dst = (src[0] as u32) << 16 | (src[1] as u32) << 8 | src[2] as u32;
    }
}

fn coords(tab: &Tab, scale: f32) -> PointerCoords {
    let (x, y) = (tab.pointer.0 / scale, tab.pointer.1 / scale);
    let scroll = tab.doc.inner().viewport_scroll();
    PointerCoords {
        page_x: x + scroll.x as f32,
        page_y: y + scroll.y as f32,
        screen_x: x,
        screen_y: y,
        client_x: x,
        client_y: y,
    }
}

fn pointer(tab: &Tab, scale: f32, button: MouseEventButton) -> BlitzPointerEvent {
    BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: coords(tab, scale),
        button,
        buttons: tab.buttons,
        mods: Modifiers::empty(),
        details: Default::default(),
        element: Default::default(),
        active_pointers: Default::default(),
    }
}

fn apply(tab: &mut Tab, cmd: Cmd, scale: f32) {
    match cmd {
        Cmd::Move(x, y) => {
            tab.pointer = (x, y);
            let event = pointer(tab, scale, MouseEventButton::Main);
            tab.doc.handle_ui_event(UiEvent::PointerMove(event));
        },
        Cmd::Button { down, x, y } => {
            tab.pointer = (x, y);
            if down {
                tab.buttons |= MouseEventButtons::Primary;
                let event = pointer(tab, scale, MouseEventButton::Main);
                tab.doc.handle_ui_event(UiEvent::PointerDown(event));
            } else {
                tab.buttons -= MouseEventButtons::Primary;
                let event = pointer(tab, scale, MouseEventButton::Main);
                tab.doc.handle_ui_event(UiEvent::PointerUp(event));
            }
        },
        Cmd::Wheel { y, x, y_at } => {
            tab.pointer = (x, y_at);
            let event = BlitzWheelEvent {
                delta: BlitzWheelDelta::Pixels(0.0, y / scale as f64),
                coords: coords(tab, scale),
                buttons: tab.buttons,
                mods: Modifiers::empty(),
                element: Default::default(),
            };
            tab.doc.handle_ui_event(UiEvent::Wheel(event));
        },
        Cmd::Key { key, modifiers } => {
            let text = match &key {
                Key::Character(c) => Some(smol_str::SmolStr::new(c)),
                _ => None,
            };
            for state in [KeyState::Pressed, KeyState::Released] {
                let event = BlitzKeyEvent {
                    key: key.clone(),
                    code: Code::Unidentified,
                    modifiers,
                    location: Location::Standard,
                    is_auto_repeating: false,
                    is_composing: false,
                    state,
                    text: text.clone(),
                };
                let ui = if state == KeyState::Pressed {
                    UiEvent::KeyDown(event)
                } else {
                    UiEvent::KeyUp(event)
                };
                tab.doc.handle_ui_event(ui);
            }
        },
        Cmd::Load(_) | Cmd::Back | Cmd::Forward | Cmd::Reload | Cmd::Dump(_) => {},
    }
}

/// The parity harness's JSON (`lucinanet_ui::DUMP_JS`), from the document instead of from a script.
fn dump(doc: &dyn Document, path: &std::path::Path) {
    let inner = doc.inner();
    let mut boxes = Vec::new();
    let mut text = String::new();
    let mut elements = 0usize;
    if let Some(root) = inner.try_root_element() {
        walk(&inner, root.id.into(), "html".to_owned(), &mut boxes, &mut text, &mut elements);
    }
    let title = inner.find_title_node().map(|n| n.text_content()).unwrap_or_default();
    let json = serde_json::json!({"title": title, "text": text, "elements": elements, "boxes": boxes});
    if let Err(error) = std::fs::write(path, json.to_string()) {
        eprintln!("lucinanet: dump: {error}");
    }
}

/// What innerText leaves out.
const NOT_TEXT: [&str; 7] = ["head", "script", "style", "noscript", "template", "title", "meta"];

fn walk(
    doc: &blitz_dom::BaseDocument,
    id: blitz_dom::NodeId,
    path: String,
    boxes: &mut Vec<serde_json::Value>,
    text: &mut String,
    elements: &mut usize,
) {
    let Some(node) = doc.get_node(id) else {
        return;
    };
    let layout = node.final_layout();
    let at = node.absolute_position(0.0, 0.0);
    let (x, y) = (at.x as f64, at.y as f64);
    let (w, h) = (layout.size.width as f64, layout.size.height as f64);
    let hidden = node
        .primary_styles()
        .is_some_and(|s| s.get_box().slow_clone_display().is_none());
    if boxes.len() < 4000 && w > 0.0 && h > 0.0 {
        boxes.push(serde_json::json!({"p": path, "x": x, "y": y, "w": w, "h": h}));
    }
    let mut seen = std::collections::HashMap::<String, usize>::new();
    for &child in &node.children {
        let Some(c) = doc.get_node(child) else {
            continue;
        };
        if let Some(element) = c.element_data() {
            *elements += 1;
            let tag = element.name.local.to_string();
            let n = seen.entry(tag.clone()).or_default();
            *n += 1;
            let child_path = format!("{path}/{tag}[{n}]");
            if hidden || NOT_TEXT.contains(&tag.as_str()) {
                continue;
            }
            walk(doc, child, child_path, boxes, text, elements);
            if matches!(tag.as_str(), "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "section" | "br") {
                text.push('\n');
            }
        } else if c.is_text_node() && !hidden {
            text.push_str(&c.text_content());
            text.push(' ');
        }
    }
}
