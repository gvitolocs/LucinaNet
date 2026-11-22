# Media in LucinaNet: `<video>`, `<audio>` and WebAudio on Juno

LucinaNet's Juno build is static musl. GStreamer does not fit it, so until now servo-media ran its dummy backend and nothing
played. `crates/lucinanet-media` is a servo-media backend in Rust, built on the media parts Juno already has in LucinaOS.

## What plays

| | How |
|---|---|
| Containers | MP4 (plain, moov at either end, fragmented), Matroska and WebM for pictures (`lucina_video::demux`); MP4, Matroska, WebM, Ogg, MP3, FLAC, WAV, AIFF for sound (symphonia through `lucina_audio_decode`) |
| Video | H.264, H.265, VP9 on the Venus video block when it takes the stream; otherwise H.264 `rusty_h264`, VP9 `filmcraft-vp9`, AV1 `rav1d` on the CPU (`lucina_video_play::decode`). H.265 only on Venus. VP8 not yet |
| Audio | AAC (LC, HE, HE v2 through syom), MP3, Vorbis, FLAC, ALAC, PCM. **No Opus yet** (see "Open") |
| Drawing | I420 planes into WebRender as external images (`surfaces.rs`, patch C; swgl converts and scales them while compositing), BGRA frames through Servo's `media_frame_renderer` instead with `LUCINANET_MEDIA_YUV=0` or without a LucinaNet painter |
| Sound out | one mixer for the whole browser into `/dev/lucina-audio` (Juno's device takes one program and one stream) |
| Clock | audio is master: each player's position at the speaker, from the mixer's block marks and the device's played count |
| WebAudio | servo-media's own Rust graph renders; its output goes into the mixer; `decodeAudioData` through symphonia |
| `canPlayType` | only what the decoders above play ("probably" with every codec known, "maybe" for a known container) |
| Screen | Juno's screen is held on while a video plays (juno-power's `LUCINA_HOLD_FD` protocol), not while paused or ended |

**Out of scope:** DRM. Encrypted Media Extensions (Widevine, PlayReady) are not implemented in Servo and are not attempted here;
a page that needs them does not play. MediaStream capture and WebRTC stay on the dummy backend (Juno's microphone is silent).

## How it fits

```
script (htmlmediaelement.rs) ── push_data / end_of_stream / set_input_size ──▶ source::Source (byte cache, Verus: rules::flow)
          ▲                                                                       │ two readers
          │ PlayerEvent (Metadata, StateChanged, Position, SeekData, NeedData,     ├──▶ lucina_video demux ─▶ worker (decode thread:
          │              EnoughData, VideoFrameUpdated, SeekDone, EndOfStream)    │                         Venus / rav1d / h264 / vp9)
          │                                                                       └──▶ symphonia ─▶ sound thread ─▶ mixer stream
player::Presenter ◀── pace (Video app) ◀── Clock (audio master, Verus: rules::clock) ◀──────────────────── mixer ─▶ /dev/lucina-audio
          ├──▶ surfaces::Surface (I420 planes, 3 external image ids; the default) ─▶ VideoFrameRenderer ─▶ YuvImage ─▶ swgl composites
          └──▶ frames::Frames (BGRA, with LUCINANET_MEDIA_YUV=0; also without a LucinaNet painter) ─▶ VideoFrameRenderer ─▶ WebRender
```

## The original, and what was kept

The GStreamer backend is the behavioural original for the player's
contract with Servo; the Video app (`lucina-app-video`) is the original for playing on Juno.

| Piece | Decision | Notes |
|---|---|---|
| servo-media GStreamer player (MPL-2.0) | **reference** (behaviour) | Event order kept: metadata, first picture, `StateChanged(Paused)`; `play`/`pause` answered with state changes; position every 100 ms (GstPlay's default); `seek` refused when not seekable or past the duration; `SeekDone` at the time asked; pushes dropped while a fetch is being restarted (servosrc); `EnoughData` makes `push_data` fail |
| GStreamer source `servosrc` (appsrc) | **adapt** | Two readers over one sparse cache instead of one demuxer reading once. After `EnoughData` Servo cancels a seekable fetch and would answer the next `NeedData` with a seek to the playback position; this asks for a fetch from where the old one stopped (`SeekData`) instead, so a long film downloads in pieces without disturbing playback |
| GStreamer audio sink / decoder | **adapt** | The sink keeps 40 ms in the mixer instead of one render quantum in an appsrc; the decoder gives stereo at the context's rate instead of the file's own channels |
| GStreamer's `can_play_type` registry scan | **adapt** | Static lists of what LucinaOS decodes |
| `lucina-app-video` decode thread, pacing, clock, decoders (LucinaOS) | **adopt** | Moved unchanged into the library `lucina-video-play` (LucinaOS commit 7399e1c45) and used as they are |
| `lucina_video` demuxers and `limits` | **adopt** | Containers at most 5 M samples, 32 MB a frame, pictures at most 4096x2304, VP9 sizes checked before the decoder |
| `lucina_audio_decode` (symphonia 0.6.1 MPL-2.0, syom 0.9.0 MIT) | **adopt** | Sound decoded to 48 kHz stereo |
| Decoders: rav1d (BSD-2-Clause), rusty_h264-decoder 0.17.0 (BSD-2-Clause), filmcraft-vp9 0.9.0 (MIT OR Apache-2.0) | **adopt** | As the Video app, each checked frame by frame against FFmpeg in LucinaOS |
| mp4parse (Mozilla) | **reject** for now | `lucina_video::mp4` already reads plain and fragmented MP4 with Juno's limits; a second MP4 parser would be two to keep right |
| GStreamer itself | **reject** | C, does not fit the static musl build |
| `opus1` 0.3.0 (BSD-3-Clause, pure Rust, libopus 1.6 port) | **adopt** | Measured against libopus (FFmpeg) on Linux x86_64: CELT music 103-110 dB SNR, 530-2280x real time; its README says bit-exact with libopus on 64-bit ARM, which the laptop test checks. Being wired into `lucina_audio_decode` for files and MSE packets |
| `opus-pure` 0.2.2 (BSD-3) | reference | As accurate (110-113 dB), 25% slower; has an Ogg reader |
| `opus-decoder` 0.1.1 (MIT/Apache, no `unsafe`) | **reject** | Right, but 1.7x real time on a Linux x86_64 desktop CPU: too slow for the laptop |
| `rusty-opus` 1.0.2 (BSD-3) | **reject** | 69-83 dB on CELT, worse than the others |
| GStreamer's `servosrc`/`qtdemux` for MSE | reference | MSE is not in upstream Servo at all; the spec is the original (below) |

Licences: everything linked is MPL-2.0, MIT, Apache-2.0 or BSD-2-Clause, the LucinaOS code included. New code here
is MPL-2.0, as LucinaNet.

## Proved, tested

- **Verus** (`scripts/verify-shipped.sh`, on the files as shipped; 2026-11-05: flow 8 verified, clock 11 verified, 0 errors;
  planted bugs in `verification/mutants.sh`): `rules/flow.rs` (the two signals alternate; pushes only while
  filling; full exactly at the high mark and back exactly at the low mark; a read waits for the running fetch only within its
  slack; the bytes of a push that are new never overrun the next cached run), `rules/clock.rs` (a stream's position stays inside
  its mix block and never goes back as the device plays; frames to microseconds exact and without overflow).
- **Verus, 2026-11-08** (planted bugs in `verification/mutants.sh`, all refused): `rules/buffered.rs`, shipped, 12 verified (a
  track's buffered ranges normalised as TimeRanges must be, every frame inside one, the first at the first frame; a
  SourceBuffer's ranges exactly the intersection of its tracks'; the quota check without overflow; eviction ends at a random
  access point more than 10 s behind the playback position, never at or after it; 9 mutants). `verification/handover/model.rs`,
  a model of `surfaces::pin`/`unpin` and `media_images.rs`, 7 verified: while a lock of an id is open, every texture locked under
  it points at a pinned picture, so `publish` never writes what WebRender reads (6 mutants). The model found a bug in the first
  version: each unlock let go of the oldest pin, and a lock that found no picture (WebRender's tile path unlocks those too) freed
  the picture another open lock's texture still pointed at. Pins are now counted per id and kept until its last lock is let go.
- **End to end without Servo** (`tests/drive.rs`): the player driven as `htmlmediaelement.rs` drives it (NeedData, EnoughData with
  Backoff, SeekData and the seek lock, play once paused), every picture shown compared with FFmpeg's `framemd5` at its time:
  H.264+AAC MP4 with the index first and last, VP9+Vorbis WebM, an MP3 alone, a seek to 3 s, pause, a file that is not media.
- **`LUCINANET_MEDIA_DUMP_YUV=<base>`**, the player's diagnostic: every picture shown as raw I420 in `<base>.<player id>`, and
  `<base>.pts` one line per picture shown — `<index> <pts_ms>`: the 0-based place in the file, and its time in milliseconds.
- **Unit tests** in each module: the byte cache (reads, waits, far reads asking for a fetch, pushes dropped until it starts,
  enough then a fetch from where it stopped, a second fetch skipping bytes already held, eviction), the mixer (sums, positions,
  pause, volume, the `none` sink and the WAV tee over it), BGRA conversion (colours, NV12 against planar, buffer reuse),
  `canPlayType`.

## Turning sound off

`LUCINANET_MEDIA_SINK=none` (or `null`) is the switch: the mixer opens the stand-in sink at once, without asking any
device for one. On macOS CoreAudio is never opened and no output stream is ever built, so a headless `--render` run
cannot be heard; the clock behaves exactly as it does with no device, since `Nowhere` paces at real time. Unset, empty
or an unrecognised value leaves the behaviour as it was (an unrecognised value says so once, in the log, and plays).
`LUCINANET_MEDIA_WAV=<path>` works on top of it — the tee wraps whichever sink was chosen — so a run with both records
what the mixer mixed while playing nothing. `mixer.rs`: `sink_choice` (the value, read without the process environment)
and `base_sink` (which returns before its macOS block).

    LUCINANET_MEDIA_SINK=none <binary> --render file://tests/media/silent-audio.html /tmp/silent.png

`tests/media/sink-check.sh [binary]` proves the same through the browser in ~10 s — silent samples, no window, no
network — and passes only with the `sound sink none` line in the log, frames in the WAV the tee wrote, and no CoreAudio
line at all. Every script under `tests/media` and `tests/web` that renders a page with sound exports the switch as
`none` by default (`LUCINANET_MEDIA_SINK=${LUCINANET_MEDIA_SINK:-none}`), so a caller can turn the sound back on on
purpose; the Juno laptop scripts stay on the real device, which is what they measure.

**`LUCINA_AUDIO_TEST_DB` does not silence macOS.** Nothing in this tree reads it: it is the level Juno's own
`/dev/lucina-audio` device is opened at, and it survives only in the laptop scripts. The Mac's level is the mixer's own
`DEVICE_DB` (-24 dB, `mixer.rs`), so a run that relied on that variable played its clip out of the speaker.

## Media Source Extensions

The original is the W3C Media Source Extensions specification and its ISO BMFF and WebM byte stream formats; where they leave
room, Chromium's behaviour (buffered ranges joined across gaps under a frame, 150 MB/12 MB quotas, eviction 10 s behind the
playback position).
- **Parsers:** `lucina_video::mse` (LucinaOS): appended bytes split anywhere into initialization segments and coded frames; the
  size and offset arithmetic in `mse_rules.rs` for Verus; tests: fragmented MP4 and WebM (known and unknown sizes) in 1-byte,
  4 KiB and whole pieces give the file demuxers' packets byte for byte; terabyte boxes, garbage, a cluster before tracks.
- **Track buffers:** `crates/lucinanet-media/src/mse.rs`: coded frame processing (3.5.8), removal, eviction, `buffered`.
- **Playing:** `mse_play.rs`: the video track buffer as a `Demuxer` for the same decode thread, the sound through
  `lucina_audio_decode::packet::PacketDecoder` into the mixer.
- **DOM:** Servo patch (MediaSource, SourceBuffer, SourceBufferList, `createObjectURL(MediaSource)`, attaching), against
  `mse_*` methods on servo-media's `Player` that GStreamer, OHOS and the dummy backend refuse by default.
- `isTypeSupported` answers only what plays, within Juno's smooth limits (`mime::Caps`, below): YouTube picks VP9 + Opus.
- `LUCINANET_MEDIA_MSE_DUMP=<dir>` records a page's MSE calls and appended bytes (`mse_dump.rs`); `tests/mse_replay.rs`
  replays a recording (`real_session` with `LUCINANET_MEDIA_MSE_REPLAY`), and `tests/media/laptop/mse-replay.html` replays it
  through the browser's own MediaSource. Recordings of real sites stay out of git.

## Servo patches (carried by the engine fork)

- **A, small:** a `media-embedder` feature in `components/servo` so LucinaNet installs this backend itself.
- **B, large:** Media Source Extensions (MediaSource, SourceBuffer, SourceBufferList, `URL.createObjectURL(MediaSource)`), which
  YouTube and most streaming sites need. Upstream Servo has none.
- **C:** YUV frames into WebRender (`YuvImage`, composited by swgl) in place of BGRA: 0055-0056, below.
- Numbers: the order the patches apply in the fork, oldest first: 0037 = A; 0050 MSE on Player, 0051 MSE DOM, 0052 timeupdate during playback (upstream bug),
  0053 a same-size video frame is a new frame, not a new display list, 0054 MSE duration from the initialization segment;
  0055 `VideoFrameData::ExternalYuv` and an embedder `Media` image handler, 0056 plane keys and `push_yuv_image` with the
  compositor-surface flags, 0057 `getVideoPlaybackQuality()` (`Player::playback_quality`).

## YouTube (2026-11-07)

- **Frame path (0055-0056, `surfaces.rs`, `lucinanet-servo/src/media_images.rs`).** A shown picture is copied once as packed
  I420 (3 MB at 1080p, against 8 MB of BGRA written by a per-pixel conversion) and published under the player's three
  external image ids. WebRender gets a `YuvImage` with PREFER/SUPPORTS_EXTERNAL_COMPOSITOR_SURFACE and makes the video a
  compositor surface; its software compositor asks LucinaNet for the planes (`lock_composite_surface`), which come back as
  swgl textures over the picture's own bytes (`set_texture_buffer`, no copy), and converts and scales them into the window's
  frame across its composite threads. Where WebRender draws the video into picture tiles instead (a transform, a rounded
  clip), the `Media` external image handler gives the same textures one plane at a time. Pictures stay pinned while
  WebRender reads them. Without the handler (tests, other embedders) the BGRA path is unchanged; `LUCINANET_MEDIA_TRACE`
  says once which way the planes went; and `LUCINANET_MEDIA_YUV=0` keeps a page off this path, on BGRA.
  Adopt: Gecko's software WebRender does video this way (`RenderCompositorSWGL::LockCompositeSurface`).
- **Planar frames were off by default for a while (the planar-frames flaw, now fixed).** A video that WebRender scales came out
  as vertical colour bars: one source row repeated over every output row. Measured on macOS (YouTube's player) and
  here on Linux x86_64 (a 640x360 frame in a 960x270 box, paused at 1.0 s: 480 of the picture's 480 columns constant down
  their height, with the Rust rasterizer and with C++ swgl alike, 2026-11-13); the same
  page with BGRA frames, `frames.rs`, was correct (0 of 480), and so was the 1:1 compositor-surface picture. `surfaces.rs`
  therefore started BGRA on every platform in the meantime. The cause was not swgl's sampling but the uv rect of the media
  planes, handed to it unflipped; the engine pinned here (`40c0ff93c`) hands it flipped, and
  `tests/media/scaled-planes.sh` passes with the planes on Linux x86_64 and on macOS. Planes are the default
  again; `LUCINANET_MEDIA_YUV=0` stays as the way to force BGRA frames when debugging.
  `LUCINANET_MEDIA_COPY_PLANES=1` (with the planes) was the experiment that ruled the handover out: `wrap` copies each plane
  into our own buffer, rows exactly `w` apart, kept alive past `unlock`/`unpin` by a registry keyed `(id, plane)`. Measured on
  x86 2026-11-13: byte-identical with and without it, and the same bars either way — the pointer, its pitch and its lifetime
  were out, which the uv fix later confirmed.
- **Caps (`mime::Caps`).** YouTube asks `isTypeSupported` for every format it is offered and takes
  the best that says yes, so formats above Juno's smooth limits are refused: by codec level (RFC 6381 `avc1.PPCCLL`,
  `vp09.PP.LL`, `av01.P.LLT`) and by `width`/`height`/`framerate` parameters. Defaults with Venus: H.264 to level 4.1, VP9 to
  4.0, 1920x1088, 30 fps, no AV1 (Venus has none; rav1d on the CPU); without Venus: 3.1, 720p. `LUCINANET_MEDIA_CAPS`
  (`avc1=42,fps=60,av01=8`) overrides them for measuring. To be set from the laptop A/B.
- **`getVideoPlaybackQuality()` (0057)**: frames shown and dropped from the presenter, so YouTube's player sees drops and
  steps down by itself.
- **On x86 (a Linux desktop, this run, watch page with consent):** YouTube took VP9 + Opus (AV1 before the caps), played
  1.64 to 28.8 s with 60 s buffered, 866 frames, 0 dropped at 854x480. Its SABR session (80 appends) replays in
  `real_session`: 1800 frames, 0 dropped, no append errors.
- **Opus through MSE:** `vp9_and_opus_webm_as_youtube_play_frame_exact` (WebM split at clusters as YouTube appends) and
  `opus_only_webm_plays_to_the_end`.
- Each paint also copies the whole window for the UI (`engine.rs` `copy_frame`) and the UI draws it: the laptop A/B
  (`tests/media/laptop/ab-laptop.sh`) logs paint and copy times with the dropped frames to see whether that is the next limit.

## Found on the way

- rusty_h264 installs a `#[global_allocator]` by default; it does not link next to Servo's jemalloc, so `lucina-video-play` has it
  behind a default feature that LucinaNet turns off.
- The first picture must go up whatever its time (GStreamer's preroll): a WebM whose first frame is at 3 ms waited forever for a
  clock that only starts with that picture.
- A stand-in sound device that counts wall time from its first write runs ahead after a gap; a device stops counting when it
  has nothing to play, and so does `mixer::Nowhere`.

## On the laptop (2026-11-05, LucinaNet main 37a8f4de5d + `--features media`, Juno probe build)

Pages from `tests/media/laptop` with `run-laptop.sh` (sound to the real device at -90 dB, `LUCINA_AUDIO_TEST_DB`), synthetic
1080p 24 fps 30 s clips (testsrc2 with grain; H.264 4.2 Mbit/s high profile + AAC, VP9 2.7 Mbit/s + Vorbis):

| Page | Result | Shown / decoded | Dropped | Decode mean | Late mean / worst | RSS |
|---|---|---|---|---|---|---|
| H.264 1080p (Venus) | PASS, ended at 29.96 s | 423 / 600 | 177 | 43.6 ms | 52 / 182 ms | 208 MB |
| VP9 1080p (Venus) | PASS, ended at 29.97 s | 402 / 720 | 318 | 42.2 ms | 95 / 511 ms | 234 MB |
| MP3 30 s | PASS, ended at 30.00 s | | | | | 93 MB |
| H.264 seek to 20 s | PASS, seeked 20.000 | 210 / 322 | 85 | 43.1 ms | 48 / 238 ms | 238 MB |

- Plays, sound-clocked, to the end, with seeking; **but 1080p drops 30-44% of frames**. Decoding is on Venus (`on_venus` = all),
  so the decode thread's 43 ms is CPU it does not get: the frame's BGRA conversion, its 8 MB upload and swgl compositing the
  whole page every frame are the suspects; patch 0053 (no display list per frame) is in this build. Next: the per-thread
  profile (`tests/media/laptop/profile-laptop.sh`, not yet run on this build), then the YUV path into WebRender (patch C).
- Size: stripped Juno binary 106.19 MB with `media`, 100.29 MB without (+5.9 MB).
- Wikipedia idle (`LUCINANET_IDLE_TEST=20`, Servo article), one run each: 165 frames with `media`, 139 without; engine ticks
  12491 against 10567. Not yet repeated, so not yet read as a difference: the `media` run came straight after the video pages.
- MSE on the laptop (`mse-1080.html`) not yet run.

## Open

- `playbackRate` other than 1 is kept and not applied (needs the sound stretched).
- `media` is off in the Juno build (`scripts/build-juno.sh`): turning it on waits on the laptop numbers below.
- Not proved: a track buffer's byte count, which frames depend on a removed one, and that `join` is given the track's frames.
- Laptop: the YUV/BGRA and decode-nice A/B at 720p30, 1080p30 and 1080p60 (YouTube's itags 136, 137, 299), the MSE replay of
  the recorded YouTube session, then the caps set from it; power against the Video app; YouTube itself on Juno.
- MediaCapabilities (`decodingInfo`) is not in Servo; YouTube falls back to `isTypeSupported`, which the caps answer.
- DRM/EME is out of scope: no Widevine, so DRM-only videos and services do not play.
