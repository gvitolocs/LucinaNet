# Compat shims: web APIs the engine does not have yet

LucinaJS implements some web APIs and not others, and a page that calls a missing one at script load stops there.
`crates/lucinanet-servo/src/compat.js` is a plain script the engine injects as a user script at **document start, in
every frame, before the page's own scripts and before `LUCINANET_USER_JS` (a harness build only, `docs/harness-doors.md`)** (engine.rs, next to the user content
manager). It defines each API **only where the engine has none**, so a real implementation is never overridden, and
`LUCINANET_COMPAT_SHIMS=0` leaves every page exactly as it was. The script is ES5-level syntax on purpose (no arrow,
`class`, template string or block scoping): LucinaJS parses the file as one script, so one construct it would reject
takes the whole shim down, and only a page run shows that.

| What | What it does | What it does not do |
|---|---|---|
| `requestIdleCallback` / `cancelIdleCallback` | The callback runs on the ordinary timer queue about 1 ms later with `{didTimeout: false, timeRemaining: () => max(0, 50 - elapsed)}`; the id it returns cancels | There is no idle period to fill: `timeout` is accepted and never needed, `didTimeout` is always false, and work a page queues for idle time runs early instead of late |
| `scheduler.postTask(fn, {delay, signal, priority})` / `scheduler.yield()` | A promise per task on the timer queue, honouring `delay` and an `AbortSignal` (aborted before the task runs: rejects with `AbortError`; after it ran: the settled promise is left alone). A task that throws rejects its promise | One queue, so `priority` is accepted and means nothing |
| `PerformanceObserver.supportedEntryTypes`, `observe()` | Where the engine has no `supportedEntryTypes` it becomes `['mark', 'measure', 'resource', 'navigation', 'paint']`, and an `observe()` for a type outside that list is dropped here instead of reaching the engine | The engine has its own list and it is kept: measured, it answers eight types (`container`, `largest-contentful-paint`, `mark`, `measure`, `navigation`, `paint`, `resource`, `visibility-state`), and the filter reads *that* list per call, so nothing it does deliver is silenced. Its own `observe()` still runs every supported type; nothing new is measured. An unsupported `observe()` threw `InvalidModificationError` and logged `Warn: No valid entry type provided to observe()`, several times on a YouTube page |
| `document.all` | **Not a shim any more.** The pinned engine carries the real `HTMLAllCollection` object, so nothing here defines it — see the next section | Nothing: the row is kept only to say where the interim shim went |

## `document.all`: the engine has it, since this pin

The engine pinned here, at `40c0ff93cec46c2b962dac7273f75fa6b2f0e7e3`, carries `HTMLAllCollection`
and the `[[IsHTMLDDA]]` object itself, so `document.all` is the real thing: falsy, `typeof` it is `"undefined"`, and never `=== undefined` on any host (the underlying flaw is tracked privately).
The interim `compat.js` shim of it, with its `allHostSuffixes` host list, is removed; the sanitizer guard it stood in for stood down by itself the moment `document.all === undefined` stopped being true.

## Measured and dropped: `Animation` and `Element.prototype.animate`

These were shimmed, measured on youtube.com, and taken out again. Do not put them back without the numbers below.

With an `Animation` shim the page no longer threw `ReferenceError: Animation is not defined`
(`kevlar_base_module…:12331:612`) — it threw `ReferenceError: KeyframeEffect is not defined` (`…:3878:28`) one Web
Animations call later, because a shim that answers `typeof Animation == "function"` at document start also preempts
the page's own fallback: YouTube feature-tests `Animation` and, finding none, loads
`jsbin/web-animations-next-lite.min.vf`, which defines `Animation` and `KeyframeEffect` together. Measured with that
shim in place, the polyfill was requested in the runs without it and in none of the runs with it. Half a Web
Animations API is worse than none, and the visible result was identical either way (no result row is stamped in any
run), so the shim is gone and `Animation`, `KeyframeEffect` and `Element.prototype.animate` are left exactly as the
engine has them; with the shim removed the polyfill is back on both sides of the comparison, subject only to which end
of its own race a run lands on (table below).

A complete Web Animations shim — `Animation` plus `KeyframeEffect` plus `document.timeline`, or nothing — is the shape
that would be worth adding; it is not what this file does today.

### 2026-11-17: it is not a shim's job after all — the engine's own pref, flipped

The engine has always had the complete shape behind a preference that was off here: `dom_web_animations_enabled` gates
`Animation`, `KeyframeEffect`, `AnimationEffect` and the `animate()` member of `Animatable` together, in one place, so
they arrive together or not at all. It is now in `WEB_FEATURES` (`crates/lucinanet-servo/src/engine.rs`), the app-side
list this repo already keeps over Servo's defaults — no engine patch, and `LUCINANET_PREFS=dom_web_animations_enabled=false`
still puts any one run back the way it was.

Whether that is the row's cure is only decidable on the runs where the page's own loader loses its race, because that is
the only condition the row appears in. Eight runs each way on the search page, same binary:

| side | runs | polyfill requested | polyfill not requested | `Animation is not defined` among the not-requested | among the requested |
|---|---|---|---|---|---|
| pref off | 8 | 6 | 2 | **2 of 2** | 0 of 6 |
| pref on | 8 | 3 | 5 | **0 of 5** | 0 of 3 |

`ReferenceError: KeyframeEffect is not defined` — the failure this section is about — is in neither side's logs, and the
picture column does not move (0.33 % of the frame between the sides, against 6.6 % between two runs of one side).

One correction to the paragraph above, from the same measurement: `document.timeline` is *not* behind the pref
(`Document.webidl` declares it with no `Pref`, and it reads `object` with the pref off too), so the all-or-nothing
property belongs to the three gated names, which is precisely the three the rows are about. And what the flip buys is
the objects, not the motion: at this pin `Animation.play/pause/cancel/finish/currentTime` are commented out of the
webidl and `document.getAnimations()` does not exist, so a page that feature-tests `window.Animation` — which is all
this row ever was — is satisfied, and a page that plays an animation still gets nothing. Numbers, both probes and every
pair of PNGs: `<work-dir>/lucinanet/web-animations-pref/` (`analyze.py` prints them all from the run logs).

## Proof

Nothing in Rust can parse a script in a unit test, so the checks are a script and a page, both outside the repo:

- `<work-dir>/lucinanet/compat/check_compat.js` runs the file under node as a classic script and again under
  `"use strict"`, asserts each API's behaviour (including the `delay`, `signal` and second-run cases), asserts
  `Animation` and `Element.prototype.animate` are **not** defined and that a pre-existing API is left alone, and greps
  out the syntax LucinaJS may not have.
- `<work-dir>/lucinanet/compat/page/index.html`, rendered by the real binary with `--render`
  (`<work-dir>/lucinanet/compat/run-page.sh on|off <port>`), writes PASS/FAIL lines into its own body text for each API
  and is read back from the PNG. The same page with `LUCINANET_COMPAT_SHIMS=0` must show them absent, which proves the
  guard and the injection point at once; the page also prints what `LUCINANET_USER_JS` saw, which is the ordering.
- `<work-dir>/lucinanet/compat/run-yt.sh on|off <settle>` runs the YouTube search page both ways through the
  consent-reject harness (read-only there), with `measure.sh` and `below.py` for the numbers.

### The YouTube search page, three runs each way

Binary `sha256 693b37a879fc…` (compat.js without the Web Animations definitions), `https://www.youtube.com/results?
search_query=rust+programming`, 1366x900, quiet 6000 ms + hold 10 s, the consent-reject harness plus its
compat-watch script (part of the private test setup, not in this tree), `LUCINANET_COMPAT_SHIMS=0` for the off runs.
The polyfill column is the net log: this
build has no `GET request for` line, so it is `grep web-animations` over the `netphase … request …` lines.

| Run | Shims | console Error lines | `web-animations-next-lite` requested | `yt-video-renderer` | rows stamped | px below y=56 not background | `No valid entry type` Warn |
|---|---|---|---|---|---|---|---|
| 1 | on | 0 | yes | undefined | 0 | 2.26% (26044) | 0 |
| 1 | off | 0 | yes | undefined | 0 | 2.26% (26044) | 1 |
| 2 | on | 1 — ``ReferenceError: `Animation` is not defined``, `kevlar_base_module…:12331:612` | no | undefined | 0 | 2.26% (26044) | 0 |
| 2 | off | 1 — the same line | no | undefined | 0 | 2.26% (26044) | 1 |
| 3 | on | 1 — the same line | no | undefined | 0 | 2.26% (26044) | 0 |
| 3 | off | 0 | yes | undefined | 0 | 2.26% (26044) | 1 |

What that says: `Animation` is `undefined` at document start in all six runs, both ways, so the shim no longer stands
in front of the page's own fallback — and `KeyframeEffect` appears in none of the six logs, so the half-coverage error
is gone. Whether a run logs the `Animation` error now follows only whether YouTube's polyfill loader won its race
(requested in run 1 on, 1 off, 3 off; not in 2 on, 2 off, 3 on), which is run noise, not a shim effect. The shims'
own effect is the last column: the `Warn: No valid entry type provided to observe()` line is in every off run and in
no on run. Nothing stamps a result row either way, and the six PNGs are the same image — identical but for
`yt-off-1.png`, which differs from `yt-on-1.png` in 802 pixels with a maximum channel delta of 12, on the A/B
guide-rail line. Logs and pictures: `<work-dir>/lucinanet/compat/logs/yt-{on,off}-{1,2,3}.log`,
`png/yt-{on,off}-{1,2,3}.png`.
