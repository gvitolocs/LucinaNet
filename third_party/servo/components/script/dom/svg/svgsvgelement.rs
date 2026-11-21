/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashSet;

use atomic_refcell::AtomicRefCell;
use base64::Engine as _;
use cssparser::Parser;
use dom_struct::dom_struct;
use html5ever::{LocalName, Prefix, local_name, ns};
use js::context::{JSContext, NoGC};
use js::rust::HandleObject;
use layout_api::SVGElementData;
use servo_url::ServoUrl;
use style::attr::AttrValue;
use style::properties::PropertyId;
use style::parser::ParserContext;
use style::stylesheets::Origin;
use style::values::specified::LengthPercentage;
use style_traits::ParsingMode;
use uuid::Uuid;
use xml5ever::serialize::TraversalScope;

use crate::dom::bindings::codegen::Bindings::DocumentBinding::DocumentMethods;
use crate::dom::bindings::codegen::Bindings::NodeBinding::NodeMethods;
use crate::dom::bindings::inheritance::Castable;
use crate::dom::bindings::root::{DomRoot, LayoutDom};
use crate::dom::bindings::str::DOMString;
use crate::dom::document::Document;
use crate::dom::element::attributes::storage::AttrRef;
use crate::dom::element::{AttributeMutation, Element};
use crate::dom::iterators::ShadowIncluding;
use crate::dom::node::virtualmethods::VirtualMethods;
use crate::dom::node::{
    ChildrenMutation, CloneChildrenFlag, Node, NodeDamage, NodeTraits, UnbindContext,
};
use crate::dom::svg::svggraphicselement::SVGGraphicsElement;
use crate::dom::svg::svgstyle::{
    BAKED_PROPERTIES, bake_style, fingerprint, is_initial, localize_url_refs,
};
use crate::dom::window::Window;

#[dom_struct]
pub(crate) struct SVGSVGElement {
    svggraphicselement: SVGGraphicsElement,
    #[no_trace]
    uuid: Uuid,
    // The XML source of subtree rooted at this SVG element, serialized into
    // a base64 encoded `data:` url. This is cached to avoid recomputation
    // on each layout and must be invalidated when the subtree changes.
    #[no_trace]
    cached_serialized_data_url: AtomicRefCell<Option<Result<ServoUrl, ()>>>,
    // Fingerprint of the page style baked into the cached serialisation. When it
    // differs from the current one the cached image is stale.
    #[no_trace]
    cached_style_fingerprint: AtomicRefCell<Option<u64>>,
}

impl SVGSVGElement {
    fn new_inherited(
        local_name: LocalName,
        prefix: Option<Prefix>,
        document: &Document,
    ) -> SVGSVGElement {
        SVGSVGElement {
            svggraphicselement: SVGGraphicsElement::new_inherited(local_name, prefix, document),
            uuid: Uuid::new_v4(),
            cached_serialized_data_url: Default::default(),
            cached_style_fingerprint: Default::default(),
        }
    }

    #[cfg_attr(crown, allow(crown::unrooted_must_root))]
    pub(crate) fn new(
        cx: &mut js::context::JSContext,
        local_name: LocalName,
        prefix: Option<Prefix>,
        document: &Document,
        proto: Option<HandleObject>,
    ) -> DomRoot<SVGSVGElement> {
        Node::reflect_node_with_proto(
            cx,
            Box::new(SVGSVGElement::new_inherited(local_name, prefix, document)),
            document,
            proto,
        )
    }

    /// Serialise the subtree (with the resolved page style baked in) unless the cached
    /// serialisation is still current. Returns whether the cached data changed.
    pub(crate) fn serialize_and_cache_subtree(&self, cx: &mut js::context::JSContext) -> bool {
        let baked = self.collect_baked_styles();
        let fingerprint = fingerprint(&baked);
        if self.cached_serialized_data_url.borrow().is_some() {
            if *self.cached_style_fingerprint.borrow() == Some(fingerprint) {
                return false;
            }
            // The page style changed since the last serialisation.
            self.invalidate_cached_serialized_subtree_and_rasterization_result(cx.no_gc());
        }

        let document_fragment = self.owner_document().CreateDocumentFragment(cx);
        let cloned_node = Node::clone(
            cx,
            self.upcast(),
            None,
            CloneChildrenFlag::CloneChildren,
            None,
        );
        if document_fragment
            .upcast::<Node>()
            .AppendChild(cx, &cloned_node)
            .is_err()
        {
            error!("Unable to clone SVG tree");
            *self.cached_serialized_data_url.borrow_mut() = Some(Err(()));
            return true;
        }

        // Bake before `use` expansion: the clone has the same shape as the original here.
        Self::apply_baked_styles(cx, &cloned_node, &baked);
        *self.cached_style_fingerprint.borrow_mut() = Some(fingerprint);

        self.process_use_elements(cx, &cloned_node);

        let Ok(xml_source) = cloned_node.xml_serialize(TraversalScope::IncludeNode) else {
            *self.cached_serialized_data_url.borrow_mut() = Some(Err(()));
            return true;
        };

        let xml_source: String = xml_source.into();
        let base64_encoded_source = base64::engine::general_purpose::STANDARD.encode(xml_source);
        let data_url = format!("data:image/svg+xml;base64,{base64_encoded_source}");
        match ServoUrl::parse(&data_url) {
            Ok(url) => *self.cached_serialized_data_url.borrow_mut() = Some(Ok(url)),
            Err(error) => error!("Unable to parse serialized SVG data url: {error}"),
        };
        true
    }

    /// The resolved page style to bake for every element of the subtree, in preorder.
    fn collect_baked_styles(&self) -> Vec<Vec<(&'static str, String)>> {
        let window = self.owner_window();
        let root = self.upcast::<Node>();
        let ids: HashSet<String> = root
            .traverse_preorder(ShadowIncluding::No)
            .filter_map(|node| node.downcast::<Element>().and_then(|e| e.get_id()))
            .map(|id| id.to_string())
            .collect();
        let mut out = Vec::new();
        Self::collect_element_styles(&window, root, None, &ids, &mut out);
        out
    }

    fn collect_element_styles(
        window: &Window,
        node: &Node,
        parent_values: Option<&[String]>,
        ids: &HashSet<String>,
        out: &mut Vec<Vec<(&'static str, String)>>,
    ) {
        let Some(element) = node.downcast::<Element>() else {
            return;
        };
        let is_root = parent_values.is_none();
        let connected = node.is_connected();
        let values: Vec<String> = BAKED_PROPERTIES
            .iter()
            .map(|property| {
                if !connected {
                    return String::new();
                }
                match PropertyId::parse_enabled_for_all_content(property.name) {
                    Ok(id) => window
                        .resolved_style_query_no_flush(node.to_trusted_node_address(), id)
                        .to_string(),
                    Err(_) => String::new(),
                }
            })
            .collect();

        let mut baked = Vec::new();
        for (index, property) in BAKED_PROPERTIES.iter().enumerate() {
            let value = values[index].trim();
            if value.is_empty() || (is_root && !property.on_root) {
                continue;
            }
            let skip = if property.inherited {
                match parent_values {
                    Some(parent) => parent[index].trim() == value,
                    None => is_initial(property.name, value),
                }
            } else {
                is_initial(property.name, value)
            };
            if skip {
                continue;
            }
            // The CSS `transform` would apply twice with a `transform` attribute.
            if property.name == "transform" && element.has_attribute(&local_name!("transform")) {
                continue;
            }
            let value = if value.contains("url(") {
                match localize_url_refs(value, ids) {
                    Some(localized) => localized,
                    None => continue,
                }
            } else {
                value.to_owned()
            };
            baked.push((property.name, value));
        }
        out.push(baked);

        for child in node.children() {
            Self::collect_element_styles(window, &child, Some(&values), ids, out);
        }
    }

    /// Write the baked declarations into the `style` attribute of the cloned elements.
    fn apply_baked_styles(
        cx: &mut JSContext,
        cloned_root: &Node,
        baked: &[Vec<(&'static str, String)>],
    ) {
        let elements: Vec<DomRoot<Element>> = cloned_root
            .traverse_preorder(ShadowIncluding::No)
            .filter_map(DomRoot::downcast::<Element>)
            .collect();
        if elements.len() != baked.len() {
            error!("SVG clone does not match the original tree, not baking styles");
            return;
        }
        for (element, declarations) in elements.iter().zip(baked) {
            if declarations.is_empty() {
                continue;
            }
            let existing = element.get_string_attribute(&local_name!("style"));
            let merged = bake_style(&existing.str(), declarations);
            element.set_string_attribute(cx, &local_name!("style"), DOMString::from(merged));
        }
    }

    fn process_use_elements(&self, cx: &mut JSContext, root_node: &Node) {
        for node in root_node.traverse_preorder(ShadowIncluding::No) {
            if let Some(element) = node.downcast::<Element>()
                && element.local_name() == &local_name!("use")
            {
                self.process_single_use_element(cx, element, root_node)
            }
        }
    }

    fn process_single_use_element(
        &self,
        cx: &mut JSContext,
        use_element: &Element,
        root_node: &Node,
    ) {
        let href = use_element.get_string_attribute(&local_name!("href"));
        let Some(id_string) = href.str().strip_prefix("#").map(DOMString::from) else {
            return;
        };

        let document = self.upcast::<Node>().owner_doc();
        let Some(referenced_element) = document.GetElementById(cx, id_string) else {
            return;
        };
        let referenced_node = referenced_element.upcast::<Node>();

        // Don't use this node if it doesn't have an `<svg>` ancestor.
        if !referenced_node
            .inclusive_ancestors_unrooted(cx.no_gc(), ShadowIncluding::No)
            .any(|ancestor| ancestor.is::<SVGSVGElement>())
        {
            return;
        };

        // Don't use this node if it already exists within the same `<svg>` element.
        if referenced_node
            .inclusive_ancestors_unrooted(cx.no_gc(), ShadowIncluding::No)
            .any(|ancestor| *ancestor == self.upcast())
        {
            return;
        };

        let cloned_node = Node::clone(
            cx,
            referenced_node,
            None,
            CloneChildrenFlag::CloneChildren,
            None,
        );
        let _ = root_node.AppendChild(cx, &cloned_node);
    }

    fn invalidate_cached_serialized_subtree_and_rasterization_result(&self, no_gc: &NoGC) {
        let owner_window = self.owner_window();
        owner_window
            .image_cache()
            .evict_rasterized_image(&self.uuid);
        if let Some(Ok(url)) = &*self.cached_serialized_data_url.borrow() {
            owner_window.layout_mut().remove_cached_image(url);
            owner_window.image_cache().evict_completed_image(
                url,
                owner_window.origin().immutable(),
                &None,
            );
        }

        *self.cached_serialized_data_url.borrow_mut() = None;
        *self.cached_style_fingerprint.borrow_mut() = None;
        self.upcast::<Node>().dirty(no_gc, NodeDamage::Other);
    }
}

impl<'dom> LayoutDom<'dom, SVGSVGElement> {
    pub(crate) fn data(self) -> SVGElementData<'dom> {
        let svg_id = self.unsafe_get().uuid;
        let element = self.upcast::<Element>();
        let width = element.get_attr_for_layout(&ns!(), &local_name!("width"));
        let height = element.get_attr_for_layout(&ns!(), &local_name!("height"));
        let view_box = element.get_attr_for_layout(&ns!(), &local_name!("viewBox"));
        SVGElementData {
            source: self
                .unsafe_get()
                .cached_serialized_data_url
                .borrow()
                .clone(),
            width,
            height,
            view_box,
            svg_id,
        }
    }
}

impl VirtualMethods for SVGSVGElement {
    fn super_type(&self) -> Option<&dyn VirtualMethods> {
        Some(self.upcast::<SVGGraphicsElement>() as &dyn VirtualMethods)
    }

    fn attribute_mutated(
        &self,
        cx: &mut js::context::JSContext,
        attr: AttrRef<'_>,
        mutation: AttributeMutation,
    ) {
        self.super_type()
            .unwrap()
            .attribute_mutated(cx, attr, mutation);

        self.invalidate_cached_serialized_subtree_and_rasterization_result(cx.no_gc());
    }

    fn attribute_affects_presentational_hints(&self, attr: AttrRef<'_>) -> bool {
        match attr.local_name() {
            &local_name!("width") | &local_name!("height") => true,
            _ => self
                .super_type()
                .unwrap()
                .attribute_affects_presentational_hints(attr),
        }
    }

    fn parse_plain_attribute(&self, name: &LocalName, value: DOMString) -> AttrValue {
        match *name {
            local_name!("width") | local_name!("height") => {
                let value = &value.str();
                let parser = &mut Parser::new(value);
                let doc = self.owner_document();
                let url = doc.url().into_url().into();
                let context = ParserContext::new(
                    Origin::Author,
                    &url,
                    None,
                    ParsingMode::ALLOW_UNITLESS_LENGTH,
                    doc.quirks_mode(),
                    /* namespaces = */ Default::default(),
                    None,
                    None,
                    /* attr_taint = */ Default::default(),
                );
                let val = LengthPercentage::parse_quirky(
                    &context,
                    parser,
                    style::values::specified::AllowQuirks::Always,
                );
                AttrValue::LengthPercentage(value.to_string(), val.ok())
            },
            _ => self
                .super_type()
                .unwrap()
                .parse_plain_attribute(name, value),
        }
    }

    fn children_changed(&self, cx: &mut JSContext, mutation: &ChildrenMutation) {
        if let Some(super_type) = self.super_type() {
            super_type.children_changed(cx, mutation);
        }

        self.invalidate_cached_serialized_subtree_and_rasterization_result(cx.no_gc());
    }

    fn unbind_from_tree(&self, cx: &mut js::context::JSContext, context: &UnbindContext<'_>) {
        if let Some(s) = self.super_type() {
            s.unbind_from_tree(cx, context);
        }

        self.invalidate_cached_serialized_subtree_and_rasterization_result(cx.no_gc());
    }
}
