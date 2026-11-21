/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use log::warn;
use malloc_size_of_derive::MallocSizeOf;
use serde::{Deserialize, Serialize};
use servo_base::cross_process_instant::CrossProcessInstant;
use servo_base::generic_channel::GenericSender;
use time::Duration;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TimerMetadata {
    pub url: String,
    pub iframe: TimerMetadataFrameType,
    pub incremental: TimerMetadataReflowType,
}

#[derive(Clone, Debug, Deserialize, Serialize, MallocSizeOf)]
pub struct ProfilerChan(pub Option<GenericSender<ProfilerMsg>>);

impl ProfilerChan {
    pub fn send(&self, msg: ProfilerMsg) {
        if let Some(sender) = &self.0
            && let Err(e) = sender.send(msg)
        {
            warn!("Error communicating with the time profiler thread: {}", e);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum ProfilerData {
    NoRecords,
    Record(Vec<Duration>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum ProfilerMsg {
    /// Normal message used for reporting time
    Time(
        (ProfilerCategory, Option<TimerMetadata>),
        (CrossProcessInstant, CrossProcessInstant),
    ),
    /// Message used to get time spend entries for a particular ProfilerBuckets (in nanoseconds)
    Get(
        (ProfilerCategory, Option<TimerMetadata>),
        GenericSender<ProfilerData>,
    ),
    /// Message used to force print the profiling metrics
    Print,

    /// Report a layout query that could not be processed immediately for a particular URL.
    BlockedLayoutQuery(String),

    /// Tells the profiler to shut down.
    Exit(GenericSender<()>),
}

/// Usage sites of variants marked “Rust tracing only” are not visible to rust-analyzer.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ProfilerCategory {
    /// `Paint` is rasterising or presenting.
    ///
    /// Not associated with a specific URL.
    Painting = 0x00,

    /// The script thread is doing layout work.
    Layout = 0x10,

    ImageSaving = 0x51,
    ScriptSpawnPipeline = 0x60,
    ScriptConstellationMsg = 0x61,
    ScriptDevtoolsMsg = 0x62,
    ScriptDocumentEvent = 0x63,

    /// Rust tracing only: the script thread is executing a script.
    /// This may include time doing layout or parse work initiated by the script.
    ScriptEvaluate = 0x65,

    ScriptEvent = 0x66,
    ScriptFileRead = 0x67,
    ScriptFontLoading = 0x68,
    ScriptImageCacheMsg = 0x69,
    ScriptInputEvent = 0x6a,
    ScriptNetworkEvent = 0x6b,

    /// The script thread is parsing HTML, rather than doing other work like evaluating scripts or doing layout.
    ScriptParseHTML = 0x6c,

    ScriptPlannedNavigation = 0x6d,
    ScriptResize = 0x6e,
    ScriptRendering = 0x6f,
    ScriptSetScrollState = 0x70,
    ScriptSetViewport = 0x71,
    ScriptTimerEvent = 0x72,
    ScriptStylesheetLoad = 0x73,
    ScriptUpdateReplacedElement = 0x74,
    ScriptWebSocketEvent = 0x75,
    ScriptWorkerEvent = 0x76,
    ScriptServiceWorkerEvent = 0x77,

    /// The script thread is parsing XML, rather than doing other work like evaluating scripts or doing layout.
    ScriptParseXML = 0x78,

    ScriptEnterFullscreen = 0x79,
    ScriptExitFullscreen = 0x7a,
    ScriptWorkletEvent = 0x7b,
    ScriptGeolocationEvent = 0x7c,
    ScriptPerformanceEvent = 0x7d,
    ScriptNavigationAndTraversalEvent = 0x7e,
    ScriptPortMessage = 0x7f,
    ScriptWebGPUMsg = 0x80,

    ScriptDatabaseAccessEvent = 0x81,

    /// Web performance metrics.
    TimeToFirstPaint = 0x90,
    TimeToFirstContentfulPaint = 0x91,
    TimeToInteractive = 0x92,
    TimeToLargestContentfulPaint = 0x93,

    IpcReceiver = 0x94,
    IpcBytesReceiver = 0x95,
}

impl ProfilerCategory {
    pub const fn variant_name(&self) -> &'static str {
        match self {
            ProfilerCategory::Painting => "Painting",
            ProfilerCategory::Layout => "Layout",
            ProfilerCategory::ImageSaving => "ImageSaving",
            ProfilerCategory::ScriptSpawnPipeline => "ScriptSpawnPipeline",
            ProfilerCategory::ScriptConstellationMsg => "ScriptConstellationMsg",
            ProfilerCategory::ScriptDatabaseAccessEvent => "ScriptDatabaseAccessEvent",
            ProfilerCategory::ScriptDevtoolsMsg => "ScriptDevtoolsMsg",
            ProfilerCategory::ScriptDocumentEvent => "ScriptDocumentEvent",
            ProfilerCategory::ScriptEvaluate => "ScriptEvaluate",
            ProfilerCategory::ScriptEvent => "ScriptEvent",
            ProfilerCategory::ScriptFileRead => "ScriptFileRead",
            ProfilerCategory::ScriptFontLoading => "ScriptFontLoading",
            ProfilerCategory::ScriptGeolocationEvent => "ScriptGeolocationEvent",
            ProfilerCategory::ScriptImageCacheMsg => "ScriptImageCacheMsg",
            ProfilerCategory::ScriptInputEvent => "ScriptInputEvent",
            ProfilerCategory::ScriptNetworkEvent => "ScriptNetworkEvent",
            ProfilerCategory::ScriptParseHTML => "ScriptParseHTML",
            ProfilerCategory::ScriptPlannedNavigation => "ScriptPlannedNavigation",
            ProfilerCategory::ScriptRendering => "ScriptRendering",
            ProfilerCategory::ScriptResize => "ScriptResize",
            ProfilerCategory::ScriptSetScrollState => "ScriptSetScrollState",
            ProfilerCategory::ScriptSetViewport => "ScriptSetViewport",
            ProfilerCategory::ScriptTimerEvent => "ScriptTimerEvent",
            ProfilerCategory::ScriptStylesheetLoad => "ScriptStylesheetLoad",
            ProfilerCategory::ScriptUpdateReplacedElement => "ScriptUpdateReplacedElement",
            ProfilerCategory::ScriptWebSocketEvent => "ScriptWebSocketEvent",
            ProfilerCategory::ScriptWorkerEvent => "ScriptWorkerEvent",
            ProfilerCategory::ScriptServiceWorkerEvent => "ScriptServiceWorkerEvent",
            ProfilerCategory::ScriptParseXML => "ScriptParseXML",
            ProfilerCategory::ScriptEnterFullscreen => "ScriptEnterFullscreen",
            ProfilerCategory::ScriptExitFullscreen => "ScriptExitFullscreen",
            ProfilerCategory::ScriptWorkletEvent => "ScriptWorkletEvent",
            ProfilerCategory::ScriptPerformanceEvent => "ScriptPerformanceEvent",
            ProfilerCategory::ScriptNavigationAndTraversalEvent => {
                "ScriptNavigationAndTraversalEvent"
            },
            ProfilerCategory::ScriptPortMessage => "ScriptPortMessage",
            ProfilerCategory::ScriptWebGPUMsg => "ScriptWebGPUMsg",
            ProfilerCategory::TimeToFirstPaint => "TimeToFirstPaint",
            ProfilerCategory::TimeToFirstContentfulPaint => "TimeToFirstContentfulPaint",
            ProfilerCategory::TimeToLargestContentfulPaint => "TimeToLargestContentfulPaint",
            ProfilerCategory::TimeToInteractive => "TimeToInteractive",
            ProfilerCategory::IpcReceiver => "IpcReceiver",
            ProfilerCategory::IpcBytesReceiver => "IpcBytesReceiver",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, MallocSizeOf, Ord, PartialEq, PartialOrd, Serialize)]
pub enum TimerMetadataFrameType {
    RootWindow,
    IFrame,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum TimerMetadataReflowType {
    Incremental,
    FirstReflow,
}

#[cfg(feature = "tracing")]
pub type Span = tracing::Span;
#[cfg(not(feature = "tracing"))]
pub type Span = ();

pub fn profile<T, F>(
    category: ProfilerCategory,
    meta: Option<TimerMetadata>,
    profiler_chan: ProfilerChan,
    #[cfg(feature = "tracing")] span: Span,
    #[cfg(not(feature = "tracing"))] _span: Span,
    callback: F,
) -> T
where
    F: FnOnce() -> T,
{
    let start_time = CrossProcessInstant::now();
    let val = {
        #[cfg(feature = "tracing")]
        let _enter = span.enter();
        callback()
    };
    let end_time = CrossProcessInstant::now();

    send_profile_data(category, meta, &profiler_chan, start_time, end_time);
    val
}

/// LucinaNet: `LUCINANET_VIDEO_TRACE=1` prints the steps that decide whether a `<video>` box gets
/// a picture at all — the first frame, the image key it installs, the display list that names it,
/// the layout read of that key — on stderr, on one clock (epoch milliseconds) so that the script,
/// layout and paint threads interleave in the order the events really happened. Silent otherwise,
/// and the format arguments are not built.
pub fn video_tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LUCINANET_VIDEO_TRACE").is_some())
}

/// The clock `video_trace!` stamps with: milliseconds since the Unix epoch.
pub fn video_trace_clock() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or(0)
}

#[macro_export]
macro_rules! video_trace {
    ($($arg:tt)*) => {
        if $crate::time::video_tracing() {
            eprintln!(
                "lucinanet-video: {} {}",
                $crate::time::video_trace_clock(),
                format_args!($($arg)*)
            );
        }
    };
}

pub fn send_profile_data(
    category: ProfilerCategory,
    meta: Option<TimerMetadata>,
    profiler_chan: &ProfilerChan,
    start_time: CrossProcessInstant,
    end_time: CrossProcessInstant,
) {
    profiler_chan.send(ProfilerMsg::Time((category, meta), (start_time, end_time)));
}
