/*---
description: >
  A call of a value that is not callable reports which expression was called, taken from the source
  text at the position the call instruction reports.
---*/

function messageOf(action) {
  try {
    action();
  } catch (error) {
    return error.message;
  }
  return "<no error thrown>";
}

var notAFunction;
var host = { method: undefined, value: 1 };
var table = { "sc_initFeedback": 1, "0": 1 };
var makeHost = function () { return host; };
var first = null;
var second = { z: 1 };
var receiver = {
  aaaaaaaaaaaaaaaaaa: {
    bbbbbbbbbbbbbbbbbb: {
      cccccccccccccccccc: { dddddddddddddddddd: {} },
    },
  },
};

// A plain name is written as it is.
assert.sameValue(messageOf(function () { notAFunction(); }), "notAFunction is not a function");

// A member access keeps its receiver.
assert.sameValue(messageOf(function () { host.method(); }), "host.method is not a function");
assert.sameValue(messageOf(function () { host.value.nested(); }), "host.value.nested is not a function");

// A computed key that spells a name is written as a member access; a key that does not, is not.
assert.sameValue(
  messageOf(function () { table["sc_initFeedback"](); }),
  "table.sc_initFeedback is not a function"
);
assert.sameValue(messageOf(function () { table[0](); }), "table[0] is not a function");

// The arguments of a call inside the chain collapse to an ellipsis.
assert.sameValue(
  messageOf(function () { makeHost("a", 2).method(); }),
  "makeHost(...).method is not a function"
);
// A call whose result is called names the callee that produced the result.
assert.sameValue(
  messageOf(function () { makeHost()()(); }),
  "makeHost(...) is not a function"
);

// A receiver that is not a name becomes an intermediate value.
assert.sameValue(
  messageOf(function () { (first || second).missing(); }),
  "(intermediate value).missing is not a function"
);
// A call of a value that has no name at all keeps the ellipsis of the call it came from.
assert.sameValue(messageOf(function () { (first || second)(); }), "(intermediate value) is not a function");
assert.sameValue(
  messageOf(function () { (function () { return 1; })()(); }),
  "(intermediate value)(...) is not a function"
);

// A name longer than 60 characters is cut, ellipsis included.
var longMessage = messageOf(function () {
  receiver.aaaaaaaaaaaaaaaaaa.bbbbbbbbbbbbbbbbbb.cccccccccccccccccc.dddddddddddddddddd.missing();
});
var longName = longMessage.slice(0, longMessage.length - " is not a function".length);
assert.sameValue(longName.length, 60);
assert.sameValue(longName.indexOf("receiver.aaaaaaaaaaaaaaaaaa"), 0);
assert.sameValue(longName.slice(-3), "...");

// An optional call does not reach the check at all.
assert.sameValue(messageOf(function () { host.method?.(); }), "<no error thrown>");
