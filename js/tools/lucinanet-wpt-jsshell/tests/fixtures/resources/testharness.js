// A stand-in for `resources/testharness.js`, small enough to read in one go.
//
// The runner drives the real WPT harness, which is 5,000 lines of browser-facing code a
// fixture test has no business depending on. What the runner *does* depend on is a
// handful of documented shapes, and this file reproduces exactly those: the status and
// phase numbers, `structured_clone()`, and the argument lists the callbacks are called
// with - `(test, tests)` for `add_test_state_callback` and `add_result_callback`,
// `(tests, status, asserts)` for `add_completion_callback`. Upstream:
// resources/testharness.js:2764-2830 (Test), 3583-3607 (TestsStatus), 3858-3920
// (Tests.push/notify_result/complete), 4222-4250 (the registrars).
(function (global) {
  "use strict";

  var STATUSES = { PASS: 0, FAIL: 1, TIMEOUT: 2, NOTRUN: 3, PRECONDITION_FAILED: 4 };
  var PHASES = { INITIAL: 0, STARTED: 1, HAS_RESULT: 2, CLEANING: 3, COMPLETE: 4 };

  function AssertionError(message) {
    this.name = "AssertionError";
    this.message = message;
  }
  AssertionError.prototype = Object.create(Error.prototype);

  function format(value) {
    if (typeof value === "string") {
      return JSON.stringify(value);
    }
    return String(value);
  }

  function Test(name, func) {
    this.name = name;
    this.func = func;
    this.phase = PHASES.INITIAL;
    this.status = STATUSES.NOTRUN;
    this.message = null;
    this.stack = null;
    this.index = null;
  }

  Test.prototype.structured_clone = function () {
    return {
      name: String(this.name),
      status: this.status,
      message: this.message,
      stack: this.stack,
      index: this.index,
      phase: this.phase,
    };
  };

  // The one place a result enters the harness: a test can only be settled once, and
  // settling it notifies the runner, which is what makes a run end.
  Test.prototype.set_status = function (status, message, stack) {
    if (this.phase >= PHASES.HAS_RESULT) {
      return;
    }
    this.status = status;
    this.message = message === undefined ? null : message;
    this.stack = stack === undefined ? null : stack;
    this.phase = PHASES.HAS_RESULT;
    tests.notify_result(this);
  };

  function Tests() {
    this.tests = [];
    this.num_pending = 0;
    this.phase = PHASES.INITIAL;
    this.status = { status: 0, message: null, stack: null };
    this.processing_callbacks = false;
    this.test_state_callbacks = [];
    this.test_done_callbacks = [];
    this.all_done_callbacks = [];
  }

  Tests.prototype.push = function (test) {
    test.index = this.tests.length;
    this.tests.push(test);
    this.num_pending += 1;
    this.notify_test_state(test);
  };

  Tests.prototype.notify_test_state = function (test) {
    var harness = this;
    this.test_state_callbacks.forEach(function (callback) {
      callback(test, harness);
    });
  };

  Tests.prototype.notify_result = function (test) {
    if (this.phase === PHASES.COMPLETE) {
      return;
    }
    this.num_pending -= 1;
    var harness = this;
    this.processing_callbacks = true;
    this.test_done_callbacks.forEach(function (callback) {
      callback(test, harness);
    });
    this.processing_callbacks = false;
    if (this.all_done()) {
      this.complete();
    }
  };

  Tests.prototype.all_done = function () {
    return (
      this.tests.length > 0 &&
      all_loaded &&
      this.num_pending === 0 &&
      !this.processing_callbacks
    );
  };

  Tests.prototype.complete = function () {
    if (this.phase === PHASES.COMPLETE) {
      return;
    }
    this.phase = PHASES.COMPLETE;
    var harness = this;
    this.all_done_callbacks.forEach(function (callback) {
      callback(harness.tests, harness.status, []);
    });
  };

  var tests = new Tests();

  // `ShellTestEnvironment` becomes ready one microtask after the harness is evaluated
  // (testharness.js:485-493), and `all_done` will not say true before that. Without the
  // deferral a run ends at the first synchronous result, while the test file is still
  // registering the tests after it.
  var all_loaded = false;
  Promise.resolve().then(function () {
    all_loaded = true;
    if (tests.all_done()) {
      tests.complete();
    }
  });

  function failure(e) {
    var message = e && e.message !== undefined ? String(e.message) : String(e);
    var stack = e && e.stack !== undefined ? String(e.stack) : null;
    return [message, stack];
  }

  function test(func, name, properties) {
    var this_test = new Test(name, func);
    tests.push(this_test);
    this_test.phase = PHASES.STARTED;
    try {
      func();
      this_test.set_status(STATUSES.PASS, null, null);
    } catch (e) {
      var reported = failure(e);
      this_test.set_status(STATUSES.FAIL, reported[0], reported[1]);
    }
    return this_test;
  }

  function promise_test(func, name, properties) {
    var this_test = new Test(name, func);
    tests.push(this_test);
    this_test.phase = PHASES.STARTED;
    var settled;
    try {
      settled = func(this_test);
    } catch (e) {
      var thrown = failure(e);
      this_test.set_status(STATUSES.FAIL, thrown[0], thrown[1]);
      return this_test;
    }
    Promise.resolve(settled).then(
      function () {
        this_test.set_status(STATUSES.PASS, null, null);
      },
      function (e) {
        var rejected = failure(e);
        this_test.set_status(STATUSES.FAIL, rejected[0], rejected[1]);
      }
    );
    return this_test;
  }

  // `make_message` upstream: `${function_name}: ${description}${detail}`, with the
  // description reduced to "" (and a trailing space) when it was not given.
  function message(function_name, description, detail) {
    return function_name + ": " + (description ? description + " " : "") + detail;
  }

  function assert_equals(actual, expected, description) {
    if (actual !== expected) {
      throw new AssertionError(message(
        "assert_equals",
        description,
        "expected " + format(expected) + " but got " + format(actual)
      ));
    }
  }

  function assert_true(value, description) {
    if (value !== true) {
      throw new AssertionError(message("assert_true", description, format(value) + " is not true"));
    }
  }

  function assert_false(value, description) {
    if (value !== false) {
      throw new AssertionError(message("assert_false", description, format(value) + " is not false"));
    }
  }

  function assert_unreached(description) {
    throw new AssertionError("assert_unreached: " + (description || "Reached unreachable code"));
  }

  function add_test_state_callback(callback) {
    tests.test_state_callbacks.push(callback);
  }

  function add_result_callback(callback) {
    tests.test_done_callbacks.push(callback);
  }

  function add_completion_callback(callback) {
    tests.all_done_callbacks.push(callback);
  }

  // The real harness reaches the shell global through `expose()`; here the names are
  // what the test file uses, so they go straight onto `self`.
  var exposed = {
    test: test,
    promise_test: promise_test,
    assert_equals: assert_equals,
    assert_true: assert_true,
    assert_false: assert_false,
    assert_unreached: assert_unreached,
    add_test_state_callback: add_test_state_callback,
    add_result_callback: add_result_callback,
    add_completion_callback: add_completion_callback,
    AssertionError: AssertionError,
    statuses: STATUSES,
    phases: PHASES,
  };
  Object.keys(exposed).forEach(function (name) {
    Object.defineProperty(global, name, {
      value: exposed[name],
      writable: true,
      configurable: true,
      enumerable: false,
    });
  });
})(typeof globalThis !== "undefined" ? globalThis : this);
