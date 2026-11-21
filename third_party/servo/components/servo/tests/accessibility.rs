/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! WebView API unit tests.
mod common;

use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::rc::Rc;

use accesskit::Role::{self, GenericContainer};
use accesskit::{Action, ActionRequest, Affine, NodeId, Rect, TreeId, TreeUpdate};
use accesskit_consumer::TreeChangeHandler;
use euclid::Scale;
use servo::{
    DiagnosticsLoggingOption, InputEvent, Key, KeyState, KeyboardEvent, LoadStatus, NamedKey, Opts,
    Preferences, Scroll, WebView, WebViewBuilder, WebViewPoint, WebViewVector,
};
use url::Url;
use webrender_api::units::{DevicePoint, DeviceVector2D};

use crate::common::{ServoTest, WebViewDelegateImpl, evaluate_javascript};

struct NoOpChangeHandler;

impl TreeChangeHandler for NoOpChangeHandler {
    fn node_added(&mut self, _: &accesskit_consumer::Node) {}
    fn node_updated(&mut self, _: &accesskit_consumer::Node, _: &accesskit_consumer::Node) {}
    fn focus_moved(
        &mut self,
        _: Option<&accesskit_consumer::Node>,
        _: Option<&accesskit_consumer::Node>,
    ) {
    }
    fn node_removed(&mut self, _: &accesskit_consumer::Node) {}
}

#[test]
fn test_basic_accessibility_update() {
    let servo_test = build_test();

    let delegate = Rc::new(WebViewDelegateImpl::default());
    let webview = WebViewBuilder::new(servo_test.servo(), servo_test.rendering_context.clone())
        .delegate(delegate.clone())
        .url(Url::parse("data:text/html,<!DOCTYPE html>").unwrap())
        .build();

    webview.set_accessibility_active(true);

    servo_test.spin(|| webview.load_status() != LoadStatus::Complete);

    let updates = wait_for_min_updates(&servo_test, delegate.clone(), 3);
    let tree = build_tree(updates);
    let _ = assert_tree_structure_and_get_root_web_area(&tree);
}

#[test]
fn test_activate_accessibility_after_layout() {
    let servo_test = build_test();

    let delegate = Rc::new(WebViewDelegateImpl::default());
    let webview = WebViewBuilder::new(servo_test.servo(), servo_test.rendering_context.clone())
        .delegate(delegate.clone())
        .url(Url::parse("data:text/html,<!DOCTYPE html>").unwrap())
        .build();

    servo_test.spin(|| webview.load_status() != LoadStatus::Complete);

    webview.set_accessibility_active(true);

    let updates = wait_for_min_updates(&servo_test, delegate.clone(), 3);
    let tree = build_tree(updates);
    let _ = assert_tree_structure_and_get_root_web_area(&tree);
}

#[test]
fn test_navigate_creates_new_accessibility_update() {
    let servo_test = build_test();

    let page_1_url = Url::parse("data:text/html,<!DOCTYPE html> page 1").unwrap();
    let page_2_url = Url::parse("data:text/html,<!DOCTYPE html> page 2").unwrap();

    let delegate = Rc::new(WebViewDelegateImpl::default());
    let webview = WebViewBuilder::new(servo_test.servo(), servo_test.rendering_context.clone())
        .delegate(delegate.clone())
        .url(page_1_url)
        .build();
    webview.set_accessibility_active(true);

    servo_test.spin(|| webview.load_status() != LoadStatus::Complete);

    let updates = wait_for_min_updates(&servo_test, delegate.clone(), 3);
    let mut tree = build_tree(updates);
    let root_web_area = assert_tree_structure_and_get_root_web_area(&tree);

    let result = find_first_matching_node(root_web_area, |node| {
        node.role() == accesskit::Role::TextRun
    });
    let text_node = result.expect("Should be exactly one TextRun in the tree");

    assert_eq!(text_node.value().as_deref(), Some("page 1"));

    webview.load(page_2_url.clone());
    servo_test.spin(|| webview.url() != Some(page_2_url.clone()));

    update_tree_after_min_updates(&servo_test, &delegate, &mut tree, 2);

    let root_node = tree.state().root();
    let result =
        find_first_matching_node(root_node, |node| node.role() == accesskit::Role::TextRun);
    let text_node = result.expect("Should be exactly one TextRun in the tree");

    assert_eq!(text_node.value().as_deref(), Some("page 2"));
}

// FIXME(accessibility): when clicking back and forward, we currently rely on
// layout and the accessibility tree being rebuilt from scratch, so that the full
// a11y tree can be resent.
// But if bfcache navigations stop redoing layout, or we implement incremental
// a11y tree building, this test will break.
#[test]
fn test_accessibility_after_navigate_and_back() {
    let url_1 = "data:text/html,<!DOCTYPE html> page 1";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url_1);
    let root_web_area = assert_tree_structure_and_get_root_web_area(&tree);

    let result = find_all_matching_nodes(root_web_area, |node| {
        node.role() == accesskit::Role::TextRun
    });
    assert_eq!(result.len(), 1);
    let text_node = result[0];

    assert_eq!(text_node.value().as_deref(), Some("page 1"));

    let url_2 = Url::parse("data:text/html,<!DOCTYPE html> page 2").unwrap();
    webview.load(url_2.clone());
    servo_test.spin(|| webview.url() != Some(url_2.clone()));

    update_tree_after_min_updates(&servo_test, &delegate, &mut tree, 3);

    let root_node = tree.state().root();
    let result = find_all_matching_nodes(root_node, |node| node.role() == accesskit::Role::TextRun);
    assert_eq!(result.len(), 1);
    let text_node = result[0];

    assert_eq!(text_node.value().as_deref(), Some("page 2"));

    webview.go_back(1);
    servo_test.spin(|| webview.url() != Some(Url::parse(url_1).unwrap()));

    update_tree_after_min_updates(&servo_test, &delegate, &mut tree, 3);

    let root_node = tree.state().root();
    let result = find_all_matching_nodes(root_node, |node| node.role() == accesskit::Role::TextRun);
    assert_eq!(result.len(), 1);
    let text_node = result[0];

    assert_eq!(text_node.value().as_deref(), Some("page 1"));
}

// TODO(accessibility): write test for resend a11y tree when clicking back or forward

#[test]
fn test_accessibility_basic_mapping() {
    let mut element_role_pairs = VecDeque::from([
        ("article", Role::Article),
        ("aside", Role::Complementary),
        ("footer", Role::ContentInfo),
        ("h1", Role::Heading),
        ("h2", Role::Heading),
        ("h3", Role::Heading),
        ("h4", Role::Heading),
        ("h5", Role::Heading),
        ("h6", Role::Heading),
        ("header", Role::Banner),
        ("hr", Role::Splitter),
        ("main", Role::Main),
        ("nav", Role::Navigation),
        ("p", Role::Paragraph),
    ]);

    let mut url: String = "data:text/html,<!DOCTYPE html>".to_owned();
    for (element, _) in element_role_pairs.iter() {
        write!(url, "<{element}></{element}>").unwrap();
    }

    let (_servo_test, _delegate, _webview, tree) = build_webview_and_tree(url.as_str());
    let root = assert_tree_structure_and_get_root_web_area(&tree);

    assert_eq!(root.children().len(), element_role_pairs.len());
    for child in root.children() {
        let Some((tag, role)) = element_role_pairs.pop_front() else {
            panic!("Number of children of root node should match number of tag/role pairs");
        };
        assert_eq!(child.data().html_tag(), Some(tag));
        assert_eq!(child.role(), role);
    }
    assert!(
        element_role_pairs.is_empty(),
        "Number of children of root node should match number of tag/role pairs"
    );
}

#[test]
fn test_accessibility_basic_role_from_attribute() {
    let url = "data:text/html,<!DOCTYPE html><div role='blockquote'></div>";
    let (_servo_test, _delegate, _webview, tree) = build_webview_and_tree(url);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let first_child = root
        .children()
        .next()
        .expect("Root web area should have at least one child.");
    assert_eq!(first_child.role(), Role::Blockquote);
}

#[test]
fn test_accessibility_basic_name_from_contents() {
    let url = "data:text/html,<!DOCTYPE html><h1>Servo</h1>";
    let (_servo_test, _delegate, _webview, tree) = build_webview_and_tree(url);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let first_child = root
        .children()
        .next()
        .expect("Root web area should have at least one child.");
    assert_eq!(first_child.role(), Role::Heading);
    assert_eq!(first_child.label(), Some("Servo".to_owned()));
}

#[test]
fn test_accessibility_name_from_contents_subtree() {
    let url = "data:text/html,<!DOCTYPE html>\
               <h1>Servo aims to empower <code>developers</code> with a <em>lightweight</em>, \
               <strong>high-performance</strong> alternative for <span>embedding \
               <span>web technologies</span> in <span>applications</span></span>.</h1>";
    let (_servo_test, _delegate, _webview, tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let heading = root
        .children()
        .next()
        .expect("Root web area should have at least one child.");
    assert_eq!(heading.role(), Role::Heading);
    let heading_children: Vec<accesskit_consumer::Node> = heading.children().collect();
    assert_eq!(heading_children.len(), 9);
    assert_eq!(
        heading.label(),
        Some(
            "Servo aims to empower developers with a lightweight, high-performance alternative for \
             embedding web technologies in applications."
                .to_owned()
        ),
        "Heading label should be composed of the text contents of all of its descendant text nodes"
    );
}

#[test]
fn test_accessibility_basic_mutation() {
    let url = "data:text/html,<!DOCTYPE html>\
               <h1 id='h1'>This is an h1</h1>\
               <h2 id='h2'>This is an h2</h2>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<_> = root.children().collect();
    assert_eq!(children.len(), 2);
    let h1 = children[0];
    assert_eq!(h1.label(), Some("This is an h1".to_owned()));
    let h2 = children[1];
    assert_eq!(h2.label(), Some("This is an h2".to_owned()));

    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('h2').remove();",
    );

    let update = expect_single_update(&servo_test, delegate);
    // The `<html>` element is also re-sent, because removing the `<h2>` made the document shorter
    // and therefore changed its bounds.
    assert_eq!(update.nodes.len(), 2);
    let root_web_area = find_node_with_role(&update, Role::RootWebArea);
    assert_eq!(root_web_area.children().len(), 1);
    tree.update_and_process_changes(update, &mut NoOpChangeHandler);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<_> = root.children().collect();
    assert_eq!(children.len(), 1);
    let h1 = children[0];
    assert_eq!(h1.label(), Some("This is an h1".to_owned()));
}

#[test]
fn test_accessibility_with_mutation_move_nodes() {
    let url = "data:text/html,<!DOCTYPE html>\
               <div id='div1'></div>\
               <h1 id='h1'>This is an h1</h1>\
               <h2 id='h2'>This is an h2</h2>\
               <div id='div2'></div>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 4);
    let div1 = children[0];
    let h1 = children[1];
    let h2 = children[2];
    let div2 = children[3];
    assert_eq!(div1.role(), Role::GenericContainer);
    assert_eq!(h1.role(), Role::Heading);
    assert_eq!(h1.label(), Some("This is an h1".to_owned()));
    assert_eq!(h2.role(), Role::Heading);
    assert_eq!(h2.label(), Some("This is an h2".to_owned()));
    assert_eq!(div2.role(), Role::GenericContainer);

    // use both moveBefore and appendChild to exercise the different ways nodes move.
    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "div1.moveBefore(h1,null); div2.appendChild(h2);",
    );

    let update = expect_single_update(&servo_test, delegate);
    assert_eq!(update.nodes.len(), 3);
    assert_eq!(update.nodes[0].1.role(), Role::GenericContainer);
    assert_eq!(update.nodes[0].1.children().len(), 1);
    assert_eq!(update.nodes[1].1.role(), Role::GenericContainer);
    assert_eq!(update.nodes[1].1.children().len(), 1);
    assert_eq!(update.nodes[2].1.role(), Role::RootWebArea);
    assert_eq!(update.nodes[2].1.children().len(), 2);
    tree.update_and_process_changes(update, &mut NoOpChangeHandler);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    assert_eq!(root.children().count(), 2);
    let div1 = root.children().nth(0).unwrap();
    let div2 = root.children().nth(1).unwrap();
    assert_eq!(div1.children().count(), 1);
    assert_eq!(div2.children().count(), 1);
    let h1 = div1.children().nth(0).unwrap();
    let h2 = div2.children().nth(0).unwrap();
    assert_eq!(h1.role(), Role::Heading);
    assert_eq!(h1.label().as_deref(), Some("This is an h1"));
    assert_eq!(h2.role(), Role::Heading);
    assert_eq!(h2.label().as_deref(), Some("This is an h2"));
}

#[test]
fn test_accessibility_text_change() {
    let url = "data:text/html,<!DOCTYPE html>\
               <h1 id='h1'>This is an h1</h1>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 1);
    let h1 = children[0];
    assert_eq!(h1.role(), Role::Heading);
    assert_eq!(h1.label(), Some("This is an h1".to_owned()));

    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "h1.firstChild.appendData(', now with more text');",
    );
    let update = expect_single_update(&servo_test, delegate);
    // Appending text always re-sends the two nodes whose contents changed
    let _ = find_node_with_role(&update, Role::TextRun);
    let heading = find_node_with_role(&update, Role::Heading);
    assert_eq!(heading.children().len(), 1);
    tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 1);
    let h1 = children[0];
    assert_eq!(h1.role(), Role::Heading);
    assert_eq!(
        h1.label(),
        Some("This is an h1, now with more text".to_owned())
    );
}

#[test]
fn test_accessibility_role_mutations() {
    let url = "data:text/html,<!DOCTYPE html>\
        <div id='div1' role='emphasis'>Servo</div>\
        <aside id='aside'>Aside</aside>\
        <main id='main' role='status'>Main</main>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 3);
    let div = children[0];
    assert_eq!(
        div.role(),
        Role::Emphasis,
        "<div> should respect author's override"
    );
    let aside = children[1];
    assert_eq!(
        aside.role(),
        Role::Complementary,
        "<aside> should default to implicit ARIA role"
    );
    let main = children[2];
    assert_eq!(
        main.role(),
        Role::Status,
        "<main> should respect author's override"
    );

    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "\
            div1.role = 'blockquote';\
            aside.role = 'presentation';\
            main.role = null;\
        ",
    );

    let update = expect_single_update(&servo_test, delegate);
    assert_eq!(update.nodes.len(), 3);
    assert_eq!(
        update.nodes[0].1.role(),
        Role::Main,
        "<main> should fallback to implicit role when explicit role is removed"
    );
    assert_eq!(
        update.nodes[1].1.role(),
        Role::Blockquote,
        "<div> should have the new explicit role"
    );
    assert_eq!(
        update.nodes[2].1.role(),
        Role::GenericContainer,
        "<aside> should use the explicit role instead of the implicit role"
    );
    tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 3);
    assert_eq!(children[0].role(), Role::Blockquote);
    assert_eq!(children[1].role(), Role::GenericContainer);
    assert_eq!(children[2].role(), Role::Main);
}

#[test]
fn test_accessibility_partial_subtree_move_and_delete() {
    let url = "data:text/html,<!DOCTYPE html>\
               <header id='header'>\
                 <div id='div'>\
                   <h1 id='h1'>This is an h1</h1>\
                   <p id='p'>This is a paragraph</p>\
                 </div>\
               </header>\
               <article id='article'><h2 id='h2'>This is an h2</h2></article>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 2);
    let header = children[0];
    assert_eq!(header.role(), Role::Banner);
    let h1 = find_all_matching_nodes(header, |node| node.role() == Role::Heading)[0];
    assert_eq!(h1.label(), Some("This is an h1".to_owned()));
    let _p = find_all_matching_nodes(header, |node| node.role() == Role::Paragraph)
        .pop()
        .expect("Should be exactly one Paragraph node");

    let article = children[1];
    assert_eq!(article.role(), Role::Article);
    let h2 = find_all_matching_nodes(article, |node| node.role() == Role::Heading)[0];
    assert_eq!(h2.label(), Some("This is an h2".to_owned()));

    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "article.moveBefore(div, h2);\
         article.moveBefore(h1, div);\
         p.remove();\
         header.remove();",
    );

    let root = apply_single_update(&servo_test, delegate, &mut tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 1);
    let article = children[0];
    assert_eq!(article.role(), Role::Article);
    let children: Vec<accesskit_consumer::Node> = article.children().collect();
    assert_eq!(children.len(), 3);
    let h1 = children[0];
    assert_eq!(h1.role(), Role::Heading);
    assert_eq!(h1.label(), Some("This is an h1".to_owned()));
    let div = children[1];
    assert_eq!(div.role(), Role::GenericContainer);
    let h2 = children[2];
    assert_eq!(h2.role(), Role::Heading);
    assert_eq!(h2.label(), Some("This is an h2".to_owned()));
}

#[test]
fn test_accessibility_children_of_heading_change() {
    let url = "data:text/html,<!DOCTYPE html>\
               <h2>Activating accessibility for a <code>WebView</code></h2>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 1);
    let heading = children[0];
    assert_eq!(heading.role(), Role::Heading);
    assert_eq!(
        heading.label(),
        Some("Activating accessibility for a WebView".to_owned())
    );
    let heading_children: Vec<_> = heading.children().collect();
    assert_eq!(heading_children.len(), 2);
    assert_eq!(heading_children[0].role(), Role::TextRun);
    assert_eq!(heading_children[1].role(), Role::Code);

    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.querySelector('code').remove();",
    );

    let root = apply_single_update(&servo_test, delegate, &mut tree);
    let heading = find_first_matching_node(root, |node| node.role() == Role::Heading)
        .expect("Heading should still be in the tree");
    assert_eq!(
        heading.label(),
        Some("Activating accessibility for a".to_owned())
    );
}

#[test]
fn test_accessibility_descendants_of_heading_change() {
    let url = "data:text/html,<!DOCTYPE html>\
               <h1>We really <em>really <strong>really</strong></em> like owls</h1>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 1);
    let heading = children[0];
    assert_eq!(heading.role(), Role::Heading);
    assert_eq!(
        heading.label(),
        Some("We really really really like owls".to_owned())
    );
    let heading_children: Vec<_> = heading.children().collect();
    assert_eq!(heading_children.len(), 3);
    assert_eq!(heading_children[0].role(), Role::TextRun);
    assert_eq!(heading_children[1].role(), Role::Emphasis);
    assert_eq!(heading_children[2].role(), Role::TextRun);

    let em = heading_children[1];
    assert_eq!(em.children().collect::<Vec<_>>().len(), 2);

    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.querySelector('strong').remove();",
    );

    let root = apply_single_update(&servo_test, delegate, &mut tree);
    let heading = find_first_matching_node(root, |node| node.role() == Role::Heading)
        .expect("Heading should still be in the tree");
    assert_eq!(
        heading.label(),
        Some("We really really  like owls".to_owned())
    );
}

#[test]
fn test_accessibility_bounds() {
    let url = "data:text/html,<!DOCTYPE html>\
               <div id='box' style='position:absolute;left:10px;top:20px;\
               width:100px;height:50px'></div>";
    let (_servo_test, _delegate, _webview, tree) = build_webview_and_tree(url);

    let scroll_view = find_first_matching_node(tree.state().root(), |node| {
        node.role() == Role::ScrollView
    })
    .expect(
        "Tree should include a scroll view corresponding to the WebView. (covers whole viewport)",
    );
    assert_rect_eq(
        scroll_view
            .raw_bounds()
            .expect("WebView node should have bounds"),
        Rect::new(0.0, 0.0, TEST_VIEWPORT_SIZE, TEST_VIEWPORT_SIZE),
    );

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let div = root
        .children()
        .next()
        .expect("Root web area should have at least one child.");
    assert_eq!(div.data().html_tag(), Some("div"));

    // No transforms between div and container; bounds are viewport-relative CSS pixels
    let expected = Rect::new(10.0, 20.0, 110.0, 70.0);
    assert_rect_eq(div.raw_bounds().expect("div should have bounds"), expected);
    assert_rect_eq(
        div.bounding_box().expect("div should have a bounding box"),
        expected,
    );
}

#[test]
fn test_accessibility_webview_bounds_updated_after_hidpi_change() {
    let (servo_test, delegate, webview, mut tree) =
        build_webview_and_tree("data:text/html,<!DOCTYPE html>");

    webview.set_hidpi_scale_factor(Scale::new(2.0));

    update_tree_after_min_updates(&servo_test, &delegate, &mut tree, 1);

    let scroll_view =
        find_first_matching_node(tree.state().root(), |node| node.role() == Role::ScrollView)
            .expect("Tree should include a scroll view corresponding to the WebView");
    assert_rect_eq(
        scroll_view
            .raw_bounds()
            .expect("WebView node should have bounds"),
        Rect::new(0.0, 0.0, TEST_VIEWPORT_SIZE / 2.0, TEST_VIEWPORT_SIZE / 2.0),
    );
}

#[test]
fn test_accessibility_text_run_bounds() {
    let url = "data:text/html,<!DOCTYPE html>\
               <h1 style='position:absolute;left:0;top:0;margin:0;\
               width:300px;height:40px'>Servo</h1>";
    let (_servo_test, _delegate, _webview, tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let heading = root
        .children()
        .next()
        .expect("Root web area should have at least one child.");
    assert_eq!(heading.role(), Role::Heading);
    assert_rect_eq(
        heading.raw_bounds().expect("heading should have bounds"),
        Rect::new(0.0, 0.0, 300.0, 40.0),
    );

    // TODO(accessibility): Text nodes have no layout box of their own, so no bounds are computed
    // for them yet. They should eventually get the union of the rectangles of their own
    // `Fragment::Text` fragments. This assertion demonstrates the current behaviour; flip it once
    // text run bounds are implemented. See #47164.
    let text_run = find_first_matching_node(heading, |node| node.role() == Role::TextRun)
        .expect("Heading should contain a TextRun");
    assert!(
        text_run.raw_bounds().is_none(),
        "TextRun bounds are not implemented yet, but got {:?}",
        text_run.raw_bounds()
    );
}

#[test]
fn test_accessibility_bounds_omitted_for_display_contents() {
    let url = "data:text/html,<!DOCTYPE html>\
               <div style='position:absolute;left:0;top:0;width:200px;height:30px'>\
               <span style='display:contents'>hello</span></div>";
    let (_servo_test, _delegate, _webview, tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let div = root
        .children()
        .next()
        .expect("Root web area should have at least one child.");
    assert_rect_eq(
        div.raw_bounds().expect("div should have bounds"),
        Rect::new(0.0, 0.0, 200.0, 30.0),
    );

    // TODO(accessibility): A `display: contents` element generates no box, so no bounds are
    // computed for it yet. Other engines (Blink, WebKit, Gecko) compute its bounds as the union of
    // the bounding boxes of its rendered descendants. This test demonstrates the current
    // behaviour; flip these assertions once that is implemented. See #47163.
    let span = find_first_matching_node(root, |node| node.data().html_tag() == Some("span"))
        .expect("Document should contain the `display: contents` span");
    assert!(
        span.raw_bounds().is_none(),
        "`display: contents` bounds are not implemented yet, but got {:?}",
        span.raw_bounds()
    );
    let text_run = find_first_matching_node(span, |node| node.role() == Role::TextRun)
        .expect("The `display: contents` span should contain a TextRun");
    assert!(
        text_run.raw_bounds().is_none(),
        "TextRun bounds are not implemented yet, but got {:?}",
        text_run.raw_bounds()
    );
}

#[test]
fn test_accessibility_bounds_omitted_for_display_none() {
    // display: none: no geometry, text must not inherit ancestor bounds
    let url = "data:text/html,<!DOCTYPE html>\
               <div style='position:absolute;left:0;top:0;width:200px;height:30px'>\
               <span id='hidden' style='display:none'>hidden</span></div>";
    let (_servo_test, _delegate, _webview, tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    for node in find_all_matching_nodes(root, |node| {
        node.data().html_tag() == Some("span") || node.role() == Role::TextRun
    }) {
        assert!(
            node.raw_bounds().is_none(),
            "`display: none` content should have no bounds, but {:?} had {:?}",
            node.role(),
            node.raw_bounds()
        );
    }
}

#[test]
fn test_accessibility_bounds_updated_after_relayout() {
    let url = "data:text/html,<!DOCTYPE html>\
               <div id='box' style='position:absolute;left:10px;top:20px;\
               width:100px;height:50px'></div>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let div = root.children().next().expect("Should have a div child");
    let div_id = div.locate().0; // Maps to layout's NodeId
    assert_rect_eq(
        div.raw_bounds().expect("div should have bounds"),
        Rect::new(10.0, 20.0, 110.0, 70.0),
    );

    // Relayout with geometry changes auto-refreshes accessibility tree
    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "const box = document.getElementById('box');\
         box.style.left = '30px'; box.style.width = '200px';",
    );

    let update = expect_single_update(&servo_test, delegate);
    assert_eq!(update.nodes.len(), 1);
    let (updated_node_id, updated_node) = &update.nodes[0];
    assert_eq!(*updated_node_id, div_id);
    let expected = Rect::new(30.0, 20.0, 230.0, 70.0);
    assert_rect_eq(updated_node.bounds().expect("div have bounds"), expected);

    tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let div = root.children().next().expect("Should have a div child");
    assert_rect_eq(div.raw_bounds().expect("div should have bounds"), expected);
}

#[test]
fn test_accessibility_bounds_updated_after_renderer_scroll() {
    let url = "data:text/html,<!DOCTYPE html>\
               <main style='position:absolute;left:10px;top:100px;\
               width:100px;height:50px'>Target</main>\
               <div style='width:2000px;height:2000px'></div>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let main = find_first_matching_node(root, |node| node.role() == Role::Main)
        .expect("Document should contain a main element");
    assert_rect_eq(
        main.raw_bounds().expect("main should have bounds"),
        Rect::new(10.0, 100.0, 110.0, 150.0),
    );

    // A positive delta reveals more content at the bottom and right, so this is equivalent to
    // `window.scrollTo(20, 40)`.
    webview.notify_scroll_event(
        Scroll::Delta(WebViewVector::Device(DeviceVector2D::new(20.0, 40.0))),
        WebViewPoint::Device(DevicePoint::new(250.0, 250.0)),
    );
    update_tree_after_min_updates(&servo_test, &delegate, &mut tree, 1);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let main = find_first_matching_node(root, |node| node.role() == Role::Main)
        .expect("Document should contain a main element");
    let expected = Rect::new(-10.0, 60.0, 90.0, 110.0);
    assert_rect_eq(
        main.bounding_box().expect("main should have bounds"),
        expected,
    );
}

#[test]
fn test_accessibility_bounds_updated_after_script_scroll() {
    let url = "data:text/html,<!DOCTYPE html>\
               <main style='position:absolute;left:10px;top:100px;\
               width:100px;height:50px'>Target</main>\
               <div style='width:2000px;height:2000px'></div>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let main = find_first_matching_node(root, |node| node.role() == Role::Main)
        .expect("Document should contain a main element");
    assert_rect_eq(
        main.raw_bounds().expect("main should have bounds"),
        Rect::new(10.0, 100.0, 110.0, 150.0),
    );

    // Scrolling the viewport down and to the right shifts every viewport-relative bound up and to
    // the left by the same amount, mirroring `..._after_renderer_scroll` but driven from script.
    let _ = evaluate_javascript(&servo_test, webview.clone(), "window.scrollTo(20, 40);");
    update_tree_after_min_updates(&servo_test, &delegate, &mut tree, 1);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let main = find_first_matching_node(root, |node| node.role() == Role::Main)
        .expect("Document should contain a main element");
    let expected = Rect::new(-10.0, 60.0, 90.0, 110.0);
    assert_rect_eq(
        main.bounding_box().expect("main should have bounds"),
        expected,
    );
}

#[test]
fn test_accessibility_build_initial_tree_after_scroll() {
    let servo_test = build_test();
    let url = "data:text/html,<!DOCTYPE html>\
               <main style='position:absolute;left:10px;top:100px;\
               width:100px;height:50px'>Target</main>\
               <div style='width:2000px;height:2000px'></div>";
    let delegate = Rc::new(WebViewDelegateImpl::default());
    let webview = WebViewBuilder::new(servo_test.servo(), servo_test.rendering_context.clone())
        .delegate(delegate.clone())
        .url(Url::parse(url).unwrap())
        .build();
    servo_test.spin(|| webview.load_status() != LoadStatus::Complete);

    // A scroll injected before the scene is built is silently dropped, and the
    // tree asserted below would be the one built without it.
    wait_for_webview_scene_to_be_up_to_date(&servo_test, &webview);
    webview.notify_scroll_event(
        Scroll::Delta(WebViewVector::Device(DeviceVector2D::new(20.0, 40.0))),
        WebViewPoint::Device(DevicePoint::new(250.0, 250.0)),
    );
    wait_for_webview_scene_to_be_up_to_date(&servo_test, &webview);

    webview.set_accessibility_active(true);

    let updates = wait_for_min_updates(&servo_test, delegate.clone(), 2);
    let tree = build_tree(updates);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let main = find_first_matching_node(root, |node| node.role() == Role::Main)
        .expect("Document should contain a main element");
    let expected = Rect::new(-10.0, 60.0, 90.0, 110.0);
    assert_rect_eq(
        main.bounding_box().expect("main should have bounds"),
        expected,
    );
}

#[test]
fn test_accessibility_unchanged_bounds_are_not_resent() {
    // Absolutely positioned elements; resizing one doesn't affect the other
    let url = "data:text/html,<!DOCTYPE html>\
               <section id='a' style='position:absolute;left:0;top:0;width:10px;height:10px'>\
                 <article id='c'></article>\
               </section>\
               <footer id='b' style='position:absolute;left:100px;top:100px;\
               width:10px;height:10px'></footer>";
    let (servo_test, delegate, webview, tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 2);
    let (node_a, node_b) = (children[0], children[1]);
    assert_rect_eq(
        node_a.raw_bounds().expect("a should have bounds"),
        Rect::new(0.0, 0.0, 10.0, 10.0),
    );
    assert_rect_eq(
        node_b.raw_bounds().expect("b should have bounds"),
        Rect::new(100.0, 100.0, 110.0, 110.0),
    );
    let node_b_id = node_b.locate().0;

    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('a').style.width = '50px';",
    );

    let update = expect_single_update(&servo_test, delegate.clone());
    let resent_ids: Vec<NodeId> = update.nodes.iter().map(|(id, _)| *id).collect();
    assert!(
        !resent_ids.contains(&node_b_id),
        "A node whose bounds did not change should not be re-serialized, but got {resent_ids:?}"
    );
}

#[test]
fn test_accessibility_bounds_are_computed_for_inline_elements() {
    let url = "data:text/html,<!DOCTYPE html>\
               <h1>We really <em>really <strong>really</strong></em> like owls</h1>";

    let (_servo_test, _delegate, _webview, tree) = build_webview_and_tree(url);
    let root = assert_tree_structure_and_get_root_web_area(&tree);

    let heading = find_first_matching_node(root, |node| node.role() == Role::Heading)
        .expect("Should be exactly one heading");
    assert_eq!(
        heading.label(),
        Some("We really really really like owls".to_owned())
    );
    assert!(heading.has_bounds());

    let em = find_first_matching_node(heading, |node| node.role() == Role::Emphasis)
        .expect("Heading should have one Emphasis child (HTML-AAM maps <em> to emphasis)");
    assert!(em.has_bounds());

    let strong = find_first_matching_node(em, |node| node.role() == Role::Strong)
        .expect("<em> should have one Strong child");
    assert!(strong.has_bounds());
}

#[test]
fn test_accessibility_update_failed_layout_from_layout_root() {
    // Absolutely positioned elements create layout roots during incremental update.
    let url = "data:text/html,<!DOCTYPE html>\
               <section id='a' style='position:absolute;left:0;top:0;width:10px;height:10px'>\
                 <article id='b' style='position:absolute;top:20px;left:20px;\
                                        width:20px;height:20px;'></article>\
               </section>\
               <footer id='c' style='position:absolute;left:100px;top:100px;\
                                     width:10px;height:10px'></footer>";
    let (servo_test, delegate, webview, tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 2);
    let (node_a, node_c) = (children[0], children[1]);
    assert_rect_eq(
        node_a.raw_bounds().expect("a should have bounds"),
        Rect::new(0.0, 0.0, 10.0, 10.0),
    );
    assert_rect_eq(
        node_c.raw_bounds().expect("c should have bounds"),
        Rect::new(100.0, 100.0, 110.0, 110.0),
    );
    let node_a_id = node_a.locate().0;

    let a_children: Vec<_> = node_a.children().collect();
    let node_b = a_children[0];
    assert_rect_eq(
        node_b.raw_bounds().expect("b should have bounds"),
        Rect::new(20.0, 20.0, 40.0, 40.0),
    );
    let node_b_id = node_b.locate().0;

    // Making `b` position: fixed will cause layout from the layout root at `a` to fail, triggering
    // relayout from the root.
    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "b.style.position = 'fixed'; \
         b.style.width = '30px'; \
         a.style.width = '50px';",
    );

    let update = expect_single_update(&servo_test, delegate.clone());

    // This test really passes if:
    // a) there's no hang because the accessibility tree never updates, and
    // b) the integrity checks in the accessibility tree pass,
    // but let's check the new bounds anyway.

    assert_eq!(update.nodes.len(), 2);

    let node_a = find_node_matching(&update, |&id, _node| id == node_a_id);
    let node_a_bounds = node_a.bounds().expect("a should have bounds after update");
    assert_rect_eq(node_a_bounds, Rect::new(0.0, 0.0, 50.0, 10.0));

    let node_b = find_node_matching(&update, |&id, _node| id == node_b_id);
    let node_b_bounds = node_b.bounds().expect("b should have bounds after update");
    assert_rect_eq(node_b_bounds, Rect::new(20.0, 20.0, 50.0, 40.0));
}

#[test]
fn test_accessibility_bounds_changed_by_sibling() {
    let url = "data:text/html,<!DOCTYPE HTML>\
               <body style='margin:0;'>\
               <main id=main style='width:100px;height:100px;'></main>\
               <footer id=footer style='width:100px;height:100px;'>Hello</footer>\
               </body>";

    let (servo_test, delegate, webview, tree) = build_webview_and_tree(url);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 2);
    let (main, footer) = (children[0], children[1]);
    assert_rect_eq(
        main.raw_bounds().expect("main should have bounds"),
        Rect::new(0.0, 0.0, 100.0, 100.0),
    );
    assert_rect_eq(
        footer.raw_bounds().expect("footer should have bounds"),
        Rect::new(0.0, 100.0, 100.0, 200.0),
    );
    let main_id = main.locate().0;
    let footer_id = footer.locate().0;

    let _ = evaluate_javascript(&servo_test, webview.clone(), "main.style.height = '200px';");

    let update = expect_single_update(&servo_test, delegate.clone());

    let main = find_node_matching(&update, |&id, _node| id == main_id);
    let main_bounds = main.bounds().expect("main should have bounds after update");
    assert_rect_eq(main_bounds, Rect::new(0.0, 0.0, 100.0, 200.0));

    let footer = find_node_matching(&update, |&id, _node| id == footer_id);
    let footer_bounds = footer
        .bounds()
        .expect("footer should have bounds after update");
    assert_rect_eq(footer_bounds, Rect::new(0.0, 200.0, 100.0, 300.0));
}

#[test]
fn test_accessibility_layout_root_node_also_changed() {
    // Absolutely positioned divs create layout roots; if the DOM content of a layout root also
    // changes, we need to make sure both types of damage are handled.
    let url = "data:text/html,<!DOCTYPE html>\
               <article id='a' style='margin:0;position:absolute;left:0;top:0;width:10px;height:10px'>\
                 <p id='c' style='margin:0;width:10px;height:10px'>c</p>\
               </article>\
               <footer id='b' style='position:absolute;left:100px;top:100px;\
                                     width:10px;height:10px'></footer>";

    let (servo_test, delegate, webview, tree) = build_webview_and_tree(url);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 2);
    let (node_a, node_b) = (children[0], children[1]);
    assert_rect_eq(
        node_a.raw_bounds().expect("a should have bounds"),
        Rect::new(0.0, 0.0, 10.0, 10.0),
    );
    let node_a_id = node_a.locate().0;

    assert_rect_eq(
        node_b.raw_bounds().expect("b should have bounds"),
        Rect::new(100.0, 100.0, 110.0, 110.0),
    );

    let node_a_children: Vec<accesskit_consumer::Node> = node_a.children().collect();
    assert_eq!(node_a_children.len(), 1);
    let node_c = node_a_children[0];
    assert_rect_eq(
        node_c.raw_bounds().expect("c should have bounds"),
        Rect::new(0.0, 0.0, 10.0, 10.0),
    );
    let node_c_id = node_c.locate().0;

    // Change a's role, and also c's width. This should mean a has damage both from the DOM change
    // (adding the role attribute), and from layout (as a layout root when layout is changing, due
    // to the change in c).
    let js = "a.setAttribute('role', 'main'); \
         c.style.width = '20px';";
    let _ = evaluate_javascript(&servo_test, webview.clone(), js);

    let update = expect_single_update(&servo_test, delegate);

    // Should be node a (new role), node c (bounds updated).
    assert_eq!(update.nodes.len(), 2);

    // Check that node a's role was indeed changed.
    let node_a_update = find_node_matching(&update, |&id, _node| id == node_a_id);
    assert_eq!(node_a_update.role(), Role::Main);

    // Check that node c's bounds were indeed updated.
    let node_c_update = find_node_matching(&update, |&id, _node| id == node_c_id);
    let node_c_bounds = node_c_update
        .bounds()
        .expect("Node c should have bounds after update");
    assert_rect_eq(node_c_bounds, Rect::new(0.0, 0.0, 20.0, 10.0));
}

#[test]
fn test_accessibility_display_none_change() {
    let url = "data:text/html,<!DOCTYPE html>\
               <style>section.subdued em { display: none }</style>
               <section class='subdued'><h1>We <em>really</em> love the web</h1></section>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let heading = find_first_matching_node(root, |node| node.role() == Role::Heading)
        .expect("Should have a heading");
    let heading_children: Vec<_> = heading.children().collect();
    assert_eq!(heading_children.len(), 3);
    let em = heading_children[1];
    assert_eq!(em.is_hidden(), true);
    assert_eq!(heading.label(), Some("We  love the web".to_owned()));

    // Test that making previously-hidden content visible works correctly.
    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.querySelector('section').removeAttribute('class');",
    );

    let update = expect_single_update(&servo_test, delegate.clone());
    tree.update_and_process_changes(update, &mut NoOpChangeHandler);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let heading = find_first_matching_node(root, |node| node.role() == Role::Heading)
        .expect("Heading should still be in the tree");
    // Un-hiding the <em> should change the computed text of the heading
    assert_eq!(heading.label(), Some("We really love the web".to_owned()));
    let heading_children: Vec<_> = heading.children().collect();
    assert_eq!(heading_children.len(), 3);
    let em = heading_children[1];
    assert_eq!(em.is_hidden(), false);

    // Test that changing a node in a hidden subtree is picked up, even though the node is hidden.
    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.querySelector('section').className = 'subdued';\
         document.querySelector('em').firstChild.appendData(', really');",
    );

    let update = expect_single_update(&servo_test, delegate.clone());
    tree.update_and_process_changes(update, &mut NoOpChangeHandler);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let heading = find_first_matching_node(root, |node| node.role() == Role::Heading)
        .expect("Heading should still be in the tree");
    // Hiding the <em> should change the computed text of the heading
    assert_eq!(heading.label(), Some("We  love the web".to_owned()));
    let heading_children: Vec<_> = heading.children().collect();
    assert_eq!(heading_children.len(), 3);
    let em = heading_children[1];
    assert_eq!(em.is_hidden(), true);

    // Make the previously added text content visible to ensure it was added correctly.
    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.querySelector('section').removeAttribute('class');",
    );

    let mut updates = wait_for_min_updates(&servo_test, delegate, 1);
    assert_eq!(updates.len(), 1);
    let update = updates.pop().expect("Guaranteed by assert above");
    tree.update_and_process_changes(update, &mut NoOpChangeHandler);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let heading = find_first_matching_node(root, |node| node.role() == Role::Heading)
        .expect("Heading should still be in the tree");
    // Un-hiding the <em> should change the computed text of the heading, and the new text should be
    // present
    assert_eq!(
        heading.label(),
        Some("We really, really love the web".to_owned())
    );
    let heading_children: Vec<_> = heading.children().collect();
    assert_eq!(heading_children.len(), 3);
    let em = heading_children[1];
    assert_eq!(em.is_hidden(), false);
}

#[test]
fn test_accessibility_display_none_change_scroll() {
    let url = "data:text/html,<!DOCTYPE html>\
               <body style='margin:0;'>\
                  <main id='main' style='width:1000px;height:100px;\
                                         overflow:scroll'>\
                    <article style='width:1000px;height:50px;'></article>\
                    <div style='width:1000px;height:2000px'></div>\
                    <aside id='aside' style='width:1000px;height:50px;'></aside>\
                  </main>\
               </body>";

    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);
    let root = assert_tree_structure_and_get_root_web_area(&tree);

    let main = find_first_matching_node(root, |node| node.role() == Role::Main)
        .expect("Document should contain a main element");
    assert_rect_eq(
        main.bounding_box().expect("main should have bounds"),
        Rect::new(0.0, 0.0, 1000.0, 100.0),
    );

    let article = find_first_matching_node(root, |node| node.role() == Role::Article)
        .expect("Document should contain an article");
    let article_id = article.locate().0;
    assert_rect_eq(
        article.bounding_box().expect("article should have bounds"),
        Rect::new(0.0, 0.0, 1000.0, 50.0),
    );

    let div = find_first_matching_node(root, |node| node.role() == Role::GenericContainer)
        .expect("Document should contain a div");
    let div_id = div.locate().0;

    let aside = find_first_matching_node(root, |node| node.role() == Role::Complementary)
        .expect("Document should contain an aside");
    assert_rect_eq(
        aside.bounding_box().expect("aside should have bounds"),
        Rect::new(0.0, 2050.0, 1000.0, 2100.0),
    );
    let aside_id = aside.locate().0;

    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "aside.style.display = 'none';",
    );

    let update = expect_single_update(&servo_test, delegate.clone());
    assert_eq!(update.nodes.len(), 1, "only <aside> should be updated");
    let _ = find_node_matching(&update, |id, _node| id == &aside_id);

    tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let aside = find_first_matching_node(root, |node| node.role() == Role::Complementary)
        .expect("Aside should stil be present");
    assert_eq!(aside.is_hidden(), true);
    assert_eq!(aside.bounding_box(), None);

    let _ = evaluate_javascript(&servo_test, webview.clone(), "main.scrollTo(0, 500);");

    let update = expect_single_update(&servo_test, delegate.clone());
    assert_eq!(
        update.nodes.len(),
        2,
        "only <article> and <div> should be updated"
    );
    let transform = Affine::translate((0.0, -500.0));
    let article_data = find_node_matching(&update, |id, _node| id == &article_id);
    assert_eq!(article_data.transform(), Some(&transform));
    let div_data = find_node_matching(&update, |id, _node| id == &div_id);
    assert_eq!(div_data.transform(), Some(&transform));

    tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let article = find_first_matching_node(root, |node| node.role() == Role::Article)
        .expect("Document should contain an article");
    assert_rect_eq(
        article.bounding_box().expect("article should have bounds"),
        Rect::new(0.0, -500.0, 1000.0, -450.0),
    );
    let _ = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "aside.style.removeProperty('display');",
    );

    let update = expect_single_update(&servo_test, delegate);
    assert_eq!(update.nodes.len(), 1, "only <aside> should be updated");
    let aside_data = find_node_matching(&update, |id, _node| id == &aside_id);
    assert_eq!(aside_data.transform(), Some(&transform));

    tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let aside = find_first_matching_node(root, |node| node.role() == Role::Complementary)
        .expect("Document should contain an aside");
    assert_eq!(aside.is_hidden(), false);
    assert_rect_eq(
        aside.bounding_box().expect("aside should have bounds"),
        Rect::new(0.0, 1550.0, 1000.0, 1600.0),
    );
}

#[test]
fn test_accessibility_click_link() {
    let url = "data:text/html,<!DOCTYPE html>\
               <a id='link' href='data:text/html,just%20a%20text%20node'>this is a link</a>";
    let (servo_test, delegate, _webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    assert_eq!(children.len(), 1);
    let link = children[0];
    assert_eq!(link.role(), Role::Link);
    assert_eq!(link.label(), Some("this is a link".to_owned()));

    let (target_node, target_tree) = link.locate();
    let action_request = ActionRequest {
        action: Action::Click,
        target_tree,
        target_node,
        data: None,
    };

    servo_test
        .servo
        .forward_accessibility_action(action_request);

    // Graft node, new pipeline, focus graft node.
    update_tree_after_min_updates(&servo_test, &delegate, &mut tree, 3);
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let children: Vec<accesskit_consumer::Node> = root.children().collect();
    let text = children[0];
    assert_eq!(text.role(), Role::TextRun);
    assert_eq!(text.value(), Some("just a text node".to_owned()));
}

#[test]
fn test_accessibility_focus_moves() {
    let url = "data:text/html,<!DOCTYPE html>\
               <a id='link' href='%23main'>Skip to main content</a>\
               <nav>\
                   <a id='news' href='/news'>News</a>\
                   <a id='contact' href='/contact'>Contact</a>\
               </nav>\
               <main id='main'>\
                   <p>\
                       <a href='https://servo.org'>Servo</a> aims to empower developers\
                   </p>\
               </main>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let focused_node = tree.state().focus().expect("Should be a focused node");
    assert_eq!(
        focused_node.data().html_tag(),
        Some("html"),
        "Focus is on document root node on initial page load"
    );

    press_tab_key(&webview);

    let updates = wait_for_min_updates(&servo_test, delegate.clone(), 1);
    for update in updates {
        tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    }

    let focused_node = tree.state().focus().expect("Should be a focused node");
    assert_eq!(
        focused_node.label(),
        Some("Skip to main content".to_owned()),
        "Focus is on skip link after pressing tab"
    );

    // Click the skip link
    let (target_node, target_tree) = focused_node.locate();
    let action_request = ActionRequest {
        action: Action::Click,
        target_tree,
        target_node,
        data: None,
    };

    servo_test
        .servo
        .forward_accessibility_action(action_request);

    let updates = wait_for_min_updates(&servo_test, delegate.clone(), 1);
    assert_eq!(updates.len(), 1);
    for update in updates {
        tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    }

    let focused_node = tree.state().focus().expect("Should be a focused node");
    assert_eq!(
        focused_node.data().html_tag(),
        Some("html"),
        "Focus is back on document root node after clicking skip link"
    );

    press_tab_key(&webview);

    let updates = wait_for_min_updates(&servo_test, delegate.clone(), 1);
    for update in updates {
        tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    }

    let focused_node = tree.state().focus().expect("Should be a focused node");
    assert_eq!(
        focused_node.label(),
        Some("Servo".to_owned()),
        "Focus is on Servo link after pressing tab"
    );

    // Move focus using JavaScript
    let _ = evaluate_javascript(&servo_test, webview.clone(), "contact.focus();");

    let updates = wait_for_min_updates(&servo_test, delegate.clone(), 1);
    for update in updates {
        tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    }

    let focused_node = tree.state().focus().expect("Should be a focused node");
    assert_eq!(
        focused_node.label(),
        Some("Contact".to_owned()),
        "Focus is on Contact link after focusing it programatically"
    );
}

// ************************************************************************************************
// If you're adding a new test here, consider adding a matching test in
// tests/wpt/mozilla/tests/accessibility-tree/
// ************************************************************************************************

/// Rendering context size in device pixels (HiDPI scale = 1.0 in tests, so also CSS pixels).
const TEST_VIEWPORT_SIZE: f64 = 500.0;

/// Find the single node with the given role in a [`TreeUpdate`]. The order of the nodes in an
/// update is unspecified, so tests must not depend on it.
#[track_caller]
fn find_node_with_role(update: &TreeUpdate, role: Role) -> &accesskit::Node {
    find_node_matching(update, |_, node| node.role() == role)
}

/// Find the single node matching the given predicate in a [`TreeUpdate`].
#[track_caller]
fn find_node_matching(
    update: &TreeUpdate,
    mut predicate: impl FnMut(&NodeId, &accesskit::Node) -> bool,
) -> &accesskit::Node {
    let mut matches = update.nodes.iter().filter(|(id, node)| predicate(id, node));
    let node = matches
        .next()
        .expect("Exactly one node should match predicate");
    assert!(
        matches.next().is_none(),
        "Exactly one node should match predicate"
    );
    &node.1
}

#[track_caller]
fn assert_rect_eq(actual: Rect, expected: Rect) {
    // Bounds are converted from `Au`, which has a resolution of 1/60th of a CSS pixel.
    const EPSILON: f64 = 0.05;
    assert!(
        (actual.x0 - expected.x0).abs() < EPSILON &&
            (actual.y0 - expected.y0).abs() < EPSILON &&
            (actual.x1 - expected.x1).abs() < EPSILON &&
            (actual.y1 - expected.y1).abs() < EPSILON,
        "expected bounds {expected:?} but got {actual:?}"
    );
}

fn build_test() -> ServoTest {
    let servo_test = ServoTest::new_with_builder(|builder| {
        let mut preferences = Preferences::default();
        preferences.accessibility_enabled = true;
        preferences.dom_servo_helpers_enabled = true;
        preferences.expensive_accessibility_test_assertions_enabled = true;
        let mut opts = Opts::default();
        opts.debug
            .toggle_option(DiagnosticsLoggingOption::AccessibilityTree, true);
        builder.preferences(preferences).opts(opts)
    });
    servo_test
}

fn build_webview_and_tree(
    url: &str,
) -> (
    ServoTest,
    Rc<WebViewDelegateImpl>,
    servo::WebView,
    accesskit_consumer::Tree,
) {
    let servo_test = build_test();
    let delegate = Rc::new(WebViewDelegateImpl::default());
    let webview = WebViewBuilder::new(servo_test.servo(), servo_test.rendering_context.clone())
        .delegate(delegate.clone())
        .url(Url::parse(url).unwrap())
        .build();
    webview.set_accessibility_active(true);
    let load_webview = webview.clone();
    servo_test.spin(|| load_webview.load_status() != LoadStatus::Complete);

    let updates = wait_for_min_updates(&servo_test, delegate.clone(), 3);
    let tree = build_tree(updates);

    // Neither load status nor accessibility updates imply a built WebRender
    // scene, and callers inject input as soon as this returns.
    wait_for_webview_scene_to_be_up_to_date(&servo_test, &webview);

    (servo_test, delegate, webview, tree)
}

fn wait_for_min_updates(
    servo_test: &ServoTest,
    delegate: Rc<WebViewDelegateImpl>,
    min_num_updates: usize,
) -> Vec<TreeUpdate> {
    servo_test.spin(|| delegate.last_accesskit_tree_updates.borrow().len() < min_num_updates);

    delegate
        .last_accesskit_tree_updates
        .borrow_mut()
        .drain(..)
        .collect()
}

fn expect_single_update(servo_test: &ServoTest, delegate: Rc<WebViewDelegateImpl>) -> TreeUpdate {
    let mut updates = wait_for_min_updates(&servo_test, delegate.clone(), 1);
    assert_eq!(updates.len(), 1);
    updates.pop().expect("Guaranteed by assert above")
}

fn apply_single_update<'tree>(
    servo_test: &ServoTest,
    delegate: Rc<WebViewDelegateImpl>,
    tree: &'tree mut accesskit_consumer::Tree,
) -> accesskit_consumer::Node<'tree> {
    let update = expect_single_update(servo_test, delegate);
    tree.update_and_process_changes(update, &mut NoOpChangeHandler);
    assert_tree_structure_and_get_root_web_area(tree)
}

fn build_tree(tree_updates: Vec<TreeUpdate>) -> accesskit_consumer::Tree {
    let first_update = tree_updates[0].clone();
    let tree_id = first_update.tree_id;

    // We need to make a TreeUpdate with a TreeId of ROOT, which can have the subtrees grafted in
    let root_node_id = NodeId(0x0);
    let mut root_node = accesskit::Node::new(Role::GenericContainer);

    // We need to make a graft node so that we have a non-graft node to set as the initial focused
    // node for the tree.
    //
    // This stands in for the node an embedder builds to graft in a WebView's tree (see
    // `ports/servoshell/desktop/gui.rs`). It deliberately has no transform: the WebView is at the
    // origin of the window and the HiDPI scale factor is 1.0 in tests, so composed bounds are
    // equal to the viewport-relative CSS pixel bounds that layout produces. It also sets no
    // bounds, matching the embedder, as AccessKit consumers exclude graft nodes from the
    // presented tree and never read them.
    let graft_node_id = NodeId(0x1);
    let mut graft_node = accesskit::Node::new(Role::GenericContainer);
    graft_node.set_tree_id(tree_id);

    root_node.set_children(vec![graft_node_id]);

    let root_tree = accesskit::Tree {
        root: root_node_id,
        toolkit_name: None,
        toolkit_version: None,
    };

    let root_update = TreeUpdate {
        nodes: vec![(root_node_id, root_node), (graft_node_id, graft_node)],
        tree: Some(root_tree.clone()),
        tree_id: TreeId::ROOT,
        focus: root_node_id,
    };

    let mut tree = accesskit_consumer::Tree::new(root_update, true /* is_host_focused */);

    for tree_update in tree_updates {
        tree.update_and_process_changes(tree_update, &mut NoOpChangeHandler);
    }

    // After building the rest of the tree, focus the graft node.
    let focus_update = TreeUpdate {
        nodes: vec![],
        tree: Some(root_tree),
        tree_id: TreeId::ROOT,
        focus: graft_node_id,
    };
    tree.update_and_process_changes(focus_update, &mut NoOpChangeHandler);

    tree
}

fn assert_tree_structure_and_get_root_web_area<'tree>(
    tree: &'tree accesskit_consumer::Tree,
) -> accesskit_consumer::Node<'tree> {
    let root_node = tree.state().root();
    let scroll_view = find_first_matching_node(root_node, |node| node.role() == Role::ScrollView)
        .expect("Tree should include a scroll view corresponding to the WebView.");
    let scroll_view_children: Vec<accesskit_consumer::Node<'_>> = scroll_view.children().collect();
    assert_eq!(scroll_view_children.len(), 1);
    let graft_node = scroll_view_children[0];
    assert!(graft_node.is_graft());

    find_first_matching_node(graft_node, |node| node.role() == Role::RootWebArea)
        .expect("Should have a RootWebArea")
}

fn update_tree_after_min_updates(
    servo_test: &ServoTest,
    delegate: &Rc<WebViewDelegateImpl>,
    tree: &mut accesskit_consumer::Tree,
    min_updates: usize,
) {
    let new_updates = wait_for_min_updates(&servo_test, delegate.clone(), min_updates);
    for tree_update in new_updates {
        tree.update_and_process_changes(tree_update, &mut NoOpChangeHandler);
    }
}

fn find_first_matching_node(
    root_node: accesskit_consumer::Node<'_>,
    mut predicate: impl FnMut(&accesskit_consumer::Node) -> bool,
) -> Option<accesskit_consumer::Node<'_>> {
    let mut children = root_node.children().collect::<VecDeque<_>>();
    while let Some(candidate) = children.pop_front() {
        if predicate(&candidate) {
            return Some(candidate);
        }
        for child in candidate.children() {
            children.push_back(child);
        }
    }
    None
}

fn find_all_matching_nodes(
    root_node: accesskit_consumer::Node<'_>,
    mut predicate: impl FnMut(&accesskit_consumer::Node) -> bool,
) -> Vec<accesskit_consumer::Node<'_>> {
    let mut children = root_node.children().collect::<VecDeque<_>>();
    let mut result = vec![];
    while let Some(candidate) = children.pop_front() {
        if predicate(&candidate) {
            result.push(candidate);
        }
        for child in candidate.children() {
            children.push_back(child);
        }
    }
    result
}

/// Wait for the WebRender scene to reflect the current state of the WebView
/// by triggering a screenshot, waiting for it to be ready, and then throwing
/// away the results.
fn wait_for_webview_scene_to_be_up_to_date(servo_test: &ServoTest, webview: &WebView) {
    let waiting = Rc::new(Cell::new(true));
    let callback_waiting = waiting.clone();
    webview.take_screenshot(None, move |result| {
        assert!(result.is_ok());
        callback_waiting.set(false);
    });
    servo_test.spin(|| waiting.get());
}

fn press_tab_key(webview: &servo::WebView) {
    webview.notify_input_event(InputEvent::Keyboard(KeyboardEvent::from_state_and_key(
        KeyState::Down,
        Key::Named(NamedKey::Tab),
    )));
    webview.notify_input_event(InputEvent::Keyboard(KeyboardEvent::from_state_and_key(
        KeyState::Up,
        Key::Named(NamedKey::Tab),
    )));
}

/// Apply incoming accessibility updates until `done(tree)` holds; panics after 10 s.
fn update_tree_until(
    servo_test: &ServoTest,
    delegate: &Rc<WebViewDelegateImpl>,
    tree: &mut accesskit_consumer::Tree,
    mut done: impl FnMut(&accesskit_consumer::Tree) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done(tree) {
        assert!(
            std::time::Instant::now() < deadline,
            "update_tree_until timed out after 10 s"
        );
        servo_test.spin(|| delegate.last_accesskit_tree_updates.borrow().is_empty());
        let updates: Vec<TreeUpdate> = delegate
            .last_accesskit_tree_updates
            .borrow_mut()
            .drain(..)
            .collect();
        for update in updates {
            tree.update_and_process_changes(update, &mut NoOpChangeHandler);
        }
    }
}

/// Find the node with the given role and label anywhere under `root`.
#[track_caller]
fn find_node_by_role_and_label<'tree>(
    root: accesskit_consumer::Node<'tree>,
    role: Role,
    label: &str,
) -> accesskit_consumer::Node<'tree> {
    find_first_matching_node(root, |node| {
        node.role() == role && node.label().as_deref() == Some(label)
    })
    .unwrap_or_else(|| panic!("no node with role {role:?} and label {label:?}"))
}

/// Whether the node advertises support for `action` (using the common filter for parents).
fn node_supports_action(node: &accesskit_consumer::Node, action: Action) -> bool {
    node.supports_action(action, &accesskit_consumer::common_filter)
}

/// Spin the event loop for roughly `millis`, so that any pending actions can (not) take effect.
fn spin_briefly(servo_test: &ServoTest, millis: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(millis);
    while std::time::Instant::now() < deadline {
        servo_test.servo.spin_event_loop();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn test_accessibility_lucina_password_value_never_exposed() {
    let url = "data:text/html,<label for='p'>Password</label>\
               <input id='p' type='password' value='hunter2'>\
               <input id='c' autocomplete='cc-number' value='4111111111111111' aria-label='Card'>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    // No node of the tree built from every update so far (build_webview_and_tree drains them
    // into `tree`), nor any update still pending, may hold the secret values or a bullet.
    let mut all_updates_debug = String::new();
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let nodes = find_all_matching_nodes(root, |_| true);
    assert!(
        nodes.len() >= 3,
        "expected the label and both fields, got {}",
        nodes.len()
    );
    for node in nodes {
        let _ = write!(all_updates_debug, "{:?}", node.data());
    }
    for update in delegate.last_accesskit_tree_updates.borrow().iter() {
        let _ = write!(all_updates_debug, "{:?}", update);
    }
    assert!(
        !all_updates_debug.contains("hunter2"),
        "a tree update leaked the password value"
    );
    assert!(
        !all_updates_debug.contains("4111"),
        "a tree update leaked the card number"
    );
    assert!(
        !all_updates_debug.contains('•'),
        "a tree update contained a bullet character"
    );

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let password = find_node_by_role_and_label(root, Role::PasswordInput, "Password");
    assert_eq!(password.value(), None);
    assert_eq!(password.children().count(), 0);
    assert!(!node_supports_action(&password, Action::SetValue));
    assert!(node_supports_action(&password, Action::Focus));

    let card = find_node_by_role_and_label(root, Role::TextInput, "Card");
    assert_eq!(card.value(), None);
    assert!(!node_supports_action(&card, Action::SetValue));
    assert_eq!(card.class_name(), Some("secret"));

    // SetValue on the password field must do nothing.
    let (target_node, target_tree) = password.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::SetValue,
            target_tree,
            target_node,
            data: Some(accesskit::ActionData::Value("x".into())),
        });
    spin_briefly(&servo_test, 500);
    match evaluate_javascript(
        &servo_test,
        webview.clone(),
        "document.getElementById('p').value",
    ) {
        Ok(servo::JSValue::String(s)) => assert_eq!(s, "hunter2"),
        other => panic!("expected string value, got {other:?}"),
    }
}

#[test]
fn test_accessibility_lucina_text_field_name_value_placeholder() {
    let url = "data:text/html,<label>Find <input id='q' type='search' value='milk' placeholder='What'></label>";
    let (servo_test, delegate, _webview, mut tree) = build_webview_and_tree(url);
    let _ = &servo_test;
    let _ = &delegate;

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let field = find_node_by_role_and_label(root, Role::SearchInput, "Find");
    assert_eq!(field.value(), Some("milk".to_owned()));
    assert_eq!(field.placeholder(), Some("What"));
    assert_eq!(field.children().count(), 0);
    assert!(node_supports_action(&field, Action::SetValue));
    assert!(node_supports_action(&field, Action::Focus));
    assert!(node_supports_action(&field, Action::Click));
}

#[test]
fn test_accessibility_lucina_set_value_fires_input_and_change() {
    let url = "data:text/html,<input id='q' aria-label='Query'>\
               <script>window.seen=[];\
               q.addEventListener('input',e=>seen.push('input:'+e.isTrusted));\
               q.addEventListener('change',e=>seen.push('change:'+e.isTrusted));</script>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let field = find_node_by_role_and_label(root, Role::TextInput, "Query");
    let (target_node, target_tree) = field.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::SetValue,
            target_tree,
            target_node,
            data: Some(accesskit::ActionData::Value("eggs".into())),
        });

    update_tree_until(&servo_test, &delegate, &mut tree, |tree| {
        let root = tree.state().root();
        find_first_matching_node(root, |node| {
            node.role() == Role::TextInput && node.value().as_deref() == Some("eggs")
        })
        .is_some()
    });

    match evaluate_javascript(&servo_test, webview.clone(), "seen.join(',')") {
        Ok(servo::JSValue::String(s)) => assert_eq!(s, "input:true,change:true"),
        other => panic!("expected string, got {other:?}"),
    }
    match evaluate_javascript(&servo_test, webview.clone(), "q.value") {
        Ok(servo::JSValue::String(s)) => assert_eq!(s, "eggs"),
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn test_accessibility_lucina_fill_and_submit_search_form() {
    let url = "data:text/html,<form action='data:text/html,results'>\
               <input name='q' aria-label='Search'><button>Go</button></form>";
    let (servo_test, delegate, _webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let field = find_node_by_role_and_label(root, Role::TextInput, "Search");
    let (target_node, target_tree) = field.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::SetValue,
            target_tree,
            target_node,
            data: Some(accesskit::ActionData::Value("eggs".into())),
        });
    update_tree_until(&servo_test, &delegate, &mut tree, |tree| {
        find_first_matching_node(tree.state().root(), |node| {
            node.role() == Role::TextInput && node.value().as_deref() == Some("eggs")
        })
        .is_some()
    });

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let button = find_node_by_role_and_label(root, Role::Button, "Go");
    let (target_node, target_tree) = button.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::Click,
            target_tree,
            target_node,
            data: None,
        });
    update_tree_until(&servo_test, &delegate, &mut tree, |tree| {
        find_first_matching_node(tree.state().root(), |node| {
            node.role() == Role::TextRun && node.value().map_or(false, |v| v.contains("q=eggs"))
        })
        .is_some()
    });
}

#[test]
fn test_accessibility_lucina_select_by_click_and_set_value() {
    let url = "data:text/html,<label for='s'>Size</label>\
               <select id='s'><option>Small</option><option selected>Large</option></select>\
               <script>window.changes=0;s.addEventListener('change',()=>changes++);</script>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let combo = find_node_by_role_and_label(root, Role::ComboBox, "Size");
    assert_eq!(combo.value(), Some("Large".to_owned()));
    let options: Vec<accesskit_consumer::Node> = combo.children().collect();
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].role(), Role::MenuListOption);
    assert_eq!(options[0].label(), Some("Small".to_owned()));
    assert_eq!(options[1].role(), Role::MenuListOption);
    assert_eq!(options[1].label(), Some("Large".to_owned()));
    assert_eq!(options[1].is_selected(), Some(true));

    // Click the "Small" option.
    let (target_node, target_tree) = options[0].locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::Click,
            target_tree,
            target_node,
            data: None,
        });
    update_tree_until(&servo_test, &delegate, &mut tree, |tree| {
        find_first_matching_node(tree.state().root(), |node| {
            node.role() == Role::ComboBox && node.value().as_deref() == Some("Small")
        })
        .is_some()
    });
    match evaluate_javascript(&servo_test, webview.clone(), "s.value") {
        Ok(servo::JSValue::String(s)) => assert_eq!(s, "Small"),
        other => panic!("expected string, got {other:?}"),
    }
    match evaluate_javascript(&servo_test, webview.clone(), "window.changes") {
        Ok(servo::JSValue::Number(n)) => assert_eq!(n, 1.0),
        other => panic!("expected number, got {other:?}"),
    }

    // SetValue "Large" on the combo box itself.
    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let combo = find_node_by_role_and_label(root, Role::ComboBox, "Size");
    let (target_node, target_tree) = combo.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::SetValue,
            target_tree,
            target_node,
            data: Some(accesskit::ActionData::Value("Large".into())),
        });
    update_tree_until(&servo_test, &delegate, &mut tree, |tree| {
        find_first_matching_node(tree.state().root(), |node| {
            node.role() == Role::ComboBox && node.value().as_deref() == Some("Large")
        })
        .is_some()
    });
}

#[test]
fn test_accessibility_lucina_checkbox_click_toggles() {
    let url = "data:text/html,<input type='checkbox' id='c'><label for='c'>Agree</label>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let checkbox = find_node_by_role_and_label(root, Role::CheckBox, "Agree");
    assert_eq!(checkbox.toggled(), Some(accesskit::Toggled::False));

    let (target_node, target_tree) = checkbox.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::Click,
            target_tree,
            target_node,
            data: None,
        });
    update_tree_until(&servo_test, &delegate, &mut tree, |tree| {
        find_first_matching_node(tree.state().root(), |node| {
            node.role() == Role::CheckBox && node.toggled() == Some(accesskit::Toggled::True)
        })
        .is_some()
    });
    match evaluate_javascript(&servo_test, webview.clone(), "c.checked") {
        Ok(servo::JSValue::Boolean(b)) => assert!(b),
        other => panic!("expected boolean, got {other:?}"),
    }
}

#[test]
fn test_accessibility_lucina_focus_and_blur_actions() {
    let url = "data:text/html,<input aria-label='A'><input aria-label='B'>";
    let (servo_test, delegate, _webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let b = find_node_by_role_and_label(root, Role::TextInput, "B");
    let (target_node, target_tree) = b.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::Focus,
            target_tree,
            target_node,
            data: None,
        });
    update_tree_until(&servo_test, &delegate, &mut tree, |tree| {
        tree.state()
            .focus()
            .map_or(false, |f| f.label().as_deref() == Some("B"))
    });

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let b = find_node_by_role_and_label(root, Role::TextInput, "B");
    let (target_node, target_tree) = b.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::Blur,
            target_tree,
            target_node,
            data: None,
        });
    update_tree_until(&servo_test, &delegate, &mut tree, |tree| {
        tree.state()
            .focus()
            .map_or(true, |f| f.label().as_deref() != Some("B"))
    });
}

/// Actions that arrive together are performed one after another, and one action's script can
/// remove a later action's target: that action is skipped, and the ones after it still run.
#[test]
fn test_accessibility_lucina_action_on_a_node_an_earlier_action_removed() {
    let url = "data:text/html,<script>window.clicks=[];</script>\
               <button id='a' onclick=\"document.getElementById('b').remove();\
               let junk=[];for(let k=0;k<200000;k++){junk.push({k});}junk=null;\
               clicks.push('A')\">A</button>\
               <button id='b' onclick=\"clicks.push('B')\">B</button>\
               <button id='c' onclick=\"clicks.push('C')\">C</button>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let targets: Vec<_> = ["A", "B", "C"]
        .into_iter()
        .map(|label| find_node_by_role_and_label(root, Role::Button, label).locate())
        .collect();
    // Sent without spinning in between, so that layout resolves them in the same reflow.
    for (target_node, target_tree) in targets {
        servo_test
            .servo
            .forward_accessibility_action(ActionRequest {
                action: Action::Click,
                target_tree,
                target_node,
                data: None,
            });
    }
    update_tree_until(&servo_test, &delegate, &mut tree, |tree| {
        find_first_matching_node(tree.state().root(), |node| {
            node.role() == Role::Button && node.label().as_deref() == Some("B")
        })
        .is_none()
    });
    spin_briefly(&servo_test, 500);

    match evaluate_javascript(&servo_test, webview.clone(), "clicks.join(',')") {
        Ok(servo::JSValue::String(s)) => assert_eq!(s, "A,C"),
        other => panic!("expected string, got {other:?}"),
    }
}

#[test]
fn test_accessibility_lucina_scroll_actions() {
    let url = "data:text/html,<div style='height:3000px'></div><button>Far</button>";
    let (servo_test, delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let far = find_node_by_role_and_label(root, Role::Button, "Far");
    let (target_node, target_tree) = far.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::ScrollIntoView,
            target_tree,
            target_node,
            data: None,
        });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut scrolled = false;
    while std::time::Instant::now() < deadline && !scrolled {
        servo_test.servo.spin_event_loop();
        std::thread::sleep(std::time::Duration::from_millis(20));
        if let Ok(servo::JSValue::Number(n)) =
            evaluate_javascript(&servo_test, webview.clone(), "window.scrollY")
        {
            scrolled = n > 0.0;
        }
    }
    assert!(
        scrolled,
        "window.scrollY should be > 0 after ScrollIntoView"
    );

    let _ = evaluate_javascript(&servo_test, webview.clone(), "window.scrollTo(0, 0);");
    let html = find_first_matching_node(tree.state().root(), |node| {
        node.data().html_tag() == Some("html")
    })
    .expect("should have an html root node");
    let (target_node, target_tree) = html.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::ScrollDown,
            target_tree,
            target_node,
            data: None,
        });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut scrolled = false;
    while std::time::Instant::now() < deadline && !scrolled {
        servo_test.servo.spin_event_loop();
        std::thread::sleep(std::time::Duration::from_millis(20));
        if let Ok(servo::JSValue::Number(n)) =
            evaluate_javascript(&servo_test, webview.clone(), "window.scrollY")
        {
            scrolled = n > 0.0;
        }
    }
    assert!(
        scrolled,
        "window.scrollY should be > 0 after ScrollDown on the root"
    );
    let _ = &delegate;
}

#[test]
fn test_accessibility_lucina_disabled_button_refuses_click() {
    let url = "data:text/html,<button disabled onclick='window.clicked=1'>No</button>";
    let (servo_test, _delegate, webview, mut tree) = build_webview_and_tree(url);

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let button = find_node_by_role_and_label(root, Role::Button, "No");
    assert!(button.is_disabled());
    assert!(!node_supports_action(&button, Action::Click));

    let (target_node, target_tree) = button.locate();
    servo_test
        .servo
        .forward_accessibility_action(ActionRequest {
            action: Action::Click,
            target_tree,
            target_node,
            data: None,
        });
    spin_briefly(&servo_test, 500);
    match evaluate_javascript(&servo_test, webview.clone(), "window.clicked === undefined") {
        Ok(servo::JSValue::Boolean(b)) => assert!(b),
        other => panic!("expected boolean, got {other:?}"),
    }
}

#[test]
fn test_accessibility_lucina_names_levels_urls_hidden() {
    let url = "data:text/html,<h2>Title</h2><a href='https://example.org/x'>X</a><img alt='A cat' src='data:, '>\
               <span id='l'>Lab</span><button aria-labelledby='l'>ignored</button>\
               <button aria-label='Close'>x</button><input title='Tip'>\
               <div aria-hidden='true'><button>Secret</button></div>";
    let (servo_test, _delegate, _webview, mut tree) = build_webview_and_tree(url);
    let _ = &servo_test;

    let root = assert_tree_structure_and_get_root_web_area(&tree);
    let heading = find_node_by_role_and_label(root, Role::Heading, "Title");
    assert_eq!(heading.level(), Some(2));

    let link = find_node_by_role_and_label(root, Role::Link, "X");
    assert!(
        link.url().map_or(false, |u| u.ends_with("/x")),
        "link url should end with /x, got {:?}",
        link.url()
    );

    let image = find_node_by_role_and_label(root, Role::Image, "A cat");
    assert_eq!(image.label(), Some("A cat".to_owned()));

    let labelled = find_node_by_role_and_label(root, Role::Button, "Lab");
    assert_eq!(labelled.label(), Some("Lab".to_owned()));
    let close = find_node_by_role_and_label(root, Role::Button, "Close");
    assert_eq!(close.label(), Some("Close".to_owned()));
    let tip = find_node_by_role_and_label(root, Role::TextInput, "Tip");
    assert_eq!(tip.label(), Some("Tip".to_owned()));

    let hidden = find_first_matching_node(root, |node| {
        node.data().html_tag() == Some("div") && node.is_hidden()
    })
    .expect("aria-hidden div should be hidden");
    assert!(hidden.is_hidden());
}
