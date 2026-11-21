/*---
description: >
  The RegExp legacy static properties report the results of the last match of the realm, and a
  regexp created by a subclass of RegExp does not write them.
---*/

"xxabd".match(/(a)(b)(d)/);

assert.sameValue(RegExp.input, "xxabd", "RegExp.input");
assert.sameValue(RegExp.$_, "xxabd", "RegExp.$_");
assert.sameValue(RegExp.lastMatch, "abd", "RegExp.lastMatch");
assert.sameValue(RegExp["$&"], "abd", "RegExp.$&");
assert.sameValue(RegExp.leftContext, "xx", "RegExp.leftContext");
assert.sameValue(RegExp["$`"], "xx", "RegExp.$`");
assert.sameValue(RegExp.rightContext, "", "RegExp.rightContext");
assert.sameValue(RegExp["$'"], "", "RegExp.$'");
assert.sameValue(RegExp.lastParen, "d", "RegExp.lastParen");
assert.sameValue(RegExp["$+"], "d", "RegExp.$+");
assert.sameValue(RegExp.$1, "a", "RegExp.$1");
assert.sameValue(RegExp.$2, "b", "RegExp.$2");
assert.sameValue(RegExp.$3, "d", "RegExp.$3");

// Only the first nine groups are recorded, but `lastParen` is the last group that matched even when
// there are more than nine groups.
"1234567890".match(/(1)(2)(3)(4)(5)(6)(7)(8)(9)(0)/);
assert.sameValue(RegExp.$9, "9", "RegExp.$9");
assert.sameValue(RegExp.lastMatch, "1234567890", "RegExp.lastMatch");
assert.sameValue(RegExp.lastParen, "0", "RegExp.lastParen");

// A group that did not take part in the match is the empty string.
"xx".match(/(x)(y)?(x)/);
assert.sameValue(RegExp.$2, "", "an unmatched group");
assert.sameValue(RegExp.$3, "x", "RegExp.$3");
assert.sameValue(RegExp.lastParen, "x", "RegExp.lastParen");

// A regexp created by a subclass of RegExp does not write the realm's legacy static properties.
class SubRegExp extends RegExp {}
"zz".match(new SubRegExp("(z)"));

assert.sameValue(RegExp.$1, "x", "the subclass match did not record its group");
assert.sameValue(RegExp.input, "xx", "the subclass match did not record its input");

// The properties belong to the realm, so a match in another realm does not change them.
var other = $262.createRealm().global;
new other.Function("return /q(z)/.exec('qqz');")();
assert.sameValue(new other.Function("return RegExp.$1;")(), "z", "the other realm's own value");
assert.sameValue(RegExp.$1, "x", "this realm is unchanged");
