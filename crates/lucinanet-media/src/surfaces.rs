/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pictures handed to WebRender as YUV planes (LucinaNet patches 0055-0056): no BGRA conversion on the player's thread, no copy
//! into Servo's shared memory and none into WebRender's texture cache. WebRender's software compositor converts and scales the
//! planes into the screen's frame while it composites, across its composite threads.
//!
//! Each player owns three external image ids, Y, U and V, of WebRender's `Media` handler type, taken from the id manager that the
//! embedder's handler factory hands over ([`set_allocator`]). The player publishes each picture it shows as packed I420
//! ([`Surface::publish`]). The paint thread pins the picture being composited ([`pin`]) and lets go of it after ([`unpin`]), so a
//! picture stays alive for as long as WebRender reads it, whatever the player has moved on to. Pins are counted per id and all of an
//! id's pictures stay until its last open lock is let go: the paint thread's texture for an id is shared by its locks, so it may
//! point at any of them. Verus checks this handover as a model (verification/handover/model.rs).
//!
//! Adopt: Gecko's software WebRender does video this way (`RenderCompositorSWGL::LockCompositeSurface`, buffers wrapped as swgl
//! textures with no copy). Without an allocator (no LucinaNet painter, the crate's tests) players keep the BGRA path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use lucina_video::convert::{Matrix, row};
use lucina_video_play::decode::Picture;
use servo_media_player::video::{Buffer, ExternalYuv, VideoFrame, VideoFrameData};
pub use servo_media_player::video::YuvMatrix;

/// Gives external image ids of the `Media` handler type. They are never given back: WebRender may still lock an id after its
/// player has gone (until the frame renderer's key deletions reach it), and Servo's handlers expect every id they are asked for
/// to be registered. Three ids a player is all it costs.
pub struct Allocator {
    pub next: Box<dyn Fn() -> u64 + Send + Sync>,
}

static ALLOCATOR: OnceLock<Allocator> = OnceLock::new();

/// Called once by the embedder's media image handler factory. Returns `false` if one was set already.
pub fn set_allocator(a: Allocator) -> bool {
    ALLOCATOR.set(a).is_ok()
}

/// Whether frames may go to WebRender as planes: an allocator is there and [`yuv_enabled`] says yes for
/// `LUCINANET_MEDIA_YUV`.
pub fn available() -> bool {
    ALLOCATOR.get().is_some() && yuv_enabled(std::env::var("LUCINANET_MEDIA_YUV").ok().as_deref())
}

/// Whether planes may be handed over: BGRA only for `LUCINANET_MEDIA_YUV=0`, and planes everywhere else (unset, `1`,
/// anything that is not `0`) on every platform.
///
/// Planes were off by default for flaw a tracked flaw — a scaled video came out as vertical colour bars, one source row
/// repeated over every output row, because the media planes' uv rect was handed to WebRender's software rasterizer
/// unflipped. The engine commit 40c0ff93c fixed that; `0` stays as the way to force BGRA frames when debugging.
fn yuv_enabled(env: Option<&str>) -> bool {
    !matches!(env, Some("0"))
}

/// One picture as packed I420: Y rows `w` apart, then U and V rows `cw` apart.
pub struct Planes {
    pub w: usize,
    pub h: usize,
    pub matrix: YuvMatrix,
    pub data: Vec<u8>,
}

impl Planes {
    pub fn chroma(&self) -> (usize, usize) {
        (self.w.div_ceil(2), self.h.div_ceil(2))
    }

    /// Plane `i` (0 = Y, 1 = U, 2 = V): its bytes, width, height and row length in bytes.
    pub fn plane(&self, i: usize) -> (&[u8], usize, usize, usize) {
        let (cw, ch) = self.chroma();
        let y = self.w * self.h;
        let c = cw * ch;
        match i {
            0 => (&self.data[..y], self.w, self.h, self.w),
            1 => (&self.data[y..y + c], cw, ch, cw),
            _ => (&self.data[y + c..y + 2 * c], cw, ch, cw),
        }
    }

    /// The picture as BGRA, as `frames.rs` makes it, for readers that want pixels (canvas `drawImage`, `texImage2D`).
    fn bgra(&self) -> Vec<u8> {
        let m = Matrix::for_size(self.w, self.h);
        let (y, u, v) = (self.plane(0).0, self.plane(1).0, self.plane(2).0);
        let cw = self.chroma().0;
        let mut out = vec![0; self.w * self.h * 4];
        let mut words = vec![0u32; self.w];
        for r in 0..self.h {
            let c = (r / 2) * cw;
            row(&m, &y[r * self.w..], &u[c..], &v[c..], &mut words);
            for (px, d) in words.iter().zip(out[r * self.w * 4..(r + 1) * self.w * 4].chunks_exact_mut(4)) {
                d.copy_from_slice(&[*px as u8, (*px >> 8) as u8, (*px >> 16) as u8, (*px >> 24) as u8]);
            }
        }
        out
    }
}

#[derive(Default)]
struct Registry {
    /// The picture each surface shows, by its Y id.
    current: HashMap<u64, Arc<Planes>>,
    /// Plane ids to their surface's Y id and the plane's index.
    planes: HashMap<u64, (u64, usize)>,
    /// By the id the paint thread locked them under: how many of its locks are open, and the pictures they were given.
    pinned: HashMap<u64, (usize, Vec<Arc<Planes>>)>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// A lock of `id`: the picture under it (any of a surface's three ids) and the plane `id` names. Every `pin` needs one [`unpin`]
/// with the same id, whether or not it found a picture: WebRender's tile path unlocks every lock it made.
pub fn pin(id: u64) -> Option<(Arc<Planes>, usize)> {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    let found = r.planes.get(&id).copied().and_then(|(y, plane)| r.current.get(&y).map(|pic| (pic.clone(), plane)));
    let (open, held) = r.pinned.entry(id).or_default();
    *open += 1;
    if let Some((pic, _)) = &found
        && !held.iter().any(|h| Arc::ptr_eq(h, pic))
    {
        held.push(pic.clone());
    }
    found
}

/// Lets go of one lock of `id`, and with its last open lock of every picture its locks were given. An unlock that let go of one
/// picture each (the first version) freed a picture an open lock's texture still pointed at, when a lock had found none.
pub fn unpin(id: u64) {
    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((open, _)) = r.pinned.get_mut(&id) {
        *open = open.saturating_sub(1);
        if *open == 0 {
            r.pinned.remove(&id);
        }
    }
}

/// A player's three ids, and the buffers its pictures are copied into.
pub struct Surface {
    ids: [u64; 3],
    pool: Vec<Arc<Planes>>,
}

/// Buffers kept: one composited, one published, one being filled.
const POOL: usize = 3;

impl Surface {
    /// `None` when frames cannot go to WebRender as planes ([`available`]).
    pub fn new() -> Option<Surface> {
        if !available() {
            return None;
        }
        let a = ALLOCATOR.get()?;
        let ids = [(a.next)(), (a.next)(), (a.next)()];
        let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
        for (i, id) in ids.iter().enumerate() {
            r.planes.insert(*id, (ids[0], i));
        }
        Some(Surface { ids, pool: Vec::new() })
    }

    fn buffer(&mut self, bytes: usize) -> Vec<u8> {
        if let Some(i) = self.pool.iter().position(|b| Arc::strong_count(b) == 1) {
            if let Ok(p) = Arc::try_unwrap(self.pool.swap_remove(i)) {
                let mut v = p.data;
                v.resize(bytes, 0);
                return v;
            }
        }
        vec![0; bytes]
    }

    /// Copies the picture into a buffer as packed I420 and makes it the one WebRender shows next. `None` above 8 bits.
    pub fn publish(&mut self, pic: &Picture) -> Option<VideoFrame> {
        let planes = match pic {
            Picture::Nv12(n) => {
                let (w, h) = (n.w, n.h);
                if w == 0 || h == 0 {
                    return None;
                }
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                let mut data = self.buffer(w * h + 2 * cw * ch);
                let (x0, y0) = n.crop;
                for r in 0..h {
                    let s = (y0 + r) * n.stride + x0;
                    data[r * w..(r + 1) * w].copy_from_slice(n.data.get(s..s + w)?);
                }
                let (u, v) = data[w * h..].split_at_mut(cw * ch);
                for r in 0..ch {
                    let s = n.uv_offset + (y0 / 2 + r) * n.stride + (x0 & !1);
                    let line = n.data.get(s..s + cw * 2)?;
                    for (i, uv) in line.chunks_exact(2).enumerate() {
                        u[r * cw + i] = uv[0];
                        v[r * cw + i] = uv[1];
                    }
                }
                Planes { w, h, matrix: matrix(w, h), data }
            },
            _ => {
                let p = pic.planes()?;
                let (w, h) = (p.w, p.h);
                if w == 0 || h == 0 {
                    return None;
                }
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                let mut data = self.buffer(w * h + 2 * cw * ch);
                for r in 0..h {
                    data[r * w..(r + 1) * w].copy_from_slice(p.y.get(r * p.y_stride..r * p.y_stride + w)?);
                }
                let (u, v) = data[w * h..].split_at_mut(cw * ch);
                for r in 0..ch {
                    let s = r * p.uv_stride;
                    u[r * cw..(r + 1) * cw].copy_from_slice(p.u.get(s..s + cw)?);
                    v[r * cw..(r + 1) * cw].copy_from_slice(p.v.get(s..s + cw)?);
                }
                Planes { w, h, matrix: matrix(w, h), data }
            },
        };
        let (w, h, m) = (planes.w, planes.h, planes.matrix);
        let planes = Arc::new(planes);
        if self.pool.len() >= POOL {
            self.pool.remove(0);
        }
        self.pool.push(planes.clone());
        registry().lock().unwrap_or_else(|e| e.into_inner()).current.insert(self.ids[0], planes.clone());
        let yuv = ExternalYuv { ids: self.ids, matrix: m, full_range: false };
        VideoFrame::new(w as i32, h as i32, Arc::new(Frame { yuv, planes }))
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
        r.current.remove(&self.ids[0]);
        for id in self.ids {
            r.planes.remove(&id);
        }
        // Pinned pictures stay with their pins until the paint thread lets go; a lock after this finds nothing and gets the
        // handler's empty texture.
    }
}

/// The matrix `lucina_video::convert::Matrix::for_size` picks: BT.709 above 576 lines, BT.601 at and below.
fn matrix(_w: usize, h: usize) -> YuvMatrix {
    if h > 576 { YuvMatrix::Bt709 } else { YuvMatrix::Bt601 }
}

/// A shown picture as Servo holds it: WebRender reads the planes through the ids; `bgra` is for readers that want pixels.
struct Frame {
    yuv: ExternalYuv,
    planes: Arc<Planes>,
}

impl Buffer for Frame {
    fn to_vec(&self) -> Option<VideoFrameData> {
        Some(VideoFrameData::ExternalYuv(self.yuv))
    }

    fn bgra(&self) -> Option<Arc<Vec<u8>>> {
        Some(Arc::new(self.planes.bgra()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(n: u8) -> Arc<Planes> {
        Arc::new(Planes { w: 2, h: 2, matrix: YuvMatrix::Bt601, data: vec![n; 6] })
    }

    #[test]
    fn pictures_stay_pinned_until_the_last_lock_of_their_id_is_let_go() {
        // Ids far from what the id manager gives, so a parallel test's surfaces do not meet them.
        let (y, u, v) = (u64::MAX - 3, u64::MAX - 2, u64::MAX - 1);
        let (one, two) = (picture(1), picture(2));
        {
            let mut r = registry().lock().unwrap();
            for (i, id) in [y, u, v].into_iter().enumerate() {
                r.planes.insert(id, (y, i));
            }
            r.current.insert(y, one.clone());
        }
        // A lock that finds picture one, a new picture, a lock that finds it, the player gone, a lock that finds nothing.
        assert!(pin(y).is_some_and(|(p, plane)| Arc::ptr_eq(&p, &one) && plane == 0));
        registry().lock().unwrap().current.insert(y, two.clone());
        assert!(pin(y).is_some_and(|(p, _)| Arc::ptr_eq(&p, &two)));
        {
            let mut r = registry().lock().unwrap();
            r.current.remove(&y);
            for id in [y, u, v] {
                r.planes.remove(&id);
            }
        }
        assert!(pin(y).is_none());
        // Two of the three unlocked, in any order: both pictures stay, as the first lock's texture may point at either.
        unpin(y);
        unpin(y);
        assert_eq!((Arc::strong_count(&one), Arc::strong_count(&two)), (2, 2));
        unpin(y);
        assert_eq!((Arc::strong_count(&one), Arc::strong_count(&two)), (1, 1));
        assert!(!registry().lock().unwrap().pinned.contains_key(&y));
    }

    #[test]
    fn yuv_enabled_is_the_whole_table() {
        // (LUCINANET_MEDIA_YUV, planes). Only `0` turns them off, and it does so on every platform; the unset default and
        // any other value are planes.
        let table = [(None, true), (Some("0"), false), (Some("1"), true), (Some(""), true), (Some("2"), true)];
        for (env, want) in table {
            assert_eq!(yuv_enabled(env), want, "LUCINANET_MEDIA_YUV {env:?}");
        }
    }

    #[test]
    fn planes_split_as_i420() {
        let p = Planes { w: 5, h: 3, matrix: YuvMatrix::Bt601, data: (0..5 * 3 + 2 * 3 * 2).map(|i| i as u8).collect() };
        assert_eq!(p.chroma(), (3, 2));
        let (y, w, h, s) = p.plane(0);
        assert_eq!((y.len(), w, h, s), (15, 5, 3, 5));
        let (u, w, h, s) = p.plane(1);
        assert_eq!((u[0], u.len(), w, h, s), (15, 6, 3, 2, 3));
        let (v, ..) = p.plane(2);
        assert_eq!((v[0], v.len()), (21, 6));
    }

    #[test]
    fn bgra_of_planes_matches_the_bgra_path() {
        // Mid grey with no colour: the same formula as frames.rs gives the same bytes.
        let p = Planes { w: 4, h: 2, matrix: YuvMatrix::Bt601, data: [vec![126; 8], vec![128; 4]].concat() };
        let out = p.bgra();
        assert_eq!(out.len(), 32);
        assert!(out.chunks_exact(4).all(|px| px[0] == px[1] && px[1] == px[2] && px[3] == 255), "{out:?}");
    }
}
