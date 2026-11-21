// META: global=jsshell

// A result the harness itself never reports: the wall clock ends the run, and what the
// runner collects is the snapshot it takes at that moment. The test that started and
// never settled becomes TIMEOUT, and a test that finished after the hang keeps its own
// result instead of disappearing with the run.

test(function () {
  assert_true(true);
}, "hang: before the hang");

promise_test(function () {
  return new Promise(function () {});
}, "hang: never settles");

test(function () {
  assert_equals(1 + 1, 2);
}, "hang: after the hang");
