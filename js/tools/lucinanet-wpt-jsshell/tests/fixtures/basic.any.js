// META: global=jsshell
// META: script=helper.js
// META: title=lucinanet-wpt-jsshell fixture

// The four results a run has to reproduce: a passing and a failing synchronous test, and
// a passing and a failing `promise_test`. The passing pair also check the environment the
// runner builds - `self`, the shell `location`, the META title and the META `script=` file
// - because a test that passes for the wrong reason proves nothing.

test(function () {
  assert_equals(fixtureDouble(21), 42, "the META script was loaded");
  assert_equals(self.META_TITLE, "lucinanet-wpt-jsshell fixture", "the META title reached the harness");
  assert_equals(typeof self.location, "object", "the shell has a location");
}, "sync pass");

test(function () {
  assert_equals(fixtureDouble(2), 5, "doubling");
}, "sync fail");

promise_test(function () {
  return Promise.resolve().then(function () {
    assert_true(self.location.pathname.length > 0, "location.pathname");
  });
}, "promise pass");

promise_test(function () {
  return new Promise(function (resolve, reject) {
    setTimeout(function () {
      reject(new Error("the timer fired and reported"));
    }, 10);
  });
}, "promise fail");
