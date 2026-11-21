/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::sync::{Arc, Mutex, RwLock, Weak};

use euclid::default::Size2D;
use layout_api::{MediaFrame, MediaFrameYuv};
use media::{GLPlayerMsg, GLPlayerMsgForward, WindowGLContext};
use paint_api::{CrossProcessPaintApi, ImageUpdate, SerializableImageData};
use pixels::RasterImage;
use servo_base::generic_channel::{GenericCallback, GenericSharedMemory};
use servo_base::id::WebViewId;
use servo_media::player::video::{ExternalYuv, VideoFrame, VideoFrameRenderer, YuvMatrix};
use webrender_api::{
    ColorRange, ExternalImageData, ExternalImageId, ExternalImageType, ImageBufferKind,
    ImageDescriptor, ImageDescriptorFlags, ImageFormat, ImageKey, YuvColorSpace,
};

use crate::tasks::task_source::SendableTaskSource;

#[derive(MallocSizeOf, PartialEq)]
enum FrameStatus {
    Locked,
    Unlocked,
}

#[derive(MallocSizeOf)]
pub(crate) struct FrameHolder(FrameStatus, VideoFrame);

impl FrameHolder {
    fn new(frame: VideoFrame) -> FrameHolder {
        FrameHolder(FrameStatus::Unlocked, frame)
    }

    fn lock(&mut self) {
        if self.0 == FrameStatus::Unlocked {
            self.0 = FrameStatus::Locked;
        };
    }

    fn unlock(&mut self) {
        if self.0 == FrameStatus::Locked {
            self.0 = FrameStatus::Unlocked;
        };
    }

    fn set(&mut self, new_frame: VideoFrame) {
        if self.0 == FrameStatus::Unlocked {
            self.1 = new_frame
        };
    }

    fn get(&self) -> (u32, Size2D<i32>, usize) {
        if self.0 == FrameStatus::Locked {
            (
                self.1.get_texture_id(),
                Size2D::new(self.1.get_width(), self.1.get_height()),
                0,
            )
        } else {
            unreachable!();
        }
    }

    pub(crate) fn get_frame(&self) -> VideoFrame {
        self.1.clone()
    }
}

#[derive(MallocSizeOf)]
pub(crate) struct MediaFrameRenderer {
    webview_id: WebViewId,
    player_id: Option<usize>,
    #[conditional_malloc_size_of]
    glplayer_id: Arc<RwLock<Option<u64>>>,
    paint_api: CrossProcessPaintApi,
    player_context: WindowGLContext,
    current_frame: Option<MediaFrame>,
    old_frame: Option<ImageKey>,
    very_old_frame: Option<ImageKey>,
    current_frame_holder: Option<FrameHolder>,
    /// <https://html.spec.whatwg.org/multipage/#poster-frame>
    poster_frame: Option<MediaFrame>,
    /// LucinaNet: U and V plane keys of YUV frames no longer shown, deleted a frame later as
    /// `old_frame` and `very_old_frame` are (the display list may still name them).
    old_planes: Vec<ImageKey>,
    very_old_planes: Vec<ImageKey>,
    /// LucinaNet: `render` installed an image key that no display list can name yet, because the
    /// last layout pass read no frame. The element damages its node once for such a key so that
    /// layout re-reads it; see `HTMLMediaElement::playback_video_frame_updated`.
    new_image_key: bool,
}

impl MediaFrameRenderer {
    pub(crate) fn new(
        webview_id: WebViewId,
        paint_api: CrossProcessPaintApi,
        player_context: WindowGLContext,
    ) -> Self {
        Self {
            webview_id,
            player_id: None,
            glplayer_id: Arc::new(RwLock::new(None)),
            paint_api,
            player_context,
            current_frame: None,
            old_frame: None,
            very_old_frame: None,
            current_frame_holder: None,
            poster_frame: None,
            old_planes: Vec::new(),
            very_old_planes: Vec::new(),
            new_image_key: false,
        }
    }

    /// LucinaNet: take and clear the flag saying that a frame is now shown under a freshly
    /// generated image key, which the display list built from the last layout pass cannot name.
    pub(crate) fn take_new_image_key(&mut self) -> bool {
        std::mem::take(&mut self.new_image_key)
    }

    pub(crate) fn current_frame(&self) -> Option<MediaFrame> {
        self.current_frame
    }

    pub(crate) fn poster_frame(&self) -> Option<MediaFrame> {
        self.poster_frame
    }

    pub(crate) fn current_frame_holder(&self) -> Option<&FrameHolder> {
        self.current_frame_holder.as_ref()
    }

    pub(crate) fn setup(
        &mut self,
        player_id: usize,
        task_source: SendableTaskSource,
        weak_video_renderer: Weak<Mutex<MediaFrameRenderer>>,
    ) {
        self.player_id = Some(player_id);
        let glplayer_id = self.glplayer_id.clone();
        let callback = GenericCallback::new(move |message| {
            let message = message.unwrap();
            let weak_video_renderer = weak_video_renderer.clone();

            let glplayer_id = glplayer_id.clone();
            task_source.queue(task!(handle_glplayer_message: move || {
                trace!("GLPlayer message {:?}", message);

                let Some(video_renderer) = weak_video_renderer.upgrade() else {
                    return;
                };

                match message {
                    GLPlayerMsgForward::Lock(sender) => {
                        if let Some(holder) = video_renderer
                            .lock()
                            .unwrap()
                            .current_frame_holder
                            .as_mut() {
                                holder.lock();
                                sender.send(holder.get()).unwrap();
                            };
                    },
                    GLPlayerMsgForward::Unlock() => {
                        if let Some(holder) = video_renderer
                            .lock()
                            .unwrap()
                            .current_frame_holder
                            .as_mut() { holder.unlock() }
                    },
                    GLPlayerMsgForward::PlayerId(id) => {
                        let mut glplayer_id = glplayer_id.write().unwrap();
                        if let Some(already_set_id) = *glplayer_id {
                            error!("Player id already set to {already_set_id} will be replaced with {id}");
                        }
                        *glplayer_id = Some(id);
                    },
                }
            }));
        })
        .expect("Could not create callback");

        if let Some(glplayer_thread_sender) = &self.player_context.glplayer_thread_sender {
            glplayer_thread_sender
                .send(GLPlayerMsg::RegisterPlayer(callback))
                .unwrap();
        }
    }

    pub(crate) fn reset(&mut self) {
        self.player_id = None;

        if let Some(glplayer_id) = self.glplayer_id.write().unwrap().take() {
            self.player_context
                .send(GLPlayerMsg::UnregisterPlayer(glplayer_id));
        }

        self.current_frame_holder = None;

        let mut updates = smallvec::smallvec![];

        if let Some(current_frame) = self.current_frame.take() {
            updates.push(ImageUpdate::DeleteImage(current_frame.image_key));
            if let Some(planes) = current_frame.yuv {
                updates.push(ImageUpdate::DeleteImage(planes.u));
                updates.push(ImageUpdate::DeleteImage(planes.v));
            }
        }

        for key in self
            .old_planes
            .drain(..)
            .chain(self.very_old_planes.drain(..))
        {
            updates.push(ImageUpdate::DeleteImage(key));
        }

        if let Some(old_image_key) = self.old_frame.take() {
            updates.push(ImageUpdate::DeleteImage(old_image_key));
        }

        if let Some(very_old_image_key) = self.very_old_frame.take() {
            updates.push(ImageUpdate::DeleteImage(very_old_image_key));
        }

        if !updates.is_empty() {
            self.paint_api
                .update_images(self.webview_id.into(), updates);
        }
    }

    pub(crate) fn set_poster_frame(&mut self, image: Option<Arc<RasterImage>>) {
        self.poster_frame = image.and_then(|image| {
            image.id.map(|image_key| MediaFrame {
                image_key,
                width: image.metadata.width as i32,
                height: image.metadata.height as i32,
                yuv: None,
            })
        });
    }

    /// LucinaNet: a frame whose Y, U and V planes the backend holds as external images. The
    /// three keys stay while the size does; each frame updates them, which gives WebRender a new
    /// generation to composite, and no pixels pass through here.
    fn render_external_yuv(&mut self, frame: &VideoFrame, yuv: ExternalYuv) {
        let (width, height) = (frame.get_width(), frame.get_height());
        let mut updates = smallvec::smallvec![];

        for key in self.very_old_planes.drain(..) {
            updates.push(ImageUpdate::DeleteImage(key));
        }
        self.very_old_planes = std::mem::take(&mut self.old_planes);
        if let Some(old_image_key) =
            std::mem::replace(&mut self.very_old_frame, self.old_frame.take())
        {
            updates.push(ImageUpdate::DeleteImage(old_image_key));
        }

        let chroma = ((width + 1) / 2, (height + 1) / 2);
        let sizes = [(width, height), chroma, chroma];
        let descriptor = |plane: usize| {
            ImageDescriptor::new(
                sizes[plane].0,
                sizes[plane].1,
                ImageFormat::R8,
                ImageDescriptorFlags::empty(),
            )
        };
        let data = |plane: usize| {
            SerializableImageData::External(ExternalImageData {
                id: ExternalImageId(yuv.ids[plane]),
                channel_index: 0,
                image_type: ExternalImageType::TextureHandle(ImageBufferKind::Texture2D),
                normalized_uvs: false,
            })
        };
        let color_space = match yuv.matrix {
            YuvMatrix::Bt601 => YuvColorSpace::Rec601,
            YuvMatrix::Bt709 => YuvColorSpace::Rec709,
            YuvMatrix::Bt2020 => YuvColorSpace::Rec2020,
        };
        let color_range = if yuv.full_range {
            ColorRange::Full
        } else {
            ColorRange::Limited
        };

        match &mut self.current_frame {
            Some(current)
                if current.width == width && current.height == height && current.yuv.is_some() =>
            {
                let planes = current.yuv.expect("checked above");
                let keys = [current.image_key, planes.u, planes.v];
                profile_traits::video_trace!("renderer: yuv planes updated in place {:?}", keys);
                for (plane, key) in keys.into_iter().enumerate() {
                    updates.push(ImageUpdate::UpdateImage(
                        key,
                        descriptor(plane),
                        data(plane),
                        None,
                    ));
                }
                current.yuv = Some(MediaFrameYuv {
                    color_space,
                    color_range,
                    ..planes
                });
            },
            _ => {
                let mut keys = [ImageKey::DUMMY; 3];
                for key in &mut keys {
                    let Some(new_key) = self.paint_api.generate_image_key_blocking(self.webview_id)
                    else {
                        return;
                    };
                    *key = new_key;
                }
                if let Some(old) = self.current_frame.take() {
                    self.old_frame = Some(old.image_key);
                    if let Some(planes) = old.yuv {
                        self.old_planes.extend([planes.u, planes.v]);
                    }
                }
                profile_traits::video_trace!(
                    "renderer: yuv AddImage {:?} of {}x{}, old_frame={:?}",
                    keys,
                    width,
                    height,
                    self.old_frame
                );
                for (plane, key) in keys.into_iter().enumerate() {
                    updates.push(ImageUpdate::AddImage(
                        key,
                        descriptor(plane),
                        data(plane),
                        false,
                    ));
                }
                self.new_image_key = true;
                self.current_frame = Some(MediaFrame {
                    image_key: keys[0],
                    width,
                    height,
                    yuv: Some(MediaFrameYuv {
                        u: keys[1],
                        v: keys[2],
                        color_space,
                        color_range,
                    }),
                });
            },
        }

        // The GL player's lock and unlock are for texture frames only.
        self.current_frame_holder = None;
        self.paint_api
            .update_images(self.webview_id.into(), updates);
    }
}

impl Drop for MediaFrameRenderer {
    fn drop(&mut self) {
        self.reset();
    }
}

impl VideoFrameRenderer for MediaFrameRenderer {
    fn render(&mut self, frame: VideoFrame) {
        if self.player_id.is_none()
            || (frame.is_gl_texture() && self.glplayer_id.read().unwrap().is_none())
        {
            profile_traits::video_trace!(
                "renderer: frame dropped, player_id={:?} gl={} glplayer_id={:?}",
                self.player_id,
                frame.is_gl_texture(),
                *self.glplayer_id.read().unwrap()
            );
            return;
        }

        if let Some(yuv) = frame.external_yuv() {
            profile_traits::video_trace!(
                "renderer: yuv frame {}x{} ids={:?} current={:?}",
                frame.get_width(),
                frame.get_height(),
                yuv.ids,
                self.current_frame.map(|frame| frame.image_key)
            );
            self.render_external_yuv(&frame, yuv);
            return;
        }

        profile_traits::video_trace!(
            "renderer: bgra frame {}x{} gl={} current={:?}",
            frame.get_width(),
            frame.get_height(),
            frame.is_gl_texture(),
            self.current_frame.map(|frame| frame.image_key)
        );

        let mut updates = smallvec::smallvec![];

        if let Some(old_image_key) =
            std::mem::replace(&mut self.very_old_frame, self.old_frame.take())
        {
            updates.push(ImageUpdate::DeleteImage(old_image_key));
        }

        let descriptor = ImageDescriptor::new(
            frame.get_width(),
            frame.get_height(),
            ImageFormat::BGRA8,
            ImageDescriptorFlags::empty(),
        );

        match &mut self.current_frame {
            Some(current_frame)
                if current_frame.width == frame.get_width()
                    && current_frame.height == frame.get_height()
                    && current_frame.yuv.is_none() =>
            {
                if !frame.is_gl_texture() {
                    updates.push(ImageUpdate::UpdateImage(
                        current_frame.image_key,
                        descriptor,
                        SerializableImageData::Raw(GenericSharedMemory::from_arc_vec(
                            frame.get_data(),
                        )),
                        None,
                    ));
                }

                self.current_frame_holder
                    .get_or_insert_with(|| FrameHolder::new(frame.clone()))
                    .set(frame);

                if let Some(old_image_key) = self.old_frame.take() {
                    updates.push(ImageUpdate::DeleteImage(old_image_key));
                }
            },
            Some(current_frame) => {
                self.old_frame = Some(current_frame.image_key);
                if let Some(planes) = current_frame.yuv.take() {
                    self.old_planes.extend([planes.u, planes.v]);
                }

                let Some(new_image_key) =
                    self.paint_api.generate_image_key_blocking(self.webview_id)
                else {
                    profile_traits::video_trace!("renderer: Paint gave no image key");
                    return;
                };

                profile_traits::video_trace!(
                    "renderer: bgra AddImage {new_image_key:?} of {}x{}, old_frame={:?}",
                    frame.get_width(),
                    frame.get_height(),
                    self.old_frame
                );

                /* update current_frame */
                current_frame.image_key = new_image_key;
                current_frame.width = frame.get_width();
                current_frame.height = frame.get_height();
                self.new_image_key = true;

                // FIXME: This code is duplicated below this branch
                let image_data = self
                    .glplayer_id
                    .read()
                    .unwrap()
                    .filter(|_| frame.is_gl_texture())
                    .map(|glplayer_id| {
                        let texture_target = if frame.is_external_oes() {
                            ImageBufferKind::TextureExternal
                        } else {
                            ImageBufferKind::Texture2D
                        };

                        SerializableImageData::External(ExternalImageData {
                            id: ExternalImageId(glplayer_id),
                            channel_index: 0,
                            image_type: ExternalImageType::TextureHandle(texture_target),
                            normalized_uvs: false,
                        })
                    })
                    .unwrap_or_else(|| {
                        SerializableImageData::Raw(GenericSharedMemory::from_arc_vec(
                            frame.get_data(),
                        ))
                    });

                self.current_frame_holder
                    .get_or_insert_with(|| FrameHolder::new(frame.clone()))
                    .set(frame);

                updates.push(ImageUpdate::AddImage(
                    new_image_key,
                    descriptor,
                    image_data,
                    false,
                ));
            },
            None => {
                let Some(image_key) = self.paint_api.generate_image_key_blocking(self.webview_id)
                else {
                    profile_traits::video_trace!("renderer: Paint gave no image key");
                    return;
                };

                profile_traits::video_trace!(
                    "renderer: bgra AddImage {image_key:?} of {}x{}, first frame, gl={}",
                    frame.get_width(),
                    frame.get_height(),
                    frame.is_gl_texture()
                );

                self.new_image_key = true;
                self.current_frame = Some(MediaFrame {
                    image_key,
                    width: frame.get_width(),
                    height: frame.get_height(),
                    yuv: None,
                });

                let image_data = self
                    .glplayer_id
                    .read()
                    .unwrap()
                    .filter(|_| frame.is_gl_texture())
                    .map(|glplayer_id| {
                        let texture_target = if frame.is_external_oes() {
                            ImageBufferKind::TextureExternal
                        } else {
                            ImageBufferKind::Texture2D
                        };

                        SerializableImageData::External(ExternalImageData {
                            id: ExternalImageId(glplayer_id),
                            channel_index: 0,
                            image_type: ExternalImageType::TextureHandle(texture_target),
                            normalized_uvs: false,
                        })
                    })
                    .unwrap_or_else(|| {
                        SerializableImageData::Raw(GenericSharedMemory::from_arc_vec(
                            frame.get_data(),
                        ))
                    });

                self.current_frame_holder = Some(FrameHolder::new(frame));

                updates.push(ImageUpdate::AddImage(
                    image_key, descriptor, image_data, false,
                ));
            },
        }
        self.paint_api
            .update_images(self.webview_id.into(), updates);
    }
}
