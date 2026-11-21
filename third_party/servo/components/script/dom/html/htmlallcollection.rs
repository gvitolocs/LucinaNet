/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use html5ever::{local_name, ns};
use js::context::{JSContext, NoGC};
use script_bindings::domstring::TracedDOMString;
use script_bindings::reflector::{Reflector, reflect_dom_object};
use stylo_atoms::Atom;

use crate::dom::bindings::codegen::Bindings::HTMLAllCollectionBinding::HTMLAllCollectionMethods;
use crate::dom::bindings::codegen::Bindings::HTMLCollectionBinding::HTMLCollectionMethods;
use crate::dom::bindings::codegen::UnionTypes::HTMLCollectionOrElement;
use crate::dom::bindings::inheritance::{Castable, ElementTypeId, HTMLElementTypeId, NodeTypeId};
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::bindings::str::DOMString;
use crate::dom::document::Document;
use crate::dom::element::Element;
use crate::dom::html::htmlcollection::{CollectionFilter, HTMLCollection};
use crate::dom::node::Node;
use crate::dom::window::Window;

/// The object behind `document.all`: all the elements of the document, in tree
/// order, reached either by index or by name. Its `[[IsHTMLDDA]]` internal slot,
/// which the JS class of its proxy carries, is what makes `typeof document.all`
/// answer "undefined" while the object is still there to be called.
///
/// <https://html.spec.whatwg.org/multipage/infrastructure.html#the-htmlallcollection-interface>
#[dom_struct]
pub(crate) struct HTMLAllCollection {
    reflector_: Reflector,
    /// The collection this interface is a set of accessors over. A live one, so
    /// that each lookup below answers for the document as it is now: `length`,
    /// the index, and the elements sharing a name are all read through it.
    htmlcollection: Dom<HTMLCollection>,
}

impl HTMLAllCollection {
    pub(crate) fn new(
        cx: &mut JSContext,
        window: &Window,
        document: &Document,
    ) -> DomRoot<HTMLAllCollection> {
        // `getElementsByTagName("*")` of a document is the list of its elements in
        // tree order, which is the collection this interface is defined over.
        let htmlcollection =
            HTMLCollection::by_qualified_name(cx, window, document.upcast(), local_name!("*"));
        reflect_dom_object(
            cx,
            Box::new(HTMLAllCollection {
                reflector_: Reflector::new(),
                htmlcollection: Dom::from_ref(&htmlcollection),
            }),
            window,
        )
    }

    /// The result of a name lookup: nothing, the one element with that name, or a
    /// fresh live collection of the elements that share it.
    ///
    /// <https://html.spec.whatwg.org/multipage/infrastructure.html#concept-get-all-named>
    fn by_name(&self, cx: &mut JSContext, name: DOMString) -> Option<HTMLCollectionOrElement> {
        // Step 1. An empty name is not a name, and matches nothing.
        if name.is_empty() {
            return None;
        }
        // Step 2. The elements matching this name: those with it as their ID, and the
        // "all"-named ones with it as their `name`, in tree order.
        let name = Atom::from(name);
        let (first, shares_the_name) = {
            let mut matches = self
                .htmlcollection
                .elements_iter(cx.no_gc())
                .filter(|elem| element_has_name(elem, &name))
                .map(|elem| elem.as_rooted());
            // Step 3. No element matches: no result.
            let first = matches.next()?;
            (first, matches.next().is_some())
        };
        if !shares_the_name {
            // Step 3. Exactly one element matches: that one.
            return Some(HTMLCollectionOrElement::Element(first));
        }
        // Step 3. More than one matches: a fresh live collection of the matching
        // elements, so that a later lookup of the same name sees the elements that
        // have taken it on in the meantime.
        #[derive(JSTraceable, MallocSizeOf)]
        struct NameFilter {
            #[no_trace]
            name: Atom,
        }
        impl CollectionFilter for NameFilter {
            fn filter(&self, elem: &Element, _root: &Node) -> bool {
                element_has_name(elem, &self.name)
            }
        }
        let global = self.global();
        Some(HTMLCollectionOrElement::HTMLCollection(
            HTMLCollection::create(
                cx,
                global.as_window(),
                &self.htmlcollection.root_node(),
                Box::new(NameFilter { name }),
            ),
        ))
    }
}

/// Whether an element answers to a given name: by its `id`, or, if it is one of
/// the "all"-named elements, by its `name`.
///
/// <https://html.spec.whatwg.org/multipage/infrastructure.html#dom-htmlallcollection-nameditem>
fn element_has_name(elem: &Element, name: &Atom) -> bool {
    if elem.get_id().is_some_and(|id| id == *name) {
        return true;
    }
    if !is_all_named_element(elem) {
        return false;
    }
    elem.get_name().is_some_and(|elem_name| &elem_name == name)
}

/// An element's ID or `name` as a supported property name: an empty one is not a name, and a
/// name two elements share is listed once.
fn push_name(names: &mut Vec<DOMString>, name: Atom) {
    let name = TracedDOMString::from(name).root();
    if !name.is_empty() && !names.contains(&name) {
        names.push(name);
    }
}

/// The elements for which a `name` is a name when it comes to this collection,
/// which is not the set that makes a name for `document` or a window: `area` is
/// missing from it and `iframe`, `map` and `meta` are in it.
///
/// <https://html.spec.whatwg.org/multipage/infrastructure.html#all-named-elements>
fn is_all_named_element(elem: &Element) -> bool {
    if elem.namespace() != &ns!(html) {
        return false;
    }
    let NodeTypeId::Element(ElementTypeId::HTMLElement(type_)) = elem.upcast::<Node>().type_id()
    else {
        return false;
    };
    matches!(
        type_,
        HTMLElementTypeId::HTMLAnchorElement
            | HTMLElementTypeId::HTMLButtonElement
            | HTMLElementTypeId::HTMLEmbedElement
            | HTMLElementTypeId::HTMLFormElement
            | HTMLElementTypeId::HTMLFrameElement
            | HTMLElementTypeId::HTMLFrameSetElement
            | HTMLElementTypeId::HTMLIFrameElement
            | HTMLElementTypeId::HTMLImageElement
            | HTMLElementTypeId::HTMLInputElement
            | HTMLElementTypeId::HTMLMapElement
            | HTMLElementTypeId::HTMLMetaElement
            | HTMLElementTypeId::HTMLObjectElement
            | HTMLElementTypeId::HTMLSelectElement
            | HTMLElementTypeId::HTMLTextAreaElement
    )
}

/// An array index is a canonical decimal string of a number up to the largest
/// index, which is one below 2^32. The same test the proxy's `get` trap applies
/// to a property id, applied here to the string an operation was handed.
fn as_array_index(name_or_index: &DOMString) -> Option<u32> {
    let name_or_index = name_or_index.str();
    let digits = name_or_index.as_bytes();
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // "042" is not the canonical representation of 42, so it names an element
    // instead of indexing the collection.
    if digits[0] == b'0' && digits.len() > 1 {
        return None;
    }
    name_or_index
        .parse::<u64>()
        .ok()
        .filter(|index| *index < u32::MAX as u64)
        .map(|index| index as u32)
}

impl HTMLAllCollectionMethods<crate::DomTypeHolder> for HTMLAllCollection {
    /// <https://html.spec.whatwg.org/multipage/infrastructure.html#dom-htmlallcollection-length>
    fn Length(&self, cx: &JSContext) -> u32 {
        self.htmlcollection.Length(cx)
    }

    /// <https://html.spec.whatwg.org/multipage/infrastructure.html#concept-get-all-indexed>
    fn IndexedGetter(&self, cx: &JSContext, index: u32) -> Option<DomRoot<Element>> {
        self.htmlcollection.IndexedGetter(cx, index)
    }

    /// <https://html.spec.whatwg.org/multipage/infrastructure.html#dom-htmlallcollection-nameditem>
    fn NamedItem(&self, cx: &mut JSContext, name: DOMString) -> Option<HTMLCollectionOrElement> {
        self.by_name(cx, name)
    }

    /// <https://html.spec.whatwg.org/multipage/infrastructure.html#concept-get-all-named>
    fn NamedGetter(&self, cx: &mut JSContext, name: DOMString) -> Option<HTMLCollectionOrElement> {
        self.NamedItem(cx, name)
    }

    /// <https://html.spec.whatwg.org/multipage/infrastructure.html#dom-htmlallcollection-item>
    fn Item(
        &self,
        cx: &mut JSContext,
        name_or_index: Option<DOMString>,
    ) -> Option<HTMLCollectionOrElement> {
        // Step 1. No argument, or an `undefined` one, is no result at all. The
        // [[Call]] of the object reaches this step through the conversion of the
        // optional argument of its `legacycaller`.
        let name_or_index = name_or_index?;
        // Step 2. An array index indexes the collection; anything else is a name.
        match as_array_index(&name_or_index) {
            Some(index) => self
                .IndexedGetter(cx, index)
                .map(HTMLCollectionOrElement::Element),
            None => self.by_name(cx, name_or_index),
        }
    }

    /// <https://html.spec.whatwg.org/multipage/obsolete.html#dom-document-all>
    fn LegacyCaller(
        &self,
        cx: &mut JSContext,
        name_or_index: Option<DOMString>,
    ) -> Option<HTMLCollectionOrElement> {
        // The [[Call]] that the `legacycaller` member gives this object is its
        // `item()`, so the `call` trap generated for it calls this method with the
        // arguments of the call, and this is the same lookup.
        self.Item(cx, name_or_index)
    }

    /// <https://html.spec.whatwg.org/multipage/infrastructure.html#the-htmlallcollection-interface>
    fn SupportedPropertyNames(&self, no_gc: &NoGC) -> Vec<DOMString> {
        // The names of the collection: the ID of every element, and the `name` of every
        // "all"-named element, in tree order and listed once. The list `HTMLCollection` keeps
        // takes the `name` of any HTML element, and would thus name elements that this object's
        // own lookup answers to nothing.
        let mut names: Vec<DOMString> = vec![];
        for elem in self.htmlcollection.elements_iter(no_gc) {
            if let Some(id) = elem.get_id() {
                push_name(&mut names, id);
            }
            if is_all_named_element(&elem)
                && let Some(name) = elem.get_name()
            {
                push_name(&mut names, name);
            }
        }
        names
    }
}
