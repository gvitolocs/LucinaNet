/*---
flags: [raw]
---*/
var other=$262.createRealm(); if (other.evalScript("this") !== other.global) throw new Error("wrong eval realm");
