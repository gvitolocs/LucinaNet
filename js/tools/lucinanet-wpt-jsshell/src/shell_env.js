// The shell environment testharness.js expects.
//
// Evaluated in the test realm *before* resources/testharness.js, which picks
// `ShellTestEnvironment` when the global has no `document` and is not a worker global
// (resources/testharness.js:523-553). That environment is called with `self` (the file
// ends with `})(self);`), needs `Promise` (ShellTestEnvironment:485-497), deliberately
// has no default test timeout (:517-521) and falls back to a promise-spin timeout when
// `setTimeout` is missing (:1253-1281). `setTimeout` is provided here instead, as a
// registry the runner drains, so a real deadline is used and no spin is needed.
//
// Nothing here is enumerable-sensitive: testharness only asks whether a name is `in`
// the global, so `document`, the worker globals and `GLOBAL` stay undefined and the
// shell environment is the one that is chosen.
(function () {
  var global = typeof globalThis !== "undefined" ? globalThis : this;

  function define(name, value) {
    Object.defineProperty(global, name, {
      value: value,
      writable: true,
      configurable: true,
      enumerable: true,
    });
  }

  define("self", global);

  // Text to the runner's stderr, never to stdout: stdout carries only the JSONL.
  function stringify(arg) {
    try {
      return typeof arg === "object" && arg !== null && arg.stack !== undefined
        ? String(arg.stack)
        : String(arg);
    } catch (e) {
      return "<unprintable>";
    }
  }

  // An array-like of arguments, joined as console methods present them.
  function logArgs(level, args) {
    var parts = [];
    for (var i = 0; i < args.length; i++) {
      parts.push(stringify(args[i]));
    }
    __wptLog(level, parts.join(" "));
  }

  define("print", function () {
    logArgs("print", arguments);
    return undefined;
  });

  define("console", {
    log: function () { logArgs("log", arguments); },
    info: function () { logArgs("info", arguments); },
    warn: function () { logArgs("warn", arguments); },
    error: function () { logArgs("error", arguments); },
    debug: function () { logArgs("debug", arguments); },
    trace: function () { logArgs("trace", arguments); },
  });

  // The timer queue. `setTimeout` returns an id and `clearTimeout` removes the entry,
  // as testharness's `Test.prototype.done` and `step_timeout` expect. The runner calls
  // `__wptRunTimers` between job drains; a callback that throws is reported through
  // `__wptError` rather than propagated, so one bad timer cannot stop the loop.
  var timers = [];
  var seq = 0;

  define("setTimeout", function (fn, ms) {
    var id = ++seq;
    var delay = typeof ms === "number" && ms > 0 ? ms : 0;
    timers.push({ id: id, fn: fn, at: Date.now() + delay });
    return id;
  });

  define("clearTimeout", function (id) {
    for (var i = 0; i < timers.length; i++) {
      if (timers[i].id === id) {
        timers.splice(i, 1);
        return;
      }
    }
  });

  define("__wptRunTimers", function () {
    var now = Date.now();
    var due = [];
    for (var i = timers.length - 1; i >= 0; i--) {
      if (timers[i].at <= now) {
        due.push(timers.splice(i, 1)[0]);
      }
    }
    // Due order, then registration order: the same sequence a browser would run them.
    due.sort(function (a, b) {
      return a.at - b.at || a.id - b.id;
    });
    for (var j = 0; j < due.length; j++) {
      var fn = due[j].fn;
      try {
        if (typeof fn === "function") {
          fn();
        } else {
          __wptError("setTimeout with a string callback is not supported");
        }
      } catch (e) {
        __wptError(stringify(e));
      }
    }
    return due.length;
  });

  // Milliseconds until the earliest waiting timer, or -1 when none is. The runner
  // sleeps that long (capped) instead of spinning.
  define("__wptNextTimer", function () {
    if (timers.length === 0) {
      return -1;
    }
    var now = Date.now();
    var best = -1;
    for (var i = 0; i < timers.length; i++) {
      var d = timers[i].at - now;
      if (d < 0) {
        d = 0;
      }
      if (best < 0 || d < best) {
        best = d;
      }
    }
    return best;
  });
})();
