// The result-collecting half of the harness.
//
// Evaluated after resources/testharness.js (so the `add_*_callback` registrars exist)
// and before the test's `script=` files, so a failure while loading one of them is
// still reported against the tests that were registered. Only documented WPT API is
// used: `add_result_callback` and `add_test_state_callback` are called as
// `(test, tests)` and `add_completion_callback` as `(tests, harness_status, asserts)`
// (resources/testharness.js:3869-3871, 3907-3914, 4064-4069).
(function () {
  var global = typeof globalThis !== "undefined" ? globalThis : this;

  // The harness `Tests` object, captured from the first registered test. It is what
  // makes a snapshot possible when the wall clock runs out and no completion callback
  // ever arrives.
  var testsObject = null;
  var harnessStatus = null;

  function clone(test) {
    var c = typeof test.structured_clone === "function" ? test.structured_clone() : test;
    var message = c.message === undefined || c.message === null ? null : String(c.message);
    if (message === null && c.stack !== undefined && c.stack !== null) {
      message = String(c.stack);
    }
    return {
      index: typeof c.index === "number" ? c.index : null,
      name: String(c.name),
      status: c.status === undefined ? null : c.status,
      phase: c.phase === undefined ? null : c.phase,
      message: message,
    };
  }

  function snapshot() {
    var list = [];
    if (testsObject) {
      var tests = testsObject.tests;
      for (var i = 0; i < tests.length; i++) {
        list.push(clone(tests[i]));
      }
    }
    return { harness: harnessStatus, tests: list };
  }

  if (typeof add_test_state_callback === "function") {
    add_test_state_callback(function (test, tests) {
      if (!testsObject && tests && tests.tests) {
        testsObject = tests;
      }
    });
  }

  if (typeof add_result_callback === "function") {
    add_result_callback(function (test, tests) {
      if (!testsObject && tests && tests.tests) {
        testsObject = tests;
      }
      __wptResult(JSON.stringify(clone(test)));
    });
  }

  if (typeof add_completion_callback === "function") {
    add_completion_callback(function (tests, status) {
      harnessStatus = {
        status: status.status,
        message: status.message === undefined || status.message === null
          ? null
          : String(status.message),
      };
      var list = [];
      for (var i = 0; i < tests.length; i++) {
        list.push(clone(tests[i]));
      }
      __wptComplete(JSON.stringify({ harness: harnessStatus, tests: list }));
    });
  } else {
    // A harness without the hook is a runner error, not a test result: without it the
    // only way to end the run is the wall clock.
    __wptError("this harness has no add_completion_callback");
  }

  Object.defineProperty(global, "__wptSnapshot", {
    value: function () {
      return JSON.stringify(snapshot());
    },
    writable: true,
    configurable: true,
  });
})();
