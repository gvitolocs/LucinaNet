/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

// https://html.spec.whatwg.org/multipage/infrastructure.html#the-htmlallcollection-interface
//
// The members without an identifier are the "all"-indexed and "all"-named
// lookups. `legacycaller` gives the object a [[Call]]: the code generator emits
// the `call` trap for it as a call of the very operation declared below, so
// nothing here duplicates the HTML spec's steps
// (<https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-all>). Their
// first step -- "if argumentsList's size is zero or argumentsList[0] is
// undefined, return null" -- is why this one argument is `optional` where the
// spec's IDL has it required: an absent argument or an `undefined` one converts
// to `None`, which is the null the spec wants, while an argument of any other
// type still stringifies.
// The objects of this interface are DOM proxies, and it is the proxy's JS class
// that carries the [[IsHTMLDDA]] internal slot, as
// `JSCLASS_EMULATES_UNDEFINED`: `typeof` answers "undefined", the value is
// falsy and compares equal to `null`, and none of that is expressible on a
// plain object.

[Exposed=Window, LegacyUnenumerableNamedProperties]
interface HTMLAllCollection {
  readonly attribute unsigned long length;
  getter Element (unsigned long index);
  getter (HTMLCollection or Element)? namedItem(DOMString name);
  (HTMLCollection or Element)? item(optional DOMString nameOrIndex);
  legacycaller (HTMLCollection or Element)? (optional DOMString nameOrIndex);
};
