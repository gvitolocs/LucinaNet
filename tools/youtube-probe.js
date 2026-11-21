// LucinaNet YouTube probe (LUCINANET_USER_JS=tools/youtube-probe.js with LUCINANET_CONSOLE=1).
// It only watches: errors, rejections, the requests the player makes to googlevideo, and the <video> element's state
// every 2 s. It never touches tokens, BotGuard or the player's requests; it records what YouTube's own code does.
(() => {
  if (window.__lucinaProbe) return;
  window.__lucinaProbe = true;
  const t0 = performance.now();
  const ms = () => Math.round(performance.now() - t0);
  const say = (what) => console.log(`ytprobe ${ms()} ${location.host} ${what}`);
  const nav = () => Math.round(performance.now());
  const seen = new Set();
  const once = (key, what) => { if (!seen.has(key)) { seen.add(key); say(`bd ${key} nav=${nav()} ${what || ""}`); } };
  say(`start ${location.href.slice(0, 100)} MediaSource=${typeof MediaSource} ManagedMediaSource=${typeof ManagedMediaSource}`);
  addEventListener("error", (e) => say(`error ${e.message} ${String(e.filename).slice(-60)}:${e.lineno}`));
  addEventListener("unhandledrejection", (e) => say(`rejection ${String(e.reason && (e.reason.stack || e.reason)).slice(0, 300)}`));
  const watch = (url) => /googlevideo|videoplayback|sabr|youtubei\/v1\/player|\/generate_204|botguard|bg\//.test(url);
  const realFetch = window.fetch;
  if (realFetch) {
    window.fetch = function (input, init) {
      const url = String(input && input.url || input);
      const p = realFetch.apply(this, arguments);
      if (/googlevideo/.test(url)) {
        once("googlevideo-sent");
        p.then(() => once("googlevideo-response"), () => {});
      }
      if (watch(url)) {
        const method = (init && init.method) || (input && input.method) || "GET";
        p.then((r) => say(`fetch ${method} ${r.status} ${url.slice(0, 110)}`), (e) => say(`fetch ${method} FAILED ${url.slice(0, 110)} ${e}`));
      }
      return p;
    };
  }
  const open = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url) {
    if (watch(String(url))) {
      this.addEventListener("loadend", () => say(`xhr ${method} ${this.status} ${String(url).slice(0, 110)}`));
    }
    return open.apply(this, arguments);
  };
  if (window.MediaSource && MediaSource.prototype.addSourceBuffer) {
    const add = MediaSource.prototype.addSourceBuffer;
    MediaSource.prototype.addSourceBuffer = function (type) {
      say(`addSourceBuffer ${type} supported=${MediaSource.isTypeSupported(type)}`);
      const sb = add.apply(this, arguments);
      try { sb.__lucinaType = String(type).split("/")[0]; } catch (e) {}
      return sb;
    };
    const append = SourceBuffer.prototype.appendBuffer;
    SourceBuffer.prototype.appendBuffer = function (data) {
      const kind = this.__lucinaType || "?";
      once(`first-append-${kind}`, `bytes=${data && data.byteLength}`);
      if (!this.__lucinaEnd) {
        this.__lucinaEnd = true;
        this.addEventListener("updateend", () => once(`first-updateend-${kind}`), { once: true });
      }
      return append.apply(this, arguments);
    };
  }
  // Where the time to first frame goes (lines start "bd"): navigation timing, each external script's fetch and its load event
  // (fetch end to load event = waiting + parse + execute), main-thread gaps over 250 ms (the script thread was busy), the
  // player API appearing, the first googlevideo request and response, the first MSE append per track.
  document.addEventListener("load", (e) => {
    const s = e.target;
    if (!s || s.tagName !== "SCRIPT" || !s.src) return;
    const r = performance.getEntriesByName(s.src)[0];
    const fetch = r ? ` fetch=${Math.round(r.startTime)}-${Math.round(r.responseEnd)} bytes=${r.encodedBodySize || r.transferSize || "?"}` : "";
    say(`bd script-done nav=${nav()} ${s.src.split("?")[0].split("/").slice(-2).join("/").slice(-60)}${fetch}`);
  }, true);
  document.addEventListener("DOMContentLoaded", () => once("dcl"));
  addEventListener("load", () => {
    once("load");
    const n = performance.getEntriesByType("navigation")[0];
    const r = (x) => Math.round(x || 0);
    if (n) say(`bd navtiming dns=${r(n.domainLookupStart)}-${r(n.domainLookupEnd)} connect=${r(n.connectStart)}-${r(n.connectEnd)} ` +
      `tls=${r(n.secureConnectionStart)} request=${r(n.requestStart)} response=${r(n.responseStart)}-${r(n.responseEnd)} ` +
      `interactive=${r(n.domInteractive)} dcl=${r(n.domContentLoadedEventEnd)}`);
  });
  let beat = performance.now();
  const gaps = setInterval(() => {
    const now = performance.now();
    if (now - beat > 250) say(`bd gap ${Math.round(beat)}-${Math.round(now)} ${Math.round(now - beat)}ms`);
    beat = now;
    if (now > 40000) clearInterval(gaps);
  }, 50);
  const playerPoll = setInterval(() => {
    const p = document.querySelector("#movie_player");
    if (p && p.getPlayerState) { once("player-api"); clearInterval(playerPoll); }
  }, 50);
  // The moments that make "time to video": the element exists, it starts playing, its clock first moves.
  const marks = setInterval(() => {
    const v = document.querySelector("video");
    if (!v || v.__lucinaMarks) return;
    v.__lucinaMarks = true;
    say(`mark video-element nav=${Math.round(performance.now())}`);
    v.addEventListener("playing", () => say(`mark playing nav=${Math.round(performance.now())} t=${v.currentTime.toFixed(2)}`), { once: true });
    const moved = () => { if (v.currentTime > 0) { say(`mark first-time nav=${Math.round(performance.now())} t=${v.currentTime.toFixed(2)}`); v.removeEventListener("timeupdate", moved); } };
    v.addEventListener("timeupdate", moved);
    // Who stops playback: every pause, wait and stall with the player's own state at that moment.
    const player = () => { const p = document.querySelector("#movie_player"); return p && p.getPlayerState ? p.getPlayerState() : "?"; };
    for (const ev of ["pause", "waiting", "stalled", "ended", "error", "emptied"]) {
      v.addEventListener(ev, () => say(`mark ${ev} nav=${Math.round(performance.now())} t=${v.currentTime.toFixed(2)} ` +
        `ready=${v.readyState} playerState=${player()} hidden=${document.hidden} focus=${document.hasFocus && document.hasFocus()}`));
    }
    clearInterval(marks);
  }, 100);
  setInterval(() => {
    const v = document.querySelector("video");
    if (!v) { say("video none"); return; }
    let buffered = "";
    try { for (let i = 0; i < v.buffered.length; i++) buffered += `[${v.buffered.start(i).toFixed(1)}-${v.buffered.end(i).toFixed(1)}]`; } catch (e) { buffered = "err"; }
    const q = v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality() : null;
    const err = v.error ? `${v.error.code}:${v.error.message}` : "-";
    const player = document.querySelector("#movie_player");
    const state = player && player.getPlayerState ? player.getPlayerState() : "?";
    say(`video t=${v.currentTime.toFixed(2)} dur=${v.duration} paused=${v.paused} ready=${v.readyState} net=${v.networkState} err=${err} ` +
      `buf=${buffered} ${v.videoWidth}x${v.videoHeight} frames=${q ? q.totalVideoFrames + "/" + q.droppedVideoFrames : "?"} ` +
      `src=${String(v.src).slice(0, 40)} playerState=${state}`);
  }, 2000);
})();
