//! Unit tests for crates/lucinanet-ax (LucinaNet), one file

use super::*;
use accesskit::{Action, Node, NodeId, Role, Tree, TreeId, TreeUpdate};
use lucina_ax::proto::Request;

#[test]
fn store_basics() {
    let mut page = PageAx::new();
    let tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let root_id = NodeId(1);
    let button_id = NodeId(2);
    let text_field_id = NodeId(3);
    let link_id = NodeId(4);
    let heading_id = NodeId(5);

    let mut root = Node::new(Role::RootWebArea);
    root.set_children(vec![button_id, text_field_id, link_id, heading_id]);

    let mut button = Node::new(Role::Button);
    button.set_label("Go".to_string());
    button.add_action(Action::Click);

    let mut text_field = Node::new(Role::TextInput);
    text_field.set_value("milk".to_string());
    text_field.set_placeholder("Find".to_string());
    text_field.add_action(Action::SetValue);

    let mut link = Node::new(Role::Link);
    link.set_label("Docs".to_string());
    link.set_url("https://example.org/docs".to_string());
    link.add_action(Action::Click);

    let mut heading = Node::new(Role::Heading);
    heading.set_label("Title".to_string());
    heading.set_level(2);

    let update = TreeUpdate {
        tree: Some(Tree::new(root_id)),
        tree_id,
        nodes: vec![
            (root_id, root),
            (button_id, button),
            (text_field_id, text_field),
            (link_id, link),
            (heading_id, heading),
        ],
        focus: NodeId(0),
    };

    page.apply(update);
    let snapshot = page.snapshot().unwrap();

    assert_eq!(snapshot.update.nodes.len(), 5);
    assert!(
        snapshot
            .update
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Go"))
    );
    assert!(
        snapshot
            .update
            .nodes
            .iter()
            .any(|(_, node)| node.value() == Some("milk"))
    );
    assert!(
        snapshot
            .update
            .nodes
            .iter()
            .any(|(_, node)| node.placeholder() == Some("Find"))
    );
    assert!(
        snapshot
            .update
            .nodes
            .iter()
            .any(|(_, node)| node.url() == Some("https://example.org/docs"))
    );
    assert!(
        snapshot
            .update
            .nodes
            .iter()
            .any(|(_, node)| node.level() == Some(2))
    );
}

#[test]
fn webview_tree_ignored() {
    let mut page = PageAx::new();
    let webview_tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000002\"").unwrap();
    page.set_webview_tree(webview_tree_id);

    let update = TreeUpdate {
        tree: None,
        tree_id: webview_tree_id,
        nodes: vec![],
        focus: NodeId(0),
    };

    page.apply(update);
    assert!(page.page_update().is_none());
}

#[test]
fn unreachable_nodes_dropped() {
    let mut page = PageAx::new();
    let tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let root_id = NodeId(1);
    let button_id = NodeId(2);
    let text_field_id = NodeId(3);

    let mut root = Node::new(Role::RootWebArea);
    root.set_children(vec![button_id, text_field_id]);

    let mut button = Node::new(Role::Button);
    button.set_label("Go".to_string());
    button.add_action(Action::Click);

    let mut text_field = Node::new(Role::TextInput);
    text_field.set_value("milk".to_string());
    text_field.add_action(Action::SetValue);

    let update = TreeUpdate {
        tree: Some(Tree::new(root_id)),
        tree_id,
        nodes: vec![
            (root_id, root),
            (button_id, button),
            (text_field_id, text_field),
        ],
        focus: NodeId(0),
    };

    page.apply(update);
    let page_update = page.page_update().unwrap();
    assert_eq!(page_update.nodes.len(), 3);

    let mut root = Node::new(Role::RootWebArea);
    root.set_children(vec![button_id]);

    let update = TreeUpdate {
        tree: None,
        tree_id,
        nodes: vec![(root_id, root)],
        focus: NodeId(0),
    };

    page.apply(update);
    let page_update = page.page_update().unwrap();
    assert_eq!(page_update.nodes.len(), 2);
}

#[test]
fn navigation_replaces() {
    let mut page = PageAx::new();
    let tree_id_1 =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let tree_id_2 =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000002\"").unwrap();
    let root_id_1 = NodeId(1);
    let root_id_2 = NodeId(2);

    let mut root_1 = Node::new(Role::RootWebArea);
    root_1.set_children(vec![NodeId(2)]);

    let mut root_2 = Node::new(Role::RootWebArea);
    root_2.set_children(vec![NodeId(3)]);

    let update_1 = TreeUpdate {
        tree: Some(Tree::new(root_id_1)),
        tree_id: tree_id_1,
        nodes: vec![(root_id_1, root_1)],
        focus: NodeId(0),
    };

    let update_2 = TreeUpdate {
        tree: Some(Tree::new(root_id_2)),
        tree_id: tree_id_2,
        nodes: vec![(root_id_2, root_2)],
        focus: NodeId(0),
    };

    page.apply(update_1);
    page.apply(update_2);
    let page_update = page.page_update().unwrap();
    assert_eq!(page_update.tree_id, tree_id_2);
}

#[test]
fn flattening() {
    let mut page = PageAx::new();
    let tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let root_id = NodeId(1);
    let container_id = NodeId(2);
    let button_id = NodeId(3);
    let paragraph_id = NodeId(4);
    let hidden_button_id = NodeId(5);
    let text_run_id = NodeId(6);

    let mut root = Node::new(Role::RootWebArea);
    root.set_children(vec![container_id, paragraph_id, text_run_id]);

    let mut container = Node::new(Role::GenericContainer);
    container.set_children(vec![button_id]);

    let mut button = Node::new(Role::Button);
    button.set_label("Deep".to_string());

    let mut paragraph = Node::new(Role::Paragraph);
    paragraph.set_hidden();
    paragraph.set_children(vec![hidden_button_id]);

    let mut hidden_button = Node::new(Role::Button);
    hidden_button.set_label("Hidden".to_string());

    let mut text_run = Node::new(Role::TextRun);
    text_run.set_value("   ".to_string());

    let update = TreeUpdate {
        tree: Some(Tree::new(root_id)),
        tree_id,
        nodes: vec![
            (root_id, root),
            (container_id, container),
            (button_id, button),
            (paragraph_id, paragraph),
            (hidden_button_id, hidden_button),
            (text_run_id, text_run),
        ],
        focus: NodeId(0),
    };

    page.apply(update);
    let page_update = page.page_update().unwrap();

    // The root and the "Deep" button: the container is lifted, the hidden paragraph and the blank run dropped.
    assert_eq!(page_update.nodes.len(), 2);
    assert!(
        page_update
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Deep"))
    );
    assert!(
        !page_update
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Hidden"))
    );
    assert!(
        !page_update
            .nodes
            .iter()
            .any(|(_, node)| node.value() == Some("   "))
    );
}

#[test]
fn provenance() {
    let mut page = PageAx::new();
    page.set_origin("https://example.org".to_string());
    let tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let root_id = NodeId(1);
    let button_id = NodeId(2);

    let mut root = Node::new(Role::RootWebArea);
    root.set_children(vec![button_id]);

    let mut button = Node::new(Role::Button);
    button.set_label("Go".to_string());

    let update = TreeUpdate {
        tree: Some(Tree::new(root_id)),
        tree_id,
        nodes: vec![(root_id, root), (button_id, button)],
        focus: NodeId(0),
    };

    page.apply(update);
    let snapshot = page.snapshot().unwrap();

    let reply = snapshot.reply(1);
    assert_eq!(
        reply["juno"][lucina_ax::tree::id_text(button_id.0)]["provenance"],
        "web https://example.org"
    );
}

#[test]
fn password_value_never_leaves() {
    let mut page = PageAx::new();
    let tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let root_id = NodeId(1);
    let password_id = NodeId(2);

    let mut root = Node::new(Role::RootWebArea);
    root.set_children(vec![password_id]);

    let mut password = Node::new(Role::PasswordInput);
    password.set_value("hunter2".to_string());

    let update = TreeUpdate {
        tree: Some(Tree::new(root_id)),
        tree_id,
        nodes: vec![(root_id, root), (password_id, password)],
        focus: NodeId(0),
    };

    page.apply(update);
    let snapshot = page.snapshot().unwrap();

    assert!(
        snapshot
            .update
            .nodes
            .iter()
            .all(|(_, node)| node.value().is_none())
    );
    let reply = snapshot.reply(1);
    assert!(!serde_json::to_string(&reply).unwrap().contains("hunter2"));
}

#[test]
fn answer_tree() {
    let mut page = PageAx::new();
    let tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let root_id = NodeId(1);

    let root = Node::new(Role::RootWebArea);

    let update = TreeUpdate {
        tree: Some(Tree::new(root_id)),
        tree_id,
        nodes: vec![(root_id, root)],
        focus: NodeId(0),
    };

    page.apply(update);
    let snapshot = page.snapshot().unwrap();
    let version = snapshot.version.clone();

    let answer = page.answer(Ok(Request::Tree), 1, &mut |_| {});
    if let Answer::Reply(reply) = answer {
        assert_eq!(reply["ok"], serde_json::json!(true));
        assert_eq!(reply["app"], "lucinanet");
        assert_eq!(reply["version"], version);
        assert!(!reply["text"].as_str().unwrap().is_empty());
    } else {
        panic!("Expected Reply");
    }
}

#[test]
fn answer_act() {
    let mut page = PageAx::new();
    let tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let root_id = NodeId(1);
    let button_id = NodeId(2);
    let text_field_id = NodeId(3);
    let password_id = NodeId(4);

    let mut root = Node::new(Role::RootWebArea);
    root.set_children(vec![button_id, text_field_id, password_id]);

    let mut button = Node::new(Role::Button);
    button.set_label("Go".to_string());
    button.add_action(Action::Click);

    let mut text_field = Node::new(Role::TextInput);
    text_field.set_value("milk".to_string());
    text_field.add_action(Action::SetValue);

    let mut password = Node::new(Role::PasswordInput);
    password.set_value("hunter2".to_string());
    password.add_action(Action::SetValue);

    let update = TreeUpdate {
        tree: Some(Tree::new(root_id)),
        tree_id,
        nodes: vec![
            (root_id, root),
            (button_id, button),
            (text_field_id, text_field),
            (password_id, password),
        ],
        focus: NodeId(0),
    };

    page.apply(update);
    let snapshot = page.snapshot().unwrap();
    let version = snapshot.version.clone();
    let button_id_text = lucina_ax::tree::id_text(button_id.0);
    let text_field_id_text = lucina_ax::tree::id_text(text_field_id.0);
    let password_id_text = lucina_ax::tree::id_text(password_id.0);

    let mut forward_called = false;
    let answer = page.answer(
        Ok(Request::Act {
            id: button_id_text.clone(),
            action: "click".to_string(),
            value: None,
            version: version.clone(),
            source: None,
        }),
        1,
        &mut |request| {
            forward_called = true;
            assert_eq!(request.action, Action::Click);
            assert_eq!(request.target_tree, tree_id);
            assert_eq!(request.target_node, button_id);
            assert!(request.data.is_none());
        },
    );
    assert!(matches!(answer, Answer::Forwarded { .. }));
    assert!(forward_called);

    forward_called = false;
    let answer = page.answer(
        Ok(Request::Act {
            id: text_field_id_text.clone(),
            action: "set_value".to_string(),
            value: Some("eggs".to_string()),
            version: version.clone(),
            source: None,
        }),
        1,
        &mut |request| {
            forward_called = true;
            assert_eq!(request.action, Action::SetValue);
            assert_eq!(request.target_tree, tree_id);
            assert_eq!(request.target_node, text_field_id);
            assert_eq!(
                request.data.unwrap(),
                ActionData::Value("eggs".to_string().into())
            );
        },
    );
    assert!(matches!(answer, Answer::Forwarded { .. }));
    assert!(forward_called);

    let answer = page.answer(
        Ok(Request::Act {
            id: button_id_text.clone(),
            action: "click".to_string(),
            value: None,
            version: "000000000000".to_string(),
            source: None,
        }),
        1,
        &mut |_| {
            panic!("Forward should not be called");
        },
    );
    if let Answer::Reply(reply) = answer {
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(reply["error"], "stale");
    } else {
        panic!("Expected Reply");
    }

    let answer = page.answer(
        Ok(Request::Act {
            id: "99".to_string(),
            action: "click".to_string(),
            value: None,
            version: version.clone(),
            source: None,
        }),
        1,
        &mut |_| {
            panic!("Forward should not be called");
        },
    );
    if let Answer::Reply(reply) = answer {
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(reply["error"], "no_node");
    } else {
        panic!("Expected Reply");
    }

    let answer = page.answer(
        Ok(Request::Act {
            id: password_id_text.clone(),
            action: "set_value".to_string(),
            value: Some("new_password".to_string()),
            version: version.clone(),
            source: None,
        }),
        1,
        &mut |_| {
            panic!("Forward should not be called");
        },
    );
    if let Answer::Reply(reply) = answer {
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(reply["error"], "refused_sensitive");
    } else {
        panic!("Expected Reply");
    }

    let answer = page.answer(
        Ok(Request::Act {
            id: button_id_text.clone(),
            action: "set_value".to_string(),
            value: Some("value".to_string()),
            version: version.clone(),
            source: None,
        }),
        1,
        &mut |_| {
            panic!("Forward should not be called");
        },
    );
    if let Answer::Reply(reply) = answer {
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(reply["error"], "not_supported");
    } else {
        panic!("Expected Reply");
    }

    let answer = page.answer(
        Ok(Request::Act {
            id: "xyz".to_string(),
            action: "click".to_string(),
            value: None,
            version: version.clone(),
            source: None,
        }),
        1,
        &mut |_| {
            panic!("Forward should not be called");
        },
    );
    if let Answer::Reply(reply) = answer {
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(reply["error"], "bad_request");
    } else {
        panic!("Expected Reply");
    }

    let answer = page.answer(Err("garbage".to_string()), 1, &mut |_| {
        panic!("Forward should not be called");
    });
    if let Answer::Reply(reply) = answer {
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(reply["error"], "bad_request");
    } else {
        panic!("Expected Reply");
    }

    let empty_page = PageAx::new();
    let answer = empty_page.answer(
        Ok(Request::Act {
            id: button_id_text.clone(),
            action: "click".to_string(),
            value: None,
            version: version.clone(),
            source: None,
        }),
        1,
        &mut |_| {
            panic!("Forward should not be called");
        },
    );
    if let Answer::Reply(reply) = answer {
        assert_eq!(reply["ok"], serde_json::json!(false));
        assert_eq!(reply["error"], "no_page");
    } else {
        panic!("Expected Reply");
    }
}

#[test]
fn secret_fields_are_marked() {
    let mut page = PageAx::new();
    let tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    let mut root = Node::new(Role::RootWebArea);
    root.set_children(vec![NodeId(2), NodeId(3), NodeId(4)]);
    let mut password = Node::new(Role::PasswordInput);
    password.set_label("Password");
    // A card number: Servo sends it without a value and marks it secret.
    let mut card = Node::new(Role::TextInput);
    card.set_label("Card");
    card.set_class_name("secret");
    let mut email = Node::new(Role::TextInput);
    email.set_label("Email");
    email.set_value("a@b.c");
    page.apply(TreeUpdate {
        tree: Some(Tree::new(NodeId(1))),
        tree_id,
        nodes: vec![
            (NodeId(1), root),
            (NodeId(2), password),
            (NodeId(3), card),
            (NodeId(4), email),
        ],
        focus: NodeId(1),
    });
    let reply = page.snapshot().unwrap().reply(1);
    let secret = |id: u64| reply["juno"][lucina_ax::tree::id_text(id)]["secret"].clone();
    assert_eq!(secret(2), serde_json::json!(true));
    assert_eq!(secret(3), serde_json::json!(true));
    assert_eq!(secret(4), serde_json::Value::Null);
}

#[test]
fn answer_intent() {
    let mut page = PageAx::new();
    let tree_id =
        serde_json::from_str::<TreeId>("\"00000000-0000-0000-0000-000000000001\"").unwrap();
    page.apply(TreeUpdate {
        tree: Some(Tree::new(NodeId(1))),
        tree_id,
        nodes: vec![(NodeId(1), Node::new(Role::RootWebArea))],
        focus: NodeId(1),
    });

    let answer = page.answer(
        Ok(Request::Intent {
            call: serde_json::json!({"app": "lucinanet", "intent": "open"}),
        }),
        1,
        &mut |_| panic!("Forward should not be called"),
    );
    match answer {
        Answer::Reply(reply) => {
            assert_eq!(reply["ok"], serde_json::json!(false));
            assert_eq!(reply["error"], "unsupported");
            assert_eq!(reply["why"], "LucinaNet answers no app intents");
        }
        Answer::Forwarded { .. } => panic!("an intent has nothing to wait for"),
    }
}
