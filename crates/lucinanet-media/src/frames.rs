/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A decoded picture as the frame Servo paints: BGRA, as `media_frame_renderer.rs` hands it to WebRender (`ImageFormat::BGRA8`).
//!
//! The conversion is the Video app's (`lucina_video::convert::row`: one integer formula, NEON on the laptop, BT.709 above 576 lines
//! and BT.601 below), a row at a time at the picture's own size; WebRender scales. NV12 from the video block is split a row at a time
//! into the planar rows that function reads. Frame buffers are reused once Servo has let go of them, so a film does not allocate 8 MB a
//! frame.
//!
//! Adapt: GStreamer's backend converts in `videoconvert` to BGRA too (`render-unix` without GL). Handing WebRender YUV planes
//! (`YuvImage`, which swgl composites itself) is the next step and needs a Servo patch (patch C in docs/media.md).

use std::sync::Arc;

use lucina_video::convert::{Matrix, row};
use lucina_video_play::decode::{Nv12Picture, Picture};
use servo_media_player::video::{Buffer, VideoFrame, VideoFrameData};

struct Bgra(Arc<Vec<u8>>);

impl Buffer for Bgra {
    fn to_vec(&self) -> Option<VideoFrameData> {
        Some(VideoFrameData::Raw(self.0.clone()))
    }
}

/// Converts pictures into frames, reusing their memory.
#[derive(Default)]
pub struct Frames {
    pool: Vec<Arc<Vec<u8>>>,
    row: Vec<u32>,
    u: Vec<u8>,
    v: Vec<u8>,
}

/// Buffers kept: one on screen, one being painted, one being made.
const POOL: usize = 3;

impl Frames {
    fn buffer(&mut self, bytes: usize) -> Vec<u8> {
        // A buffer nobody else holds any more can be written again.
        if let Some(i) = self.pool.iter().position(|b| Arc::strong_count(b) == 1) {
            let b = self.pool.swap_remove(i);
            if let Ok(mut v) = Arc::try_unwrap(b) {
                v.resize(bytes, 0);
                return v;
            }
        }
        vec![0; bytes]
    }

    fn finish(&mut self, w: usize, h: usize, data: Vec<u8>) -> Option<VideoFrame> {
        let data = Arc::new(data);
        if self.pool.len() < POOL {
            self.pool.push(data.clone());
        } else {
            // Keep the newest; the oldest will be dropped by whoever holds it.
            self.pool.remove(0);
            self.pool.push(data.clone());
        }
        VideoFrame::new(w as i32, h as i32, Arc::new(Bgra(data)))
    }

    /// `None` above 8 bits, which the screen path does not read.
    pub fn convert(&mut self, pic: &Picture) -> Option<VideoFrame> {
        if let Picture::Nv12(n) = pic {
            return self.nv12(n);
        }
        let p = pic.planes()?;
        let (w, h) = (p.w, p.h);
        if w == 0 || h == 0 {
            return None;
        }
        let m = Matrix::for_size(w, h);
        let mut out = self.buffer(w * h * 4);
        self.row.resize(w, 0);
        for r in 0..h {
            let y = &p.y[r * p.y_stride..];
            let c = (r / 2) * p.uv_stride;
            row(&m, y, &p.u[c..], &p.v[c..], &mut self.row);
            put_row(&self.row, &mut out[r * w * 4..(r + 1) * w * 4]);
        }
        self.finish(w, h, out)
    }

    fn nv12(&mut self, n: &Nv12Picture) -> Option<VideoFrame> {
        let (w, h) = (n.w, n.h);
        if w == 0 || h == 0 {
            return None;
        }
        let m = Matrix::for_size(w, h);
        let half = w.div_ceil(2);
        let mut out = self.buffer(w * h * 4);
        self.row.resize(w, 0);
        self.u.resize(half, 0);
        self.v.resize(half, 0);
        let (x0, y0) = n.crop;
        for r in 0..h {
            if r % 2 == 0 {
                let c = n.uv_offset + ((y0 + r) / 2) * n.stride + (x0 & !1);
                let line = n.data.get(c..c + half * 2)?;
                for (i, uv) in line.chunks_exact(2).enumerate() {
                    self.u[i] = uv[0];
                    self.v[i] = uv[1];
                }
            }
            let y = n.data.get((y0 + r) * n.stride + x0..)?;
            row(&m, y, &self.u, &self.v, &mut self.row);
            put_row(&self.row, &mut out[r * w * 4..(r + 1) * w * 4]);
        }
        self.finish(w, h, out)
    }
}

/// The picture as raw I420 cropped to its size, as `ffmpeg -f rawvideo -pix_fmt yuv420p` writes it: for comparing with FFmpeg.
pub fn write_i420(pic: &Picture, out: &mut impl std::io::Write) -> std::io::Result<()> {
    if let Picture::Nv12(n) = pic {
        return write_i420(&n.to_i420(), out);
    }
    let Some(p) = pic.planes() else { return Ok(()) };
    for r in 0..p.h {
        out.write_all(&p.y[r * p.y_stride..r * p.y_stride + p.w])?;
    }
    let (cw, ch) = (p.w.div_ceil(2), p.h.div_ceil(2));
    for plane in [p.u, p.v] {
        for r in 0..ch {
            out.write_all(&plane[r * p.uv_stride..r * p.uv_stride + cw])?;
        }
    }
    Ok(())
}

/// `0xAARRGGBB` words to B, G, R, A bytes.
fn put_row(src: &[u32], dst: &mut [u8]) {
    for (px, out) in src.iter().zip(dst.chunks_exact_mut(4)) {
        out.copy_from_slice(&px.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planes(w: usize, h: usize, y: u8, u: u8, v: u8) -> Picture {
        Picture::Owned {
            pts_us: 0,
            w,
            h,
            y: vec![y; w * h],
            u: vec![u; w.div_ceil(2) * h.div_ceil(2)],
            v: vec![v; w.div_ceil(2) * h.div_ceil(2)],
            y_stride: w,
            uv_stride: w.div_ceil(2),
        }
    }

    #[test]
    fn white_black_and_red_in_bgra() {
        let mut f = Frames::default();
        let white = f.convert(&planes(4, 2, 235, 128, 128)).unwrap();
        // The Video app's formula has 6 bits after the point: video white (235) is 253, as on its screen.
        assert_eq!(&white.get_data()[..4], &[253, 253, 253, 255]);
        let black = f.convert(&planes(4, 2, 16, 128, 128)).unwrap();
        assert_eq!(&black.get_data()[..4], &[0, 0, 0, 255]);
        // BT.601 red: Y 81, Cb 90, Cr 240.
        let red = f.convert(&planes(4, 2, 81, 90, 240)).unwrap();
        let d = red.get_data();
        assert!(d[2] > 240 && d[1] < 10 && d[0] < 10, "{:?}", &d[..4]);
    }

    #[test]
    fn nv12_matches_the_planar_conversion() {
        let (w, h) = (6, 4);
        let y: Vec<u8> = (0..w * h).map(|i| (16 + i * 7) as u8).collect();
        let u: Vec<u8> = (0..3 * 2).map(|i| (100 + i * 9) as u8).collect();
        let v: Vec<u8> = (0..3 * 2).map(|i| (200 - i * 11) as u8).collect();
        let planar = Picture::Owned { pts_us: 0, w, h, y: y.clone(), u: u.clone(), v: v.clone(), y_stride: w, uv_stride: 3 };
        // NV12 with a stride of 8 and the picture at (2, 2) inside it.
        let stride = 8;
        let uv_offset = stride * 6;
        let mut data = vec![0u8; uv_offset + stride * 3];
        for r in 0..h {
            data[(2 + r) * stride + 2..(2 + r) * stride + 2 + w].copy_from_slice(&y[r * w..(r + 1) * w]);
        }
        for r in 0..2 {
            for i in 0..3 {
                data[uv_offset + (1 + r) * stride + 2 + 2 * i] = u[r * 3 + i];
                data[uv_offset + (1 + r) * stride + 3 + 2 * i] = v[r * 3 + i];
            }
        }
        let nv12 = Picture::Nv12(Nv12Picture { pts_us: 0, w, h, crop: (2, 2), stride, uv_offset, data });
        let mut f = Frames::default();
        let a = f.convert(&planar).unwrap().get_data();
        let b = f.convert(&nv12).unwrap().get_data();
        assert_eq!(a, b);
    }

    #[test]
    fn buffers_are_reused_once_let_go() {
        let mut f = Frames::default();
        let first = f.convert(&planes(16, 16, 100, 128, 128)).unwrap();
        let p1 = Arc::as_ptr(&first.get_data());
        drop(first);
        let second = f.convert(&planes(16, 16, 100, 128, 128)).unwrap();
        assert_eq!(Arc::as_ptr(&second.get_data()), p1);
    }
}
