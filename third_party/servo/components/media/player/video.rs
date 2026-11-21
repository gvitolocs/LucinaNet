/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::Arc;

use malloc_size_of_derive::MallocSizeOf;

#[derive(Clone, MallocSizeOf)]
pub enum VideoFrameData {
    Raw(#[conditional_malloc_size_of] Arc<Vec<u8>>),
    Texture(u32),
    OESTexture(u32),
    /// LucinaNet: a 4:2:0 picture whose Y, U and V planes the backend holds as WebRender external
    /// images of the `Media` handler type (one id per plane), so that WebRender converts and scales
    /// it while compositing. Readers that want pixels get BGRA from [`Buffer::bgra`].
    ExternalYuv(ExternalYuv),
}

/// The matrix of a YUV picture.
#[derive(Clone, Copy, Debug, MallocSizeOf, PartialEq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

/// LucinaNet: where WebRender finds a picture's planes, and how to turn them into colour.
#[derive(Clone, Copy, Debug, MallocSizeOf, PartialEq)]
pub struct ExternalYuv {
    /// External image ids of the Y, U and V planes, allocated by the backend for the `Media`
    /// handler type. They stay the same for a player; each new frame is a new generation.
    pub ids: [u64; 3],
    pub matrix: YuvMatrix,
    pub full_range: bool,
}

pub trait Buffer: Send + Sync {
    fn to_vec(&self) -> Option<VideoFrameData>;

    /// The picture as BGRA, for frames whose data is not raw (LucinaNet: `ExternalYuv`).
    fn bgra(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

#[derive(Clone, MallocSizeOf)]
pub struct VideoFrame {
    width: i32,
    height: i32,
    data: VideoFrameData,
    #[ignore_malloc_size_of = "Difficult"]
    _buffer: Arc<dyn Buffer>,
}

impl VideoFrame {
    pub fn new(width: i32, height: i32, buffer: Arc<dyn Buffer>) -> Option<Self> {
        let data = buffer.to_vec()?;
        Some(VideoFrame {
            width,
            height,
            data,
            _buffer: buffer,
        })
    }

    pub fn get_width(&self) -> i32 {
        self.width
    }

    pub fn get_height(&self) -> i32 {
        self.height
    }

    pub fn get_data(&self) -> Arc<Vec<u8>> {
        match self.data {
            VideoFrameData::Raw(ref data) => data.clone(),
            VideoFrameData::ExternalYuv(_) => self._buffer.bgra().unwrap_or_else(|| {
                Arc::new(vec![0; self.width.max(0) as usize * self.height.max(0) as usize * 4])
            }),
            _ => unreachable!("invalid raw data request for texture frame"),
        }
    }

    pub fn get_texture_id(&self) -> u32 {
        match self.data {
            VideoFrameData::Texture(data) | VideoFrameData::OESTexture(data) => data,
            _ => unreachable!("invalid texture id request for raw data frame"),
        }
    }

    pub fn is_gl_texture(&self) -> bool {
        matches!(
            self.data,
            VideoFrameData::Texture(_) | VideoFrameData::OESTexture(_)
        )
    }

    pub fn is_external_oes(&self) -> bool {
        matches!(self.data, VideoFrameData::OESTexture(_))
    }

    /// LucinaNet: the planes' external images, for a frame WebRender composites as YUV.
    pub fn external_yuv(&self) -> Option<ExternalYuv> {
        match self.data {
            VideoFrameData::ExternalYuv(yuv) => Some(yuv),
            _ => None,
        }
    }
}

pub trait VideoFrameRenderer: Send + 'static {
    fn render(&mut self, frame: VideoFrame);
}
