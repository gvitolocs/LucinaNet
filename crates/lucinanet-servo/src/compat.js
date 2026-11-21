// LucinaNet compat shims (docs/compat-shims.md): the web APIs LucinaJS does not implement that real pages call
// without feature-testing. The engine injects this as a user script at document start, in every frame, before the
// page's own scripts and before `LUCINANET_USER_JS`; `LUCINANET_COMPAT_SHIMS=0` turns it off.
// Every definition is guarded, so an API the engine does have is left exactly as it is.
// Everything below stays at ES5 level syntax (var, function expressions, no arrow, no class, no template string,
// no block scoping) on purpose: LucinaJS parses this file as one script, so a single construct it does not accept
// would take the whole shim down, and only a page run would show it.
(function (global) {
  if (!global) { return; }

  // --- requestIdleCallback / cancelIdleCallback ---
  // There is no idle scheduler, so a queued callback is handed to the ordinary timer queue about a millisecond later
  // and is never reported as having timed out; the `timeout` option is accepted and never needed.
  var idleTimers = {};
  var idleNext = 1;
  if (typeof global.requestIdleCallback === "undefined") {
    global.requestIdleCallback = function (callback, options) {
      var id = idleNext++;
      var start = Date.now();
      idleTimers[id] = setTimeout(function () {
        delete idleTimers[id];
        if (typeof callback !== "function") { return; }
        callback({
          didTimeout: false,
          timeRemaining: function () { return Math.max(0, 50 - (Date.now() - start)); }
        });
      }, 1);
      return id;
    };
  }
  if (typeof global.cancelIdleCallback === "undefined") {
    global.cancelIdleCallback = function (id) {
      var timer = idleTimers[id];
      if (timer) { clearTimeout(timer); delete idleTimers[id]; }
    };
  }

  // --- scheduler.postTask / scheduler.yield ---
  // Tasks run on the timer queue, honouring `delay` and an AbortSignal; `priority` is accepted and has no meaning
  // with one queue. A task that throws rejects its promise, as the API does.
  if (typeof global.scheduler === "undefined") {
    var queued = function (run, options) {
      return new Promise(function (resolve, reject) {
        var delay = options && typeof options.delay === "number" && options.delay > 0 ? options.delay : 0;
        var signal = options ? options.signal : null;
        var timer = null;
        var onAbort = null;
        var settled = false;
        var detach = function () {
          if (signal && onAbort && signal.removeEventListener) { signal.removeEventListener("abort", onAbort); }
        };
        var abort = function () {
          if (settled) { return; }
          settled = true;
          if (timer) { clearTimeout(timer); timer = null; }
          detach();
          // The API rejects with an AbortError DOMException; an Error carrying the name is the closest this reaches.
          var error = new Error("The user aborted a request.");
          error.name = "AbortError";
          reject(error);
        };
        if (signal && signal.aborted) { abort(); return; }
        timer = setTimeout(function () {
          timer = null;
          if (settled) { return; }
          settled = true;
          detach();
          if (!run) { resolve(undefined); return; }
          try { resolve(run()); } catch (error) { reject(error); }
        }, delay);
        if (signal && signal.addEventListener) {
          onAbort = abort;
          signal.addEventListener("abort", onAbort);
        }
      });
    };
    global.scheduler = {
      postTask: function (callback, options) {
        return queued(function () {
          return typeof callback === "function" ? callback() : callback;
        }, options);
      },
      yield: function (options) { return queued(null, options); }
    };
  }

  // Animation and Element.prototype.animate are deliberately NOT shimmed: measured on youtube.com a
  // document-start `Animation` preempts the page's own fallback (it feature-tests `Animation` and loads
  // jsbin/web-animations-next-lite, which defines `Animation` and `KeyframeEffect` together), and the page
  // then dies on `KeyframeEffect is not defined` instead. Half a Web Animations API is worse than none; see
  // docs/compat-shims.md.

  // --- PerformanceObserver ---
  // An observe() for an entry type the engine does not have logs `Warn: No valid entry type provided to observe()`
  // and throws InvalidModificationError, which is what a page sees as an error line on every such call. Here the
  // types outside the engine's own list are dropped first, so the call never reaches it; inside the list the
  // engine's own observe is what runs. The list is read per call because it is the engine's, not ours.
  if (typeof global.PerformanceObserver === "function" && global.PerformanceObserver.prototype) {
    var FALLBACK = ["mark", "measure", "resource", "navigation", "paint"];
    var observer = global.PerformanceObserver;
    var realObserve = observer.prototype.observe;
    if (typeof observer.supportedEntryTypes === "undefined") {
      try {
        Object.defineProperty(observer, "supportedEntryTypes", {
          get: function () { return FALLBACK.concat(); },
          configurable: true
        });
      } catch (ignored) {
        try { observer.supportedEntryTypes = FALLBACK.concat(); } catch (alsoIgnored) {}
      }
    }
    if (typeof realObserve === "function" && realObserve.__lucinaFiltered !== 1) {
      var filteredObserve = function (options) {
        var isKnown = function (name) {
          var supported = observer.supportedEntryTypes || FALLBACK;
          for (var s = 0; s < supported.length; s++) {
            if (supported[s] === name) { return true; }
          }
          return false;
        };
        var list = options && typeof options.entryTypes === "object" && typeof options.entryTypes.length === "number"
          ? options.entryTypes : null;
        var call = options;
        if (list) {
          var kept = [];
          for (var i = 0; i < list.length; i++) {
            if (isKnown(list[i])) { kept.push(list[i]); }
          }
          if (kept.length === 0) { return; }
          // A copy of the dictionary, so the page's own array and object stay untouched.
          call = { entryTypes: kept };
          if (options.buffered !== undefined) { call.buffered = options.buffered; }
        } else if (options && options.type && !isKnown(options.type)) {
          return;
        }
        try {
          return call === options ? realObserve.apply(this, arguments) : realObserve.call(this, call);
        } catch (ignored) {}
      };
      // The marker on the function is what keeps a second run of this file from wrapping the wrapper.
      filteredObserve.__lucinaFiltered = 1;
      observer.prototype.observe = filteredObserve;
    }
  }
})(typeof window !== "undefined" ? window : (typeof globalThis !== "undefined" ? globalThis : this));
