/*---
description: >
  The RegExp legacy static properties have no value until a regular expression of the realm has
  matched, and reading them before that throws a TypeError.
---*/

var other = $262.createRealm().global;

function inOtherRealm(text) {
  return new other.Function(text);
}

// A realm in which no regular expression has matched yet has nothing to report. The check runs in
// that realm so that the error is compared with its own TypeError.
var readStatic = inOtherRealm(
  "return function (name) { try { return RegExp[name]; } catch (e) { return e.constructor.name; } };"
)();
assert.sameValue(readStatic("$1"), "TypeError", "RegExp.$1 before any match");
assert.sameValue(readStatic("input"), "TypeError", "RegExp.input before any match");
assert.sameValue(readStatic("lastMatch"), "TypeError", "RegExp.lastMatch before any match");
assert.sameValue(readStatic("leftContext"), "TypeError", "RegExp.leftContext before any match");

// Writing the input starts reporting the empty string for the properties a match has not set.
inOtherRealm('RegExp.input = "written";')();
assert.sameValue(inOtherRealm("return RegExp.input;")(), "written");
assert.sameValue(inOtherRealm("return RegExp.$_;")(), "written");
assert.sameValue(inOtherRealm("return RegExp.$1;")(), "");
assert.sameValue(inOtherRealm("return RegExp.lastMatch;")(), "");

// A match then gives the properties the result of that match.
inOtherRealm("return /x(a)/.exec('xab');")();
assert.sameValue(inOtherRealm("return RegExp.$1;")(), "a");
assert.sameValue(inOtherRealm("return RegExp.lastMatch;")(), "xa");
assert.sameValue(inOtherRealm("return RegExp.rightContext;")(), "b");
assert.sameValue(inOtherRealm("return RegExp.$2;")(), "");

// A match that fails leaves the properties as they were.
inOtherRealm("return /q/.exec('zz');")();
assert.sameValue(inOtherRealm("return RegExp.lastMatch;")(), "xa");
assert.sameValue(inOtherRealm("return RegExp.$1;")(), "a");

// Writing the input drops the match that was recorded, because the positions it recorded do not
// refer to the new string.
inOtherRealm('RegExp.input = "written again";')();
assert.sameValue(inOtherRealm("return RegExp.input;")(), "written again");
assert.sameValue(inOtherRealm("return RegExp.lastMatch;")(), "");
assert.sameValue(inOtherRealm("return RegExp.$1;")(), "");
