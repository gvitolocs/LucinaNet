//! Servo's `RenderingContext` without a GPU: WebRender's own software GL, `swgl`. Servo's painter only
//! talks to `gleam::gl::Gl`, and `swgl::Context` is one, so no Mesa, EGL or surfman surface is needed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use dpi::PhysicalSize;
use gleam::gl::Gl;
use image::RgbaImage;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use crate::rules::pacing;
use servo::{DeviceIntRect, RefreshDriver, RenderingContext};
use webrender::sw_compositor::SwCompositor;
use webrender::{
    ClipRadius, Compositor, CompositorCapabilities, CompositorConfig, CompositorSurfaceTransform, Device,
    MappableCompositor, MappedTileInfo, NativeSurfaceId, NativeSurfaceInfo, NativeTileId, SWGLCompositeSurfaceInfo,
    WindowVisibility,
};
use webrender_api::units::{DeviceIntPoint, DeviceIntSize};
use webrender_api::{ColorF, ExternalImageId, ImageRendering};

pub struct SoftContext {
    gl: swgl::Context,
    size: Cell<PhysicalSize<u32>>,
    pixels: RefCell<Vec<u32>>,
    /// `SwCompositor` leaves the frame top row first, GL drawing bottom row first.
    top_down: Cell<bool>,
    pacing: Arc<Pacing>,
}

impl SoftContext {
    pub fn new(size: PhysicalSize<u32>, pacing: Arc<Pacing>) -> Self {
        let gl = swgl::Context::create();
        gl.make_current();
        #[cfg(feature = "media")]
        crate::media_images::set_context(gl);
        let this = Self {
            gl,
            size: Cell::new(size),
            pixels: RefCell::new(Vec::new()),
            top_down: Cell::new(false),
            pacing,
        };
        this.bind();
        this
    }

    fn bind(&self) {
        let size = self.size.get();
        let mut pixels = self.pixels.borrow_mut();
        pixels.clear();
        pixels.resize(size.width as usize * size.height as usize, 0);
        self.gl.init_default_framebuffer(
            0,
            0,
            size.width as i32,
            size.height as i32,
            size.width as i32 * 4,
            pixels.as_mut_ptr().cast(),
        );
    }

    /// The finished frame: one BGRA u32 per pixel, top row first, and whether swgl left it that way.
    pub fn with_frame<R>(&self, f: impl FnOnce(&[u32], PhysicalSize<u32>, bool) -> R) -> R {
        self.gl.trace_frame();
        self.gl.flush();
        f(&self.pixels.borrow(), self.size.get(), self.top_down.get())
    }
}

impl RenderingContext for SoftContext {
    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        Some(Rc::new(PacedRefreshDriver::new(self.pacing.clone())))
    }

    fn compositor_config(&self) -> Option<CompositorConfig> {
        if std::env::var_os("LUCINANET_DRAW_COMPOSITOR").is_some() {
            return None;
        }
        self.top_down.set(true);
        let compositor = SwCompositor::new(self.gl, Box::new(NoNativeCompositor), false);
        Some(CompositorConfig::Native {
            compositor: Box::new(compositor),
        })
    }

    fn read_to_image(&self, rect: DeviceIntRect) -> Option<RgbaImage> {
        let size = self.size.get();
        let (x, y) = (rect.min.x.max(0) as usize, rect.min.y.max(0) as usize);
        let width = (rect.width().max(0) as usize).min((size.width as usize).checked_sub(x)?);
        let height = (rect.height().max(0) as usize).min((size.height as usize).checked_sub(y)?);
        self.gl.flush();
        let pixels = self.pixels.borrow();
        let mut out = Vec::with_capacity(width * height * 4);
        for row in 0..height {
            // The rectangle counts from the top, a GL buffer from the bottom.
            let from_bottom = size.height as usize - 1 - (y + row);
            let line = if self.top_down.get() { y + row } else { from_bottom } * size.width as usize;
            for px in &pixels[line + x..line + x + width] {
                let [b, g, r, a] = px.to_le_bytes();
                out.extend_from_slice(&[r, g, b, a]);
            }
        }
        RgbaImage::from_raw(width as u32, height as u32, out)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        self.size.set(size);
        self.bind();
    }

    fn present(&self) {
        self.gl.trace_frame();
        self.gl.flush();
    }

    fn make_current(&self) -> Result<(), surfman::Error> {
        self.gl.make_current();
        Ok(())
    }

    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        Rc::new(self.gl)
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        // Only servoshell's egui and OffscreenRenderingContext call this, and LucinaNet has neither.
        unreachable!("LucinaNet paints through gleam only")
    }
}

/// `SwCompositor` composites into the main framebuffer by itself when told there is no native compositor.
/// It still wants this object, and asks it only what kind of compositor it is.
struct NoNativeCompositor;

impl Compositor for NoNativeCompositor {
    fn create_surface(&mut self, _: &mut Device, _: NativeSurfaceId, _: DeviceIntPoint, _: DeviceIntSize, _: bool) {}
    fn create_external_surface(&mut self, _: &mut Device, _: NativeSurfaceId, _: bool) {}
    fn create_backdrop_surface(&mut self, _: &mut Device, _: NativeSurfaceId, _: ColorF) {}
    fn destroy_surface(&mut self, _: &mut Device, _: NativeSurfaceId) {}
    fn create_tile(&mut self, _: &mut Device, _: NativeTileId) {}
    fn destroy_tile(&mut self, _: &mut Device, _: NativeTileId) {}
    fn attach_external_image(&mut self, _: &mut Device, _: NativeSurfaceId, _: ExternalImageId) {}
    fn bind(&mut self, _: &mut Device, _: NativeTileId, _: DeviceIntRect, _: DeviceIntRect) -> NativeSurfaceInfo {
        unreachable!("SwCompositor binds its own tiles without a native compositor")
    }
    fn unbind(&mut self, _: &mut Device) {}
    fn begin_frame(&mut self, _: &mut Device) {}
    fn add_surface(
        &mut self,
        _: &mut Device,
        _: NativeSurfaceId,
        _: CompositorSurfaceTransform,
        _: DeviceIntRect,
        _: ImageRendering,
        _: DeviceIntRect,
        _: ClipRadius,
    ) {
    }
    fn end_frame(&mut self, _: &mut Device) {}
    fn enable_native_compositor(&mut self, _: &mut Device, _: bool) {}
    fn deinit(&mut self, _: &mut Device) {}
    fn get_capabilities(&self, _: &mut Device) -> CompositorCapabilities {
        CompositorCapabilities {
            redraw_on_invalidation: true,
            ..Default::default()
        }
    }
    fn get_window_visibility(&self, _: &mut Device) -> WindowVisibility {
        WindowVisibility::default()
    }
}

impl MappableCompositor for NoNativeCompositor {
    fn map_tile(&mut self, _: &mut Device, _: NativeTileId, _: DeviceIntRect, _: DeviceIntRect) -> Option<MappedTileInfo> {
        None
    }
    fn unmap_tile(&mut self, _: &mut Device) {}
    // A video WebRender made a compositor surface of its own: its YUV planes, which swgl converts and scales as it composites
    // (`media_images`). Without media there are none.
    #[allow(unused_variables)]
    fn lock_composite_surface(
        &mut self,
        _: &mut Device,
        ctx: *mut c_void,
        id: ExternalImageId,
        info: *mut SWGLCompositeSurfaceInfo,
    ) -> bool {
        // SAFETY: SwCompositor passes a live descriptor it owns for the call, or null.
        #[cfg(feature = "media")]
        if let Some(info) = unsafe { info.as_mut() } {
            return crate::media_images::lock_surface(ctx, id.0, info);
        }
        false
    }
    #[allow(unused_variables)]
    fn unlock_composite_surface(&mut self, _: &mut Device, _: *mut c_void, id: ExternalImageId) {
        #[cfg(feature = "media")]
        crate::media_images::unlock_surface(id.0);
    }
}

/// How often the page may start a frame, given what frames cost. Painting happens on the engine thread,
/// which also takes the input, so a page that animates must not keep it painting all the time.
#[derive(Default)]
pub struct Pacing {
    last_paint_us: AtomicU64,
    /// Milliseconds since `epoch` at the last input, plus one so that zero means none.
    last_input_ms: AtomicU64,
    epoch: OnceLock<Instant>,
    /// Set by input: the frame timer stops waiting.
    woken: Mutex<bool>,
    wake: Condvar,
    /// When the last frame started, and how many frames in a row came close together before it.
    last_frame: Mutex<Option<(Instant, u32)>>,
    /// Wakes the engine loop. A frame callback only queues the frame's work for the engine thread, which sleeps up
    /// to 250 ms between turns: without this wake, every scroll step on Gmail took two sleeps, 500 ms (2026-11-05).
    engine_wake: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl Pacing {
    fn now_ms(&self) -> u64 {
        self.epoch.get_or_init(Instant::now).elapsed().as_millis() as u64
    }

    pub fn painted(&self, took: Duration) {
        self.last_paint_us.store(took.as_micros() as u64, Ordering::Relaxed);
    }

    /// How the frame timer wakes the engine loop after it has started a frame. Set once.
    pub fn set_engine_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.engine_wake.set(Box::new(wake));
    }

    pub fn input(&self) {
        self.last_input_ms.store(self.now_ms() + 1, Ordering::Relaxed);
        *self.woken.lock().unwrap() = true;
        self.wake.notify_all();
    }

    fn input_is_recent(&self) -> bool {
        pacing::input_recent(self.now_ms(), self.last_input_ms.load(Ordering::Relaxed))
    }

    /// Waits for the next frame, or for input. Only a run of frames is paced (an animation): the first
    /// [`crate::rules::pacing::UNPACED_FRAMES`] of a run start at once, and after that the interval counts from the
    /// start of the last frame, not from the request. Pacing every frame from its request was 16-40 ms of each
    /// navigation's first paint on Juno (2026-11-05).
    fn wait_for_frame(&self) {
        let mut woken = self.woken.lock().unwrap();
        *woken = false;
        let last = *self.last_frame.lock().unwrap();
        let (wait, run) = match last {
            Some((at, run)) => {
                let since = at.elapsed().as_micros().min(u64::MAX as u128) as u64;
                let (wait, run) = pacing::frame_wait(Some(since), run, self.next_frame_in());
                (Duration::from_micros(wait), run)
            }
            None => (Duration::ZERO, 0),
        };
        if !wait.is_zero() {
            let (next, _) = self
                .wake
                .wait_timeout_while(woken, wait, |woken| !*woken)
                .unwrap();
            woken = next;
        }
        *woken = false;
        drop(woken);
        *self.last_frame.lock().unwrap() = Some((Instant::now(), run));
    }

    /// Input first. Otherwise at most half the time goes to painting: the timer starts with the paint.
    fn next_frame_in(&self) -> u64 {
        pacing::next_frame_us(self.input_is_recent(), self.last_paint_us.load(Ordering::Relaxed))
    }
}

/// Starts Servo's frames at the pace of [`Pacing`].
pub struct PacedRefreshDriver {
    pacing: Arc<Pacing>,
    frames: mpsc::Sender<Box<dyn Fn() + Send + 'static>>,
}

impl PacedRefreshDriver {
    pub fn new(pacing: Arc<Pacing>) -> Self {
        let (frames, waiting) = mpsc::channel::<Box<dyn Fn() + Send + 'static>>();
        let timing = pacing.clone();
        std::thread::Builder::new()
            .name("PaintTimerThread".into())
            .spawn(move || {
                while let Ok(first) = waiting.recv() {
                    timing.wait_for_frame();
                    first();
                    while let Ok(next) = waiting.try_recv() {
                        next();
                    }
                    if let Some(wake) = timing.engine_wake.get() {
                        wake();
                    }
                }
            })
            .expect("the frame timer thread");
        Self { pacing, frames }
    }
}

impl RefreshDriver for PacedRefreshDriver {
    fn observe_next_frame(&self, start_frame_callback: Box<dyn Fn() + Send + 'static>) {
        let _ = self.frames.send(start_frame_callback);
    }

    /// Animations always tick. Holding them for `INPUT_HOLD_MS` after input also held Servo's scroll animations: every
    /// second wheel step waited ~400 ms for the hold to end, with under 1 ms of raster in it (Wikipedia, 2026-11-05).
    /// CSS transform/opacity animations no longer rebuild the display list (compositor animations), so the hold
    /// bought little; frames are still paced by `wait_for_frame`.
    fn should_tick_animations(&self) -> bool {
        true
    }
}
