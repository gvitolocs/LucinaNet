/*---
description: >
  A `new` of a value that is not a constructor reports which expression was constructed, taken from
  the source text at the position the instruction reports.
---*/

function messageOf(action) {
  try {
    action();
  } catch (error) {
    return error.message;
  }
  return "<no error thrown>";
}

var notCtor = 1;
var holder = { Nested: {}, value: 1 };
var table = { "sc_initFeedback": 1 };

assert.sameValue(messageOf(function () { new notCtor(); }), "notCtor is not a constructor");
assert.sameValue(messageOf(function () { new holder.Nested(); }), "holder.Nested is not a constructor");
assert.sameValue(
  messageOf(function () { new holder.value.nested(); }),
  "holder.value.nested is not a constructor"
);
assert.sameValue(
  messageOf(function () { new table["sc_initFeedback"](); }),
  "table.sc_initFeedback is not a constructor"
);

// A constructor that is a plain function works, and is not reported at all.
function Ctor() { this.ok = true; }
assert.sameValue(new Ctor().ok, true);
