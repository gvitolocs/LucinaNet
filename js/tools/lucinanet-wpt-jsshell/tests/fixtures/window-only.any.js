// META: global=window,dedicatedworker
// META: title=not a shell test

// No jsshell in the global list, so the runner never evaluates it: `sourcefile.py:113`
// maps the `jsshell` variant to the `.any.js` file only when the test declares it.

test(function () {
  assert_unreached("a shell run must not evaluate this");
}, "window-only: must not run");
