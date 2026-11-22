//! Video frames as YUV planes into WebRender (Servo patches 0055-0056, `lucinanet_media::surfaces`).
//!
//! Two ways in, both on the paint thread, where swgl's context is current:
//! - Servo's external image handler for the `Media` type ([`install`]): WebRender locks one plane when it draws the video into a
//!   picture tile itself (under a transform or a rounded clip, say). The plane comes back as a swgl texture over the picture's
//!   own bytes.
//! - The software compositor's `lock_composite_surface` ([`lock_surface`]), when WebRender made the video a compositor surface of
//!   its own: all three planes at once, and swgl converts and scales them straight into the window's frame.
//!
//! Either way the bytes are never copied: `set_texture_buffer` points a texture at them, and the picture stays pinned
//! (`surfaces::pin`) until WebRender unlocks it. `LUCINANET_MEDIA_COPY_PLANES=1` says copy instead ([`tight_copy`], [`Copies`]),
//! into a buffer of ours whose rows are exactly `w` apart and which outlives the unlock: the experiment for the scaled-video flaw.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};

use gleam::gl::{self, Gl};
use lucinanet_media::surfaces::{self, Planes, YuvMatrix};
use servo::media_images::{
    ExternalImageSource, WebRenderExternalImageApi, WebRenderExternalImageIdManager, WebRenderImageHandlerType,
};
use webrender::SWGLCompositeSurfaceInfo;
use webrender_api::units::DeviceIntSize;
use webrender_api::{ColorDepth, YuvRangedColorSpace};

thread_local! {
    static CONTEXT: Cell<Option<swgl::Context>> = const { Cell::new(None) };
    /// swgl textures by (locked id, plane).
    static TEXTURES: RefCell<HashMap<(u64, usize), u32>> = RefCell::new(HashMap::new());
}

/// Hands Servo the factory of LucinaNet's `Media` image handler, and so the ids `surfaces` gives players. Before Servo starts.
pub fn install() {
    servo::media_images::set_embedder_image_handler(Box::new(|ids: WebRenderExternalImageIdManager| {
        let ids = Mutex::new(ids);
        surfaces::set_allocator(surfaces::Allocator {
            next: Box::new(move || ids.lock().unwrap_or_else(|e| e.into_inner()).next_id(WebRenderImageHandlerType::Media).0),
        });
        Box::new(MediaImages) as Box<dyn WebRenderExternalImageApi>
    }));
}

/// The context the paint thread draws with (`SoftContext::new`).
pub fn set_context(gl: swgl::Context) {
    CONTEXT.with(|c| c.set(Some(gl)));
}

/// Says once, under `LUCINANET_MEDIA_TRACE`, which way WebRender took a video's planes: so a measurement knows its path.
fn first(which: usize, what: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SAID: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
    if std::env::var_os("LUCINANET_MEDIA_TRACE").is_some() && !SAID[which].swap(true, Ordering::Relaxed) {
        eprintln!("lucinanet-media: {what}");
    }
}

fn texture(gl: &swgl::Context, key: (u64, usize)) -> u32 {
    TEXTURES.with(|t| *t.borrow_mut().entry(key).or_insert_with(|| gl.gen_textures(1)[0]))
}

/// Whether planes are copied before upload: `LUCINANET_MEDIA_COPY_PLANES=1`, read once, off otherwise. Said once, when it turns on.
/// The experiment for the handover side of the scaled-video flaw: answered no on x86 2026-11-13 — the render is byte-identical with and
/// without it, so neither the pointer, nor its pitch, nor the copy's lifetime is what makes the scaled picture bars.
fn copy_planes() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var("LUCINANET_MEDIA_COPY_PLANES").as_deref() == Ok("1");
        if on {
            eprintln!("lucinanet-media: copying planes before upload (LUCINANET_MEDIA_COPY_PLANES=1)");
        }
        on
    })
}

/// `bytes` as a buffer of our own: `h` rows of `w` bytes, `w` apart, read from rows `stride` apart. A short input leaves the rest zero.
fn tight_copy(bytes: &[u8], w: usize, h: usize, stride: usize) -> Vec<u8> {
    let mut out = vec![0; w * h];
    for r in 0..h {
        let s = (r * stride).min(bytes.len());
        let n = w.min(bytes.len() - s);
        out[r * w..r * w + n].copy_from_slice(&bytes[s..s + n]);
    }
    out
}

/// What the textures point at under `LUCINANET_MEDIA_COPY_PLANES=1`: the last copy of every `(id, plane)` asked for.
#[derive(Default)]
struct Copies {
    /// By (locked id, plane): the copy its texture points at, and the lock that took it.
    held: HashMap<(u64, usize), (u64, Arc<Vec<u8>>)>,
    locks: u64,
}

/// Locks after which a key nobody asked for again is dropped, and the buffer count at which that is worth looking for.
const STALE: u64 = 4;
const MAX: usize = 12;

impl Copies {
    /// Makes `bytes` the copy of `key` and answers with it: the registry holds it as long as that texture may point at it, and the
    /// caller holds it across the upload. Nothing is dropped at `unlock` — a copy outlives the unpin that ends its lock, which is
    /// the point. A copy goes when its key locks again, which re-points that very texture on this thread, or, once the registry
    /// holds `MAX` buffers, after `STALE` locks that did not ask for it: a gone player stops locking and its ids are never given
    /// out again.
    fn hold(&mut self, key: (u64, usize), bytes: Vec<u8>) -> Arc<Vec<u8>> {
        self.locks += 1;
        let buf = Arc::new(bytes);
        if self.held.len() >= MAX && !self.held.contains_key(&key) {
            self.held.retain(|_, (seq, _)| *seq + STALE > self.locks);
        }
        self.held.insert(key, (self.locks, buf.clone()));
        buf
    }
}

fn copies() -> &'static Mutex<Copies> {
    static C: OnceLock<Mutex<Copies>> = OnceLock::new();
    C.get_or_init(Mutex::default)
}

/// Points the texture for `key` at plane `i` of the picture.
fn wrap(gl: &swgl::Context, key: (u64, usize), pic: &Planes, i: usize) -> (u32, i32, i32) {
    let (bytes, w, h, stride) = pic.plane(i);
    let tex = texture(gl, key);
    // The switch off leaves the picture's own bytes and its own pitch, and its pin holds them for the length of this call; on,
    // ours, tight at `w`, held by the registry past the unpin and by the local `Arc` across the upload.
    let (src, pitch, _copy) = match copy_planes() {
        true => {
            let copy = copies().lock().unwrap_or_else(|e| e.into_inner()).hold(key, tight_copy(bytes, w, h, stride));
            (copy.as_ptr(), w, Some(copy))
        },
        false => (bytes.as_ptr(), stride, None),
    };
    gl.set_texture_buffer(
        tex,
        gl::R8,
        w as i32,
        h as i32,
        pitch as i32,
        src as *mut c_void,
        w as i32,
        h as i32,
    );
    (tex, w as i32, h as i32)
}

struct MediaImages;

impl WebRenderExternalImageApi for MediaImages {
    fn lock(&mut self, id: u64) -> (ExternalImageSource<'_>, euclid::default::Size2D<i32>) {
        let Some(gl) = CONTEXT.with(|c| c.get()) else {
            return (ExternalImageSource::Invalid, euclid::default::Size2D::new(1, 1));
        };
        match surfaces::pin(id) {
            Some((pic, plane)) => {
                first(1, "video planes drawn into picture tiles (external image handler)");
                let (tex, w, h) = wrap(&gl, (id, 0), &pic, plane);
                (ExternalImageSource::NativeTexture(tex), euclid::default::Size2D::new(w, h))
            },
            // The player has gone and WebRender has not yet heard that its keys were deleted.
            None => (ExternalImageSource::Invalid, euclid::default::Size2D::new(1, 1)),
        }
    }

    fn unlock(&mut self, id: u64) {
        surfaces::unpin(id);
    }
}

/// `MappableCompositor::lock_composite_surface` for the Y id WebRender attached to a video's compositor surface.
pub fn lock_surface(ctx: *mut c_void, id: u64, info: &mut SWGLCompositeSurfaceInfo) -> bool {
    let gl = swgl::Context::from(ctx);
    let pic = match surfaces::pin(id) {
        Some((pic, 0)) => pic,
        Some(_) => {
            surfaces::unpin(id);
            return false;
        },
        // `pin` counted this lock, and WebRender unlocks only surfaces that locked.
        None => {
            surfaces::unpin(id);
            return false;
        },
    };
    for i in 0..3 {
        info.textures[i] = wrap(&gl, (id, i), &pic, i).0;
    }
    info.yuv_planes = 3;
    first(0, "video composited as a YUV surface (lock_composite_surface)");
    info.color_depth = ColorDepth::Color8;
    info.color_space = match pic.matrix {
        YuvMatrix::Bt601 => YuvRangedColorSpace::Rec601Narrow,
        YuvMatrix::Bt709 => YuvRangedColorSpace::Rec709Narrow,
        YuvMatrix::Bt2020 => YuvRangedColorSpace::Rec2020Narrow,
    };
    info.size = DeviceIntSize::new(pic.w as i32, pic.h as i32);
    true
}

pub fn unlock_surface(id: u64) {
    surfaces::unpin(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(n: usize) -> Vec<u8> {
        (0..n).map(|i| i as u8).collect()
    }

    #[test]
    fn tight_copy_of_a_tight_plane_is_the_same_bytes() {
        let src = bytes(12);
        assert_eq!(tight_copy(&src, 3, 4, 3), src);
        assert_eq!(tight_copy(&src, 12, 1, 12), src);
        assert_eq!(tight_copy(&[], 4, 2, 4), [0; 8]);
    }

    #[test]
    fn tight_copy_leaves_out_the_padding() {
        // Rows 4 bytes apart of which 2 are wanted: the padding does not come along.
        assert_eq!(tight_copy(&bytes(16), 2, 4, 4), [0, 1, 4, 5, 8, 9, 12, 13]);
        // Odd width, odder stride: 5 of every 7.
        assert_eq!(
            tight_copy(&bytes(21), 5, 3, 7),
            [0, 1, 2, 3, 4, 7, 8, 9, 10, 11, 14, 15, 16, 17, 18]
        );
    }

    #[test]
    fn tight_copy_of_a_short_plane_stays_zero() {
        // Asked for 3 rows of 5 out of 10 bytes: the third was never there, and nothing read past the end.
        assert_eq!(tight_copy(&bytes(10), 5, 3, 5), [vec![0u8, 1, 2, 3, 4], vec![5, 6, 7, 8, 9], vec![0; 5]].concat());
        assert_eq!(tight_copy(&bytes(3), 5, 3, 9), [0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_replaced_copy_stays_alive_for_whoever_holds_it() {
        let mut c = Copies::default();
        let one = c.hold((7, 0), vec![1; 4]);
        let two = c.hold((7, 0), vec![2; 4]);
        assert_eq!(c.held.len(), 1);
        assert!(Arc::ptr_eq(&c.held[&(7, 0)].1, &two));
        // What the old texture was pointed at is only freed with its last holder, never at the replace alone.
        assert_eq!(&*one, &[1; 4]);
        assert_eq!(Arc::strong_count(&one), 1);
    }

    #[test]
    fn copies_held_below_the_cap_are_never_dropped() {
        // A video's keys, locked for far more than STALE locks by others: all stay, because the cap was never reached.
        let mut c = Copies::default();
        let one = c.hold((7, 0), vec![1; 4]);
        for i in 0..MAX as u64 - 2 {
            c.hold((100 + i, 1), vec![i as u8; 4]);
        }
        assert!(c.held.contains_key(&(7, 0)));
        assert_eq!(Arc::strong_count(&one), 2);
    }

    #[test]
    fn copies_drop_a_key_that_stopped_being_locked() {
        let mut c = Copies::default();
        let gone = c.hold((7, 0), vec![1; 4]);
        for i in 0..MAX as u64 {
            c.hold((1000 + i, 0), vec![i as u8; 4]);
        }
        // The cap reached, so the last hold pruned: (7, 0) has not been asked for in STALE locks and is a gone player's.
        assert!(!c.held.contains_key(&(7, 0)));
        assert!(c.held.contains_key(&(1000 + MAX as u64 - 1, 0)));
        assert!(c.held.len() <= MAX);
        // And its bytes were still readable while something held them.
        assert_eq!(&*gone, &[1; 4]);
    }
}
