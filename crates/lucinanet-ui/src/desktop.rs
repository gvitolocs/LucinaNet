//! LucinaNet on a Linux desktop: the same window as on Juno, its frame shown through softbuffer, and the
//! desktop's keys and pointer turned into the toolkit's input. `lucinanet --desktop [address]`; the window
//! is `LUCINANET_SIZE` (`WxH`, physical pixels) or 1600x1000, and does not resize.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucina_app::app::Core;
use lucina_app::keys::{Mods, Press};
use lucina_app::ui::Input;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
#[cfg(not(target_os = "macos"))]
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::{Icon, Window, WindowId};

use crate::browser::{Browser, Msg};
use crate::{News, Start, Url};

/// Linux key codes of the modifiers: the toolkit gets them as state, not as presses.
const MODIFIER_CODES: [u32; 9] = [29, 42, 54, 56, 58, 97, 100, 125, 126];

/// The Linux (evdev) key code of a physical key, which is what the toolkit reads on Juno. On Linux winit's scan code
/// is that code already.
#[cfg(not(target_os = "macos"))]
fn key_code(key: winit::keyboard::PhysicalKey) -> Option<u32> {
    key.to_scancode()
}

/// On macOS winit's scan codes are Apple's virtual key codes, so the key is mapped by its position (`KeyCode`) to the
/// evdev code of the same key on a US keyboard.
#[cfg(target_os = "macos")]
fn key_code(key: winit::keyboard::PhysicalKey) -> Option<u32> {
    use winit::keyboard::{KeyCode as K, PhysicalKey};
    let PhysicalKey::Code(key) = key else { return None };
    Some(match key {
        K::Escape => 1,
        K::Digit1 => 2,
        K::Digit2 => 3,
        K::Digit3 => 4,
        K::Digit4 => 5,
        K::Digit5 => 6,
        K::Digit6 => 7,
        K::Digit7 => 8,
        K::Digit8 => 9,
        K::Digit9 => 10,
        K::Digit0 => 11,
        K::Minus => 12,
        K::Equal => 13,
        K::Backspace => 14,
        K::Tab => 15,
        K::KeyQ => 16,
        K::KeyW => 17,
        K::KeyE => 18,
        K::KeyR => 19,
        K::KeyT => 20,
        K::KeyY => 21,
        K::KeyU => 22,
        K::KeyI => 23,
        K::KeyO => 24,
        K::KeyP => 25,
        K::BracketLeft => 26,
        K::BracketRight => 27,
        K::Enter => 28,
        K::ControlLeft => 29,
        K::KeyA => 30,
        K::KeyS => 31,
        K::KeyD => 32,
        K::KeyF => 33,
        K::KeyG => 34,
        K::KeyH => 35,
        K::KeyJ => 36,
        K::KeyK => 37,
        K::KeyL => 38,
        K::Semicolon => 39,
        K::Quote => 40,
        K::Backquote => 41,
        K::ShiftLeft => 42,
        K::Backslash => 43,
        K::KeyZ => 44,
        K::KeyX => 45,
        K::KeyC => 46,
        K::KeyV => 47,
        K::KeyB => 48,
        K::KeyN => 49,
        K::KeyM => 50,
        K::Comma => 51,
        K::Period => 52,
        K::Slash => 53,
        K::ShiftRight => 54,
        K::NumpadMultiply => 55,
        K::AltLeft => 56,
        K::Space => 57,
        K::CapsLock => 58,
        K::F1 => 59,
        K::F2 => 60,
        K::F3 => 61,
        K::F4 => 62,
        K::F5 => 63,
        K::F6 => 64,
        K::F7 => 65,
        K::F8 => 66,
        K::F9 => 67,
        K::F10 => 68,
        K::NumLock => 69,
        K::Numpad7 => 71,
        K::Numpad8 => 72,
        K::Numpad9 => 73,
        K::NumpadSubtract => 74,
        K::Numpad4 => 75,
        K::Numpad5 => 76,
        K::Numpad6 => 77,
        K::NumpadAdd => 78,
        K::Numpad1 => 79,
        K::Numpad2 => 80,
        K::Numpad3 => 81,
        K::Numpad0 => 82,
        K::NumpadDecimal => 83,
        K::IntlBackslash => 86,
        K::F11 => 87,
        K::F12 => 88,
        K::NumpadEnter => 96,
        K::ControlRight => 97,
        K::NumpadDivide => 98,
        K::AltRight => 100,
        K::Home => 102,
        K::ArrowUp => 103,
        K::PageUp => 104,
        K::ArrowLeft => 105,
        K::ArrowRight => 106,
        K::End => 107,
        K::ArrowDown => 108,
        K::PageDown => 109,
        K::Insert => 110,
        K::Delete | K::Fn => 111,
        K::SuperLeft => 125,
        K::SuperRight => 126,
        _ => return None,
    })
}

/// The room the bar keeps free at its left end for a Mac window's three buttons (DESIGN.md, Geometry:
/// "Reserve 78 wide for them; the first control starts at x = 78"), in dp.
pub const MACOS_LEFT_INSET: f32 = 78.0;

/// The left inset for a build: 78 dp where the system draws window buttons over the bar, 0 where it
/// does not. `macos` is `cfg!(target_os = "macos")`, injected so both arms are tested on one host.
pub fn left_inset_for(macos: bool) -> f32 {
    if macos { MACOS_LEFT_INSET } else { 0.0 }
}

/// This build's left inset, in dp: 78 on macOS, 0 elsewhere (see [`left_inset_for`]).
pub fn macos_left_inset() -> f32 {
    left_inset_for(cfg!(target_os = "macos"))
}

/// The left inset a window gives the bar: `LUCINANET_CHROME_LEFT_INSET` through `chrome::parse_inset`
/// when the environment holds it (so an override parses exactly as it does for `chrome::Settings`,
/// junk and negatives both 0), and otherwise [`macos_left_inset`] (the same shape as
/// `chrome::parse_mode`, which falls back to the platform default). Pure: the environment value and
/// the platform come in.
pub fn window_left_inset(env_value: Option<&str>, macos: bool) -> f32 {
    match env_value {
        Some(value) => crate::chrome::parse_inset(Some(value)),
        None => left_inset_for(macos),
    }
}

/// The window's own mark, compiled in so a desktop build carries it without a file on disk: the
/// approved 128x128 PNG under assets/logo, whose README is the note on that art. It is decoded once,
/// at window creation, and never on a paint.
const WINDOW_ICON_PNG: &[u8] = include_bytes!("../../../assets/logo/lucinanet-128.png");

/// The frame `png` hands back — any color type, any sample width — as the RGBA8 rows winit's
/// [`Icon`] wants. Gray is repeated into the three color channels and a missing alpha becomes opaque,
/// so a mark saved without an alpha channel is still the mark. `None` for a shape that cannot be
/// read, which [`icon_rgba`] turns into a reason rather than a panic.
fn rgba8(frame: &[u8], out: &png::OutputInfo) -> Option<Vec<u8>> {
    let bytes_per_sample = match out.bit_depth {
        png::BitDepth::Eight => 1,
        png::BitDepth::Sixteen => 2,
        _ => return None,
    };
    let stride = out.color_type.samples().checked_mul(bytes_per_sample)?;
    let count = usize::try_from(out.width).ok()?.checked_mul(out.height as usize)?;
    // A 16-bit sample is read as its high byte, the same truncation `STRIP_16` applies.
    let pixels = frame.get(..count.checked_mul(stride)?)?;
    let mut rgba = Vec::with_capacity(count * 4);
    for px in pixels.chunks_exact(stride) {
        let sample = |i: usize| px[i * bytes_per_sample];
        let (r, g, b, a) = match out.color_type.samples() {
            1 => (sample(0), sample(0), sample(0), 255),
            2 => (sample(0), sample(0), sample(0), sample(1)),
            3 => (sample(0), sample(1), sample(2), 255),
            4 => (sample(0), sample(1), sample(2), sample(3)),
            _ => return None,
        };
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    Some(rgba)
}

/// A PNG's width and height and its pixels as RGBA8, or the reason they could not be read. Pure: the
/// bytes come in, so a test can hand it the asset, a fixture of its own, or junk. `png` is told to
/// bring every sample down to 8 bits and to add an alpha channel (`normalize_to_color8 | ALPHA`, where
/// `ALPHA` implies `EXPAND`), which is what leaves only gray+alpha and RGBA for [`rgba8`] to map; it
/// maps whatever it is given all the same.
fn icon_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(|error| format!("not a readable PNG: {error}"))?;
    let Some(size) = reader.output_buffer_size() else {
        return Err("its frame does not fit in memory".to_string());
    };
    let mut frame = vec![0u8; size];
    let out = reader.next_frame(&mut frame).map_err(|error| format!("its pixels would not decode: {error}"))?;
    let (Some(width), Some(height)) = (usize::try_from(out.width).ok(), usize::try_from(out.height).ok()) else {
        return Err("its dimensions do not fit this machine".to_string());
    };
    if width == 0 || height == 0 {
        return Err(format!("the picture is {}x{}", out.width, out.height));
    }
    match rgba8(&frame, &out) {
        Some(rgba) => Ok((out.width, out.height, rgba)),
        None => Err(format!("its {}x{} {}-bit frame is not pixels RGBA can be built from", out.width, out.height, out.bit_depth as u8)),
    }
}

/// This build's window icon, or `None` with the reason printed. An asset that somehow cannot be read
/// costs the window its icon and nothing else: `resumed` hands this straight to `with_window_icon`,
/// which is what a build with no icon at all passes, so the window opens and paints as it did
/// without one. macOS ignores a window icon and takes the app bundle's, packaged elsewhere.
fn window_icon() -> Option<Icon> {
    match embedded_icon() {
        Ok(icon) => Some(icon),
        Err(reason) => {
            eprintln!("lucinanet: no window icon: {reason}");
            None
        },
    }
}

/// The compiled-in mark, decoded and normalized into an [`Icon`], or why it is not one.
fn embedded_icon() -> Result<Icon, String> {
    let (width, height, rgba) = icon_rgba(WINDOW_ICON_PNG)?;
    Icon::from_rgba(rgba, width, height).map_err(|reason| format!("winit refused the {width}x{height} RGBA buffer: {reason}"))
}

struct Host {
    proxy: Arc<Mutex<EventLoopProxy<News>>>,
    first: Url,
    start: Start,
    size: (u32, u32),
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    core: Option<Core<Browser>>,
    mods: ModifiersState,
    pointer: (i32, i32),
}

pub fn run(first: Url, start: Start) -> ! {
    let size = std::env::var("LUCINANET_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1600, 1000));
    let event_loop = EventLoop::<News>::with_user_event().build().expect("a desktop to open a window on");
    let mut host = Host {
        proxy: Arc::new(Mutex::new(event_loop.create_proxy())),
        first,
        start,
        size,
        window: None,
        surface: None,
        core: None,
        mods: ModifiersState::empty(),
        pointer: (0, 0),
    };
    if let Err(error) = event_loop.run_app(&mut host) {
        eprintln!("lucinanet: {error}");
    }
    crate::exit_now()
}

impl Host {
    fn input(&mut self, event_loop: &ActiveEventLoop, input: Input) {
        let Some(core) = self.core.as_mut() else { return };
        core.input(input);
        self.after(event_loop);
    }

    fn after(&mut self, event_loop: &ActiveEventLoop) {
        let Some(core) = self.core.as_ref() else { return };
        if core.quit_requested() {
            event_loop.exit();
        } else if core.needs_paint()
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    fn paint(&mut self) {
        let (Some(core), Some(surface)) = (self.core.as_mut(), self.surface.as_mut()) else {
            return;
        };
        let mut steps = crate::frametimeline::WinSteps::begin();
        core.paint();
        steps.drawn();
        let Ok(mut buffer) = surface.buffer_mut() else { return };
        // Both are 0x00RRGGBB, top row first.
        let n = buffer.len().min(core.frame.px.len());
        buffer[..n].copy_from_slice(&core.frame.px[..n]);
        steps.uploaded();
        let _ = buffer.present();
        steps.swapped();
    }
}

impl ApplicationHandler<News> for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attributes = Window::default_attributes().with_title("LucinaNet").with_resizable(false);
        // The mark, decoded once here rather than on every paint. A `None` — an asset that could not be
        // read — is what winit is handed when there is no icon at all, so the window still opens.
        attributes = attributes.with_window_icon(window_icon());
        // On a Retina Mac an unset size is 1280x800 points, so the frame (always the window's physical size) is sharp
        // and the toolkit's scale follows from its pixels; `LUCINANET_SIZE` stays physical pixels everywhere.
        attributes = if cfg!(target_os = "macos") && std::env::var_os("LUCINANET_SIZE").is_none() {
            attributes.with_inner_size(LogicalSize::new(1280.0, 800.0))
        } else {
            attributes.with_inner_size(PhysicalSize::new(self.size.0, self.size.1))
        };
        // macOS: no native title row. The tab strip starts at the very top and the three window buttons float over its left
        // end, as in Safari and Chrome; decorations stay on, so the shadow, corners and buttons remain.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes = attributes
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true)
                // The buttons stay: they are the bar's left end, not something to hide under it.
                .with_titlebar_buttons_hidden(false);
            // Test runs: open without taking the keyboard focus or activating the app.
            if std::env::var_os("LUCINANET_NO_ACTIVATE").is_some() {
                attributes = attributes.with_active(false);
            }
        }
        let window = Rc::new(event_loop.create_window(attributes).expect("a window"));
        let PhysicalSize { width, height } = window.inner_size();
        let context = softbuffer::Context::new(window.clone()).expect("a software surface");
        let mut surface = softbuffer::Surface::new(&context, window.clone()).expect("a software surface");
        let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            event_loop.exit();
            return;
        };
        surface.resize(w, h).expect("a surface the window's size");
        let proxy = self.proxy.clone();
        let tell = Arc::new(move |news: News| {
            let _ = proxy.lock().unwrap().send_event(news);
        });
        // The one binding, `mut` for the macOS arm below: the inset goes on the value `Core::new` gets,
        // not a copy of it into the block. The allow keeps every other host, which never mutates it.
        #[allow(unused_mut)]
        let mut browser = Browser::with_news(tell, self.first.clone(), self.start);
        // The row's left end is under those buttons: one reserve, in dp, and `onebar.rs` scales it with the rest,
        // so neither chrome puts a control left of x = 78.
        #[cfg(target_os = "macos")]
        {
            let env = std::env::var("LUCINANET_CHROME_LEFT_INSET").ok();
            browser.set_left_inset(window_left_inset(env.as_deref(), cfg!(target_os = "macos")));
        }
        self.core = Some(Core::new(browser, width, height));
        crate::frametimeline::window_opened(width, height, window.scale_factor() as f32, window.current_monitor().and_then(|m| m.refresh_rate_millihertz()));
        self.surface = Some(surface);
        window.request_redraw();
        self.window = Some(window);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, news: News) {
        if let Some(core) = self.core.as_mut() {
            core.message(Msg::News(news));
        }
        self.after(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.paint(),
            WindowEvent::ModifiersChanged(mods) => self.mods = mods.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let Some(code) = key_code(event.physical_key) else { return };
                if MODIFIER_CODES.contains(&code) {
                    return;
                }
                let mods = Mods {
                    shift: self.mods.shift_key(),
                    // On a Mac, Cmd plays the role Ctrl has in the browser's and the toolkit's shortcuts (Cmd+L, Cmd+C, Cmd+V).
                    ctrl: self.mods.control_key() || (cfg!(target_os = "macos") && self.mods.super_key()),
                    alt: self.mods.alt_key(),
                    meta: self.mods.super_key() && !cfg!(target_os = "macos"),
                    ..Mods::default()
                };
                self.input(event_loop, Input::Key(Press { code: code as u16, mods }));
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (position.x as i32, position.y as i32);
                self.input(event_loop, Input::Move(self.pointer.0, self.pointer.1));
            },
            WindowEvent::MouseInput { state, button, .. } => {
                let (x, y) = self.pointer;
                // macOS: a press on empty tab-strip area moves the window (the release never comes, so the toolkit gets neither).
                #[cfg(target_os = "macos")]
                if button == MouseButton::Left
                    && state == ElementState::Pressed
                    && self.core.as_ref().is_some_and(|core| core.app.drags_window(x, y))
                {
                    if let Some(window) = &self.window {
                        let _ = window.drag_window();
                    }
                    return;
                }
                let input = match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => Input::Down(x, y),
                    (MouseButton::Left, ElementState::Released) => Input::Up(x, y),
                    (MouseButton::Right, ElementState::Pressed) => Input::Secondary(x, y),
                    _ => return,
                };
                self.input(event_loop, input);
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
                    MouseScrollDelta::PixelDelta(p) => (p.y / 40.0).round() as i32,
                };
                if notches != 0 {
                    self.input(event_loop, Input::Wheel(notches));
                }
            },
            _ => {},
        }
    }
}

/// How long after a press on the drag region a second one still counts as a double-click.
const DOUBLE_CLICK_TIME: Duration = Duration::from_millis(500);
/// How far apart, in logical points, those two presses may be and still count.
const DOUBLE_CLICK_SLOP: f32 = 4.0;

/// Two presses on the bar's drag region are a double-click, which zooms the window instead of moving
/// it, when the second comes within 500 ms of the first and within 4 points of it (both edges
/// included; later or further is a first press of its own). Pure: the clock and the positions come in,
/// so one host tests both platforms' rule.
///
/// Nothing calls this yet: piece 3 wires it, in `Host::window_event` (this module) at the
/// `WindowEvent::MouseInput` arm, for a left `ElementState::Pressed` on the DragRegion that
/// `Browser::drags_window` (browser.rs) reports — the region piece 1 lays out in onebar.rs. `Host`
/// then holds the last such press as `Option<(Instant, (f32, f32))>`, `Instant` this call and the
/// position in logical points from `WindowEvent::CursorMoved`; that field and the call are piece 3's,
/// not added here. When this says true the window zooms rather than drags:
/// `window.set_maximized(!window.is_maximized())` — winit 0.30 has no `toggle_maximize`.
pub fn is_double_click(prev: Option<(Instant, (f32, f32))>, now: Instant, pos: (f32, f32)) -> bool {
    let Some((then, then_pos)) = prev else { return false };
    if now.saturating_duration_since(then) > DOUBLE_CLICK_TIME {
        return false;
    }
    (pos.0 - then_pos.0).abs() <= DOUBLE_CLICK_SLOP && (pos.1 - then_pos.1).abs() <= DOUBLE_CLICK_SLOP
}

/// Hand the window to the platform, to move it with a press held on the bar's drag region. The release
/// never comes after `drag_window` starts, so the caller must not pass the press to the toolkit too.
///
/// Nothing calls this yet: piece 3 replaces the inline `window.drag_window()` in `Host::window_event`
/// (this module, the `WindowEvent::MouseInput` arm) with a call here, having first asked
/// [`is_double_click`] the same thing — true means zoom with `window.set_maximized` instead. The
/// "is this the DragRegion" answer comes from `Browser::drags_window` (browser.rs) over the layout
/// piece 1 writes in onebar.rs.
#[cfg(target_os = "macos")]
pub fn begin_window_drag(window: &Window) {
    let _ = window.drag_window();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_mark_is_the_asset_and_winit_takes_its_pixels() {
        let (width, height, rgba) = icon_rgba(WINDOW_ICON_PNG).expect("the embedded asset decodes");
        assert_eq!((width, height), (128, 128), "assets/logo/lucinanet-128.png is the 128x128 mark");
        assert_eq!(rgba.len(), 128 * 128 * 4, "one RGBA8 pixel per picture pixel");
        // The buffer this build hands the window manager: those pixels, and the asset reached whole.
        Icon::from_rgba(rgba, width, height).expect("winit takes the decoded buffer as an icon");
        assert!(window_icon().is_some(), "the embedded mark reaches the window");
    }

    #[test]
    fn the_embedded_mark_actually_carries_the_approved_art() {
        let (_, _, rgba) = icon_rgba(WINDOW_ICON_PNG).expect("the embedded asset decodes");
        let pixels: Vec<[u8; 4]> = rgba.chunks_exact(4).map(|px| [px[0], px[1], px[2], px[3]]).collect();
        let seeable: Vec<&[u8; 4]> = pixels.iter().filter(|px| px[3] != 0).collect();
        assert!(seeable.len() > 1_000, "only {} of the {} pixels hold any ink", seeable.len(), pixels.len());
        // The violet mark, as the owner approved it: blue leads the mean of the seeable pixels by a
        // margin, which a flat gray or black placeholder would not. (The file measures 161/173/231.)
        let mean = |channel: usize| seeable.iter().map(|px| usize::from(px[channel])).sum::<usize>() / seeable.len();
        let (red, green, blue) = (mean(0), mean(1), mean(2));
        assert!(blue > red + 20 && blue > green + 20, "blue {blue} does not lead red {red} and green {green}, so this is not the violet mark");
        assert!(pixels.iter().collect::<std::collections::HashSet<_>>().len() > 8, "an antialiased mark is not a flat block of one color");
        assert_eq!(pixels[0][3], 0, "the corner of the mark is transparent, so the window's rounded frame shows through");
    }

    #[test]
    fn a_mark_saved_in_gray_is_still_read_as_rgba() {
        // The normalizer's own job: `png` brings this out as gray+alpha, and the icon wants RGBA with
        // the gray in all three channels and the pixel opaque.
        let mut file = Vec::new();
        let mut encoder = png::Encoder::new(&mut file, 3, 2);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("a PNG header");
        writer.write_image_data(&[0, 64, 128, 192, 255, 32]).expect("a writable PNG");
        writer.finish().expect("a finished PNG");
        let (width, height, rgba) = icon_rgba(&file).expect("the gray PNG decodes");
        assert_eq!((width, height), (3, 2));
        assert_eq!(rgba, [0, 0, 0, 255, 64, 64, 64, 255, 128, 128, 128, 255, 192, 192, 192, 255, 255, 255, 255, 255, 32, 32, 32, 255]);
    }

    #[test]
    fn bytes_that_are_not_the_asset_are_reported_and_cost_only_the_icon() {
        for junk in ["".as_bytes(), b"not a PNG at all".as_slice(), &[0x89, b'P', b'N', b'G'][..]] {
            let reason = icon_rgba(junk).expect_err("bytes that hold no frame cannot make an icon");
            assert!(reason.contains("PNG"), "{reason} does not say the asset was the problem");
        }
    }

    #[test]
    fn the_left_inset_is_the_button_reserve_on_a_mac_and_nothing_elsewhere() {
        assert_eq!(MACOS_LEFT_INSET, 78.0, "DESIGN.md Geometry: the first control starts at x = 78");
        for macos in [false, true] {
            assert_eq!(left_inset_for(macos), if macos { MACOS_LEFT_INSET } else { 0.0 });
        }
        assert_eq!(macos_left_inset(), left_inset_for(cfg!(target_os = "macos")), "this build's own");
    }

    #[test]
    fn an_explicit_inset_wins_and_only_an_unset_one_is_the_platform_default() {
        for macos in [false, true] {
            assert_eq!(window_left_inset(None, macos), left_inset_for(macos));
            assert_eq!(window_left_inset(Some("40"), macos), 40.0, "an override wins on both platforms");
            // `chrome::parse_inset`'s own rules: a value that is not a usable number is 0, not the default.
            for junk in ["", "wide", "-5", "NaN", "inf"] {
                assert_eq!(window_left_inset(Some(junk), macos), 0.0, "{junk} is the env var's own not-a-number");
            }
            assert_eq!(window_left_inset(Some("1e9"), macos), crate::chrome::MAX_LEFT_INSET);
        }
    }

    #[test]
    fn two_presses_close_in_time_and_place_are_a_double_click() {
        let now = Instant::now();
        let at = |ms: u64, pos: (f32, f32)| Some((now - Duration::from_millis(ms), pos));
        assert!(is_double_click(at(400, (120.0, 22.0)), now, (122.0, 22.0)));
        assert!(is_double_click(at(0, (120.0, 22.0)), now, (120.0, 26.0)), "the edge: 4 points below");
        assert!(is_double_click(at(500, (120.0, 22.0)), now, (120.0, 22.0)), "the edge: 500 ms on");
    }

    #[test]
    fn a_press_late_far_or_alone_is_no_double_click() {
        let now = Instant::now();
        let at = |ms: u64, pos: (f32, f32)| Some((now - Duration::from_millis(ms), pos));
        assert!(!is_double_click(None, now, (120.0, 22.0)), "no first press");
        assert!(!is_double_click(at(501, (120.0, 22.0)), now, (120.0, 22.0)), "a ms late");
        assert!(!is_double_click(at(0, (120.0, 22.0)), now, (124.1, 22.0)), "a point wide");
        assert!(!is_double_click(at(0, (120.0, 22.0)), now, (120.0, 100.0)), "elsewhere on the bar");
    }
}
