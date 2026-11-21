/*---
description: >
  The [[IsHTMLDDA]] slot reaches only typeof, ToBoolean and IsLooselyEqual. The promise machinery
  decides with IsCallable on `then`, so an object that emulates undefined is still adopted as an
  ordinary callable thenable, by Promise.resolve and by await alike.
features: [IsHTMLDDA]
flags: [async]
---*/

var x = $262.IsHTMLDDA;
var thenCalls = 0;

x.then = function (onFulfilled) {
  thenCalls += 1;
  onFulfilled("adopted");
};

assert.sameValue(typeof x.then, "function", "an own `then` is readable on the emulating object");

// `await` reads the same slot-free test: a falsy-looking but callable value is still awaited.
async function awaitIt() {
  return await x;
}

Promise.resolve(x)
  .then(function (value) {
    assert.sameValue(value, "adopted", "Promise.resolve adopts the emulating thenable");
    assert.sameValue(thenCalls, 1, "its `then` ran once");
    return awaitIt();
  })
  .then(function (value) {
    assert.sameValue(value, "adopted", "await adopts it the same way");
    assert.sameValue(thenCalls, 2, "`then` ran again for the await");
    assert.sameValue(typeof x, "undefined", "the slot survives the added own property");
    assert.sameValue(Boolean(x), false, "and still answers ToBoolean while the promise runs");
  })
  .then($DONE, $DONE);
