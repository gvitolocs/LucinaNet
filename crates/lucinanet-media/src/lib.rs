/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet's media backend: servo-media on Juno's own Rust decoders, in place of GStreamer (which does not fit a static musl
//! build) and of the dummy backend (which plays nothing).
//!
//! - `<video>` and `<audio>`: [`player`], with the Video app's demuxers, decoders (Venus first, then rav1d, rusty_h264,
//!   filmcraft-vp9), pacing and clock from LucinaOS; sound through symphonia into [`mixer`], the clock following it.
//! - WebAudio: servo-media's own graph renders, [`webaudio`] takes it to the mixer, and `decodeAudioData` decodes with symphonia.
//! - `canPlayType`: [`mime`], only what these decoders really play.
//! - Juno's screen stays on while a video plays: [`hold`].
//! - MediaStream capture, WebRTC: the dummy backend's, as before (Juno's microphone is silent; out of scope here).
//! - DRM (EME, Widevine): out of scope; Servo has no EME and nothing here pretends to.
//!
//! Design, adopt/adapt/reject record and what is proven: docs/media.md.

/// `LUCINANET_MEDIA_TRACE=1`: what the backend decides, on stderr.
pub(crate) fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LUCINANET_MEDIA_TRACE").is_some())
}

macro_rules! trace {
    ($($t:tt)*) => {
        if crate::tracing() {
            eprintln!("lucinanet-media: {}", format_args!($($t)*));
        }
    };
}

/// `LUCINANET_MEDIA_RECOVERY=0`: off. On by default — with the clock standing still and audio buffered past the position, the
/// watchdog re-opens the sound feed once. Every run that uses it says so in the log, so it cannot hide a cause.
pub(crate) fn recovery() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("LUCINANET_MEDIA_RECOVERY").map(|v| v != "0").unwrap_or(true))
}

/// `LUCINANET_MEDIA_WALLCLOCK_FALLBACK=1`: on. Off by default: neither Firefox nor Chromium hands a stalled clock to the wall
/// clock once an audio sink exists (see the browser maps), so neither do we unless told.
pub(crate) fn wallclock_fallback() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("LUCINANET_MEDIA_WALLCLOCK_FALLBACK").map(|v| v == "1" || v == "true").unwrap_or(false))
}

#[cfg(target_os = "macos")]
mod coreaudio;
pub mod frames;
pub mod hold;
pub mod mime;
pub mod mixer;
#[cfg(feature = "mse")]
pub mod mse;
#[cfg(feature = "mse")]
pub mod mse_dump;
#[cfg(feature = "mse")]
pub mod mse_play;
pub mod player;
pub mod ring;
pub mod rules;
pub mod sound;
pub mod source;
pub mod surfaces;
pub mod sys;
pub mod webaudio;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, Weak};

use servo_base::generic_channel::GenericCallback;
use servo_media::{Backend, BackendInit, MediaInstanceError, SupportsMediaType};
use servo_media_audio::context::{AudioContext, AudioContextOptions};
use servo_media_audio::decoder::AudioDecoder;
use servo_media_audio::sink::AudioSinkError;
use servo_media_audio::{AudioBackend, AudioStreamReader};
use servo_media_dummy::{DummyBackend, DummyStreamReader};
use servo_media_player::audio::AudioRenderer;
use servo_media_player::context::PlayerGLContext;
use servo_media_player::video::VideoFrameRenderer;
use servo_media_player::{Player, PlayerEvent, StreamType};
use servo_media_streams::capture::MediaTrackConstraintSet;
use servo_media_streams::device_monitor::MediaDeviceMonitor;
use servo_media_streams::registry::MediaStreamId;
use servo_media_streams::{MediaOutput, MediaSocket, MediaStreamType};
use servo_media_traits::{BackendMsg, ClientContextId, MediaInstance};
use servo_media_webrtc::{WebRtcController, WebRtcSignaller};

type Instances = HashMap<ClientContextId, Vec<(usize, Weak<Mutex<dyn MediaInstance>>)>>;

pub struct LucinaBackend {
    instances: Arc<Mutex<Instances>>,
    next_id: AtomicUsize,
    backend_chan: Arc<Mutex<Sender<BackendMsg>>>,
    venus: bool,
}

impl BackendInit for LucinaBackend {
    fn init() -> Box<dyn Backend> {
        let instances: Arc<Mutex<Instances>> = Arc::default();
        let (backend_chan, received) = mpsc::channel::<BackendMsg>();
        let for_thread = instances.clone();
        // GStreamer's shutdown thread takes one message and stops; this one takes them all.
        let _ = std::thread::Builder::new().name("lucinanet-media-shutdown".into()).spawn(move || {
            while let Ok(BackendMsg::Shutdown { context, id, tx_ack }) = received.recv() {
                let mut all = for_thread.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(v) = all.get_mut(&context) {
                    v.retain(|(i, _)| *i != id);
                    if v.is_empty() {
                        all.remove(&context);
                    }
                }
                let _ = tx_ack.send(());
            }
        });
        Box::new(LucinaBackend {
            instances,
            next_id: AtomicUsize::new(0),
            backend_chan: Arc::new(Mutex::new(backend_chan)),
            venus: mime::venus_present(),
        })
    }
}

impl LucinaBackend {
    fn register(&self, context: &ClientContextId, id: usize, instance: Weak<Mutex<dyn MediaInstance>>) {
        self.instances.lock().unwrap_or_else(|e| e.into_inner()).entry(*context).or_default().push((id, instance));
    }

    fn each(&self, context: &ClientContextId, f: &dyn Fn(&dyn MediaInstance) -> Result<(), MediaInstanceError>) {
        let mut all = self.instances.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = all.get_mut(context) {
            v.retain(|(_, weak)| match weak.upgrade() {
                Some(i) => {
                    if f(&*i.lock().unwrap_or_else(|e| e.into_inner())).is_err() {
                        log::warn!("lucinanet-media: a media instance refused an action");
                    }
                    true
                },
                None => false,
            });
        }
    }
}

impl Backend for LucinaBackend {
    fn create_player(
        &self,
        context: &ClientContextId,
        stream_type: StreamType,
        sender: GenericCallback<PlayerEvent>,
        video_renderer: Option<Arc<Mutex<dyn VideoFrameRenderer>>>,
        _audio_renderer: Option<Arc<Mutex<dyn AudioRenderer>>>,
        _gl_context: Box<dyn PlayerGLContext>,
    ) -> Arc<Mutex<dyn Player>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let player = Arc::new(Mutex::new(player::LucinaPlayer::new(id, stream_type, sender, video_renderer, player::Options {
            dump_yuv: std::env::var_os("LUCINANET_MEDIA_DUMP_YUV").map(|p| format!("{}.{id}", p.to_string_lossy()).into()),
        })));
        let weak: Weak<Mutex<dyn MediaInstance>> = Arc::downgrade(&player) as Weak<Mutex<player::LucinaPlayer>>;
        self.register(context, id, weak);
        player
    }

    fn create_audiostream(&self) -> MediaStreamId {
        DummyBackend.create_audiostream()
    }

    fn create_videostream(&self) -> MediaStreamId {
        DummyBackend.create_videostream()
    }

    fn create_stream_output(&self) -> Box<dyn MediaOutput> {
        DummyBackend.create_stream_output()
    }

    fn create_stream_and_socket(&self, ty: MediaStreamType) -> (Box<dyn MediaSocket>, MediaStreamId) {
        DummyBackend.create_stream_and_socket(ty)
    }

    fn create_audioinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        DummyBackend.create_audioinput_stream(set)
    }

    fn create_videoinput_stream(&self, set: MediaTrackConstraintSet) -> Option<MediaStreamId> {
        DummyBackend.create_videoinput_stream(set)
    }

    fn create_audio_context(
        &self,
        context: &ClientContextId,
        options: AudioContextOptions,
    ) -> Result<Arc<Mutex<AudioContext>>, AudioSinkError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let ctx = AudioContext::new::<Self>(id, context, self.backend_chan.clone(), options)?;
        let ctx = Arc::new(Mutex::new(ctx));
        let weak: Weak<Mutex<dyn MediaInstance>> = Arc::downgrade(&ctx) as Weak<Mutex<AudioContext>>;
        self.register(context, id, weak);
        Ok(ctx)
    }

    fn create_webrtc(&self, signaller: Box<dyn WebRtcSignaller>) -> WebRtcController {
        WebRtcController::new::<DummyBackend>(signaller)
    }

    fn can_play_type(&self, media_type: &str) -> SupportsMediaType {
        mime::can_play_type(media_type, self.venus)
    }

    fn mute(&self, id: &ClientContextId, val: bool) {
        self.each(id, &|i| i.mute(val));
    }

    fn suspend(&self, id: &ClientContextId) {
        self.each(id, &|i| i.suspend());
    }

    fn resume(&self, id: &ClientContextId) {
        self.each(id, &|i| i.resume());
    }

    fn get_device_monitor(&self) -> Box<dyn MediaDeviceMonitor> {
        DummyBackend.get_device_monitor()
    }
}

impl AudioBackend for LucinaBackend {
    type Sink = webaudio::LucinaAudioSink;

    fn make_decoder() -> Box<dyn AudioDecoder> {
        Box::new(webaudio::LucinaAudioDecoder)
    }

    fn make_sink() -> Result<Self::Sink, AudioSinkError> {
        Ok(webaudio::LucinaAudioSink::new())
    }

    fn make_streamreader(_id: MediaStreamId, _sample_rate: f32) -> Result<Box<dyn AudioStreamReader + Send>, AudioSinkError> {
        Ok(Box::new(DummyStreamReader))
    }
}

/// The backend, for `servo::set_media_backend`.
pub fn backend() -> Box<dyn Backend> {
    LucinaBackend::init()
}
