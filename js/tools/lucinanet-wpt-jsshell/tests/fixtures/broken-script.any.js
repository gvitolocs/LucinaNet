// META: global=jsshell
// META: script=throwing.js
// META: title=a helper the engine cannot load

// `wasm/jsapi/js-string/basic.any.js` in the real corpus: the failure is in a `script=`
// file, so the test body never runs. The engine gap is the reason, and it belongs to the
// file, not to the runner.

test(function () {
  assert_unreached("the helper above never loaded, so this file is never evaluated");
}, "broken script: must not run");
