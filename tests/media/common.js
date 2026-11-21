// LucinaNet media test pages: every event of interest as a console line "media-test: ...", read by run.sh from the
// engine's stderr (LUCINANET_CONSOLE=1). The page ends with "media-test: PASS ..." or "media-test: FAIL ...".
function mediaTest(el, opts) {
  const t0 = performance.now();
  const ms = () => Math.round(performance.now() - t0);
  const log = (s) => console.log("media-test: " + s + " t=" + ms());
  let updates = 0, seeked = false, failed = false;
  const fail = (why) => { if (!failed) { failed = true; log("FAIL " + why); } };
  for (const e of ["loadstart", "loadedmetadata", "loadeddata", "canplay", "canplaythrough", "playing", "pause", "seeking", "waiting", "stalled"]) {
    el.addEventListener(e, () => log(e + " ready=" + el.readyState + " time=" + el.currentTime.toFixed(3)));
  }
  el.addEventListener("error", () => fail("error code=" + (el.error && el.error.code)));
  el.addEventListener("timeupdate", () => { updates++; });
  el.addEventListener("loadedmetadata", () => {
    log("metadata duration=" + el.duration.toFixed(3) + (el.videoWidth !== undefined ? " size=" + el.videoWidth + "x" + el.videoHeight : ""));
  });
  el.addEventListener("seeked", () => { seeked = true; log("seeked time=" + el.currentTime.toFixed(3)); });
  el.addEventListener("ended", () => {
    log("ended time=" + el.currentTime.toFixed(3) + " timeupdates=" + updates);
    if (el.getVideoPlaybackQuality) {
      const q = el.getVideoPlaybackQuality();
      log("quality total=" + q.totalVideoFrames + " dropped=" + q.droppedVideoFrames);
    }
    if (opts.seekTo !== undefined && !seeked) return fail("never seeked");
    if (updates < (opts.minUpdates || 4)) return fail("only " + updates + " timeupdates");
    log("PASS " + (opts.name || "") + " wall_ms=" + ms());
  });
  if (opts.seekTo !== undefined) {
    let asked = false;
    el.addEventListener("timeupdate", () => {
      if (!asked && el.currentTime > (opts.seekAfter || 1.0)) { asked = true; log("seek to " + opts.seekTo); el.currentTime = opts.seekTo; }
    });
  }
  log("canPlayType mp4/avc1+mp4a=" + el.canPlayType('video/mp4; codecs="avc1.64001e, mp4a.40.2"') +
      " webm/vp9+vorbis=" + el.canPlayType('video/webm; codecs="vp9, vorbis"') +
      " webm/vp9+opus=" + el.canPlayType('video/webm; codecs="vp9, opus"') + " mp3=" + el.canPlayType("audio/mpeg"));
  setTimeout(() => fail("timed out"), opts.timeoutMs || 30000);
  const p = el.play();
  if (p && p.catch) p.catch((e) => log("play() refused: " + e));
}
