//! The page's accessibility tree, as Servo builds it (AccessKit `TreeUpdate`s through
//! `WebViewDelegate::notify_accessibility_tree_update`), served to Lucina through Juno's own
//! AccessKit adapter, `lucina-ax` (the LucinaOS accessibility work): the
//! same socket, requests, replies, version and rules as every Juno app, so one reader in
//! `lucina-devd` covers apps and pages.
//!
//! What this crate adds for the web:
//! - It keeps the page from Servo's incremental updates ([`PageAx::apply`]) and makes one whole
//!   tree of it, without the nodes a screen reader skips: hidden subtrees, unnamed generic
//!   containers (their children move up) and empty text runs.
//! - Servo speaks AccessKit 0.24, `lucina-ax` 0.25: the two have the same roles, actions and
//!   properties (0.25 adds `html_id`), so the tree crosses by its serde form.
//! - [`lucina_ax::Snapshot::from_update_with`] applies Juno's rules (no password value, size
//!   limits for a web page, cut text), keeps a link's URL, a field's placeholder, a heading's level
//!   and the states, and marks every node's text `web <origin>`: content from the page, to be read
//!   as data, never as instructions.
//! - An action is checked by `Snapshot::plan` (Verus-proved `decide`) and then by Servo itself
//!   against the live element (`accessibility_rules::check_action`, also proved). The reply comes
//!   once the page has answered, with the tree as it is then.
//!
//! See `docs/accessibility-tree.md`.

use std::collections::{HashMap, HashSet, VecDeque};

use accesskit::{Action, ActionData, ActionRequest, Node, NodeId, Role, Tree, TreeId, TreeUpdate};
pub use lucina_ax;
use lucina_ax::proto::{Request, refusal};
use serde_json::Value;

/// The app name in replies.
pub const APP: &str = "lucinanet";

/// One document's tree.
struct Document {
    tree_id: TreeId,
    root: NodeId,
    focus: NodeId,
    nodes: HashMap<NodeId, Node>,
}

/// The tree of the page in one WebView, from Servo's updates.
#[derive(Default)]
pub struct PageAx {
    /// The WebView's own tree, which only grafts the document's in. Its updates are not the page.
    webview_tree: Option<TreeId>,
    document: Option<Document>,
    /// Counts updates, so that a waiting reply knows the page answered.
    changes: u64,
    origin: String,
}

/// What [`PageAx::answer`] did.
pub enum Answer {
    /// Reply with this now.
    Reply(Value),
    /// An action went to the engine: reply with the tree once the page has answered, adding
    /// `done: <action>`.
    Forwarded { action: String },
}

impl PageAx {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tree id `WebView::set_accessibility_active` returned.
    pub fn set_webview_tree(&mut self, tree_id: TreeId) {
        self.webview_tree = Some(tree_id);
    }

    /// The page's origin: every node's text is content from it.
    pub fn set_origin(&mut self, origin: String) {
        self.origin = origin;
        self.changes += 1;
    }

    /// Forget the page: the tree was turned off. The next one starts afresh.
    pub fn clear(&mut self) {
        self.webview_tree = None;
        self.document = None;
        self.changes += 1;
    }

    /// Whether a page's tree has arrived.
    pub fn has_page(&self) -> bool {
        self.document.is_some()
    }

    /// How many updates the page has had.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// Apply one update from Servo. A full update for a new document tree (a navigation) replaces
    /// the page.
    pub fn apply(&mut self, update: TreeUpdate) {
        if Some(update.tree_id) == self.webview_tree {
            return;
        }
        let same_document = self
            .document
            .as_ref()
            .is_some_and(|document| document.tree_id == update.tree_id);
        if !same_document {
            let Some(Tree { root, .. }) = update.tree else {
                // An update for a document whose start we haven't seen: Servo always sends the
                // tree first, so this is an old document's, after a navigation.
                return;
            };
            self.document = Some(Document {
                tree_id: update.tree_id,
                root,
                focus: update.focus,
                nodes: HashMap::new(),
            });
        }
        let document = self.document.as_mut().expect("set above");
        if let Some(tree) = &update.tree {
            document.root = tree.root;
        }
        document.focus = update.focus;
        for (id, node) in update.nodes {
            document.nodes.insert(id, node);
        }
        // AccessKit drops nodes no longer reachable from the root.
        let reachable = reachable(document);
        document.nodes.retain(|id, _| reachable.contains(id));
        self.changes += 1;
    }

    /// The page as one whole `TreeUpdate`, without what a screen reader skips: hidden subtrees,
    /// unnamed generic containers with no actions (their children move up) and empty text runs.
    pub fn page_update(&self) -> Option<TreeUpdate> {
        let document = self.document.as_ref()?;
        let mut nodes = Vec::new();
        let mut queue = VecDeque::from([document.root]);
        let mut seen = HashSet::new();
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            let Some(node) = document.nodes.get(&id) else {
                continue;
            };
            let mut node = node.clone();
            let children = shown_children(document, &node);
            queue.extend(children.iter().copied());
            node.set_children(children);
            nodes.push((id, node));
        }
        let focus = if seen.contains(&document.focus) {
            document.focus
        } else {
            document.root
        };
        Some(TreeUpdate {
            nodes,
            tree: Some(Tree::new(document.root)),
            tree_id: document.tree_id,
            focus,
        })
    }

    /// The page as Juno's adapter presents any app: its rules applied, with the web's own
    /// properties and provenance added.
    pub fn snapshot(&self) -> Option<lucina_ax::Snapshot> {
        let document = self.document.as_ref()?;
        let update = self.page_update()?;
        // AccessKit 0.24 to 0.25 by their shared serde form.
        let update: lucina_ax::accesskit::TreeUpdate =
            serde_json::from_value(serde_json::to_value(&update).ok()?).ok()?;
        let provenance = format!("web {}", self.origin);
        let mut snapshot = lucina_ax::Snapshot::from_update_with(
            APP,
            &update,
            &lucina_ax::tree::Source {
                toolkit: "servo",
                provenance: Some(&provenance),
                web: true,
            },
        );
        // A secret field that isn't a password (a one-time code, a card): Servo withholds its value
        // and says so with class_name "secret"; mark it as `lucina-ax` marks a password, so that the
        // assistant asks the owner rather than reading an empty field.
        for (id, _) in &snapshot.update.nodes {
            if document.nodes.get(&NodeId(id.0)).and_then(Node::class_name) == Some("secret") {
                snapshot
                    .extras
                    .entry(lucina_ax::tree::id_text(id.0))
                    .or_default()
                    .secret = true;
            }
        }
        Some(snapshot)
    }

    /// Answer one request of `lucina-ax`'s protocol. An admitted action goes to `forward`, as an
    /// `ActionRequest` for Servo.
    pub fn answer(
        &self,
        request: Result<Request, String>,
        pid: u32,
        forward: &mut dyn FnMut(ActionRequest),
    ) -> Answer {
        let request = match request {
            Ok(request) => request,
            Err(why) => return Answer::Reply(refusal("bad_request", &why, None)),
        };
        let Some(snapshot) = self.snapshot() else {
            return Answer::Reply(refusal("no_page", "no page has loaded yet", None));
        };
        match request {
            Request::Tree => Answer::Reply(snapshot.reply(pid)),
            Request::Act {
                id,
                action,
                value,
                version,
                ..
            } => {
                let planned = match snapshot.plan(&id, &action, value.as_deref(), &version) {
                    Ok(planned) => planned,
                    Err(refused) => {
                        return Answer::Reply(refusal(
                            refused.code(),
                            &refused.why(),
                            Some(snapshot.reply(pid)),
                        ));
                    }
                };
                let (Some(document), Some(node)) =
                    (self.document.as_ref(), lucina_ax::tree::parse_id(&id))
                else {
                    return Answer::Reply(refusal("no_page", "the page went away", None));
                };
                // The same action in AccessKit 0.24, by name.
                let Some(servo_action) = serde_json::to_value(planned.action)
                    .ok()
                    .and_then(|name| serde_json::from_value::<Action>(name).ok())
                else {
                    return Answer::Reply(refusal("bad_request", "no such action in Servo", None));
                };
                let data = match servo_action {
                    Action::SetValue => planned.value.map(|value| ActionData::Value(value.into())),
                    _ => None,
                };
                forward(ActionRequest {
                    action: servo_action,
                    target_tree: document.tree_id,
                    target_node: NodeId(node),
                    data,
                });
                Answer::Forwarded { action }
            }
            // A page has no app intents of its own, and LucinaNet registers none.
            Request::Intent { .. } => {
                Answer::Reply(refusal("unsupported", "LucinaNet answers no app intents", None))
            }
        }
    }
}

/// A node's children as shown: hidden ones dropped, empty text runs dropped, and unnamed
/// generic containers without actions replaced by their own shown children.
fn shown_children(document: &Document, node: &Node) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeId> = node.children().iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        let Some(child) = document.nodes.get(&id) else {
            continue;
        };
        if child.is_hidden() {
            continue;
        }
        match child.role() {
            Role::TextRun if child.value().is_none_or(|text| text.trim().is_empty()) => {}
            Role::GenericContainer if child.label().is_none() && !has_actions(child) => {
                stack.extend(child.children().iter().rev().copied());
            }
            _ => out.push(id),
        }
    }
    out
}

fn has_actions(node: &Node) -> bool {
    [
        Action::Click,
        Action::Focus,
        Action::SetValue,
        Action::ScrollDown,
    ]
    .into_iter()
    .any(|action| node.supports_action(action))
}

fn reachable(document: &Document) -> HashSet<NodeId> {
    let mut seen = HashSet::new();
    let mut stack = vec![document.root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(node) = document.nodes.get(&id) {
            stack.extend(node.children().iter().copied());
        }
    }
    seen
}

/// How long the tree stays on after the last request. `lucina-ax`'s protocol is one connection per
/// request, so "nobody reads it any more" is this long without a request.
pub const IDLE_OFF: std::time::Duration = std::time::Duration::from_secs(30);

/// What the socket's thread asks of the engine's.
#[derive(Debug)]
pub enum Control {
    /// Build the tree: someone reads it.
    On,
    /// Stop building it: nobody has asked for [`IDLE_OFF`].
    Off,
    /// An admitted action.
    Act(ActionRequest),
}

/// How long the first request waits for the page's first tree after turning it on: building it for
/// a large page takes seconds on the laptop's small cores.
pub const FIRST_TREE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Answer `lucina-ax` requests on `server` from a thread of its own. The tree costs nothing until
/// someone reads it: the first request turns it on (and waits, at most [`FIRST_TREE_WAIT`], for
/// the page's first tree), and [`IDLE_OFF`] without a request turns it off again. An admitted action
/// goes to the engine too; its reply waits until the page has answered (an update, then a quarter
/// second without one, at most three seconds) and carries the tree as it is then.
pub fn serve(
    mut server: lucina_ax::Server,
    page: std::sync::Arc<std::sync::Mutex<PageAx>>,
    control: impl Fn(Control) + Send + 'static,
) -> std::io::Result<()> {
    use std::time::{Duration, Instant};

    /// Wait until the page has changed since `before` and then been quiet a quarter second, or three
    /// seconds have passed.
    fn settle(page: &std::sync::Mutex<PageAx>, before: u64) {
        let started = Instant::now();
        let (mut seen, mut quiet_since) = (before, Instant::now());
        while started.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(25));
            let now = page.lock().unwrap().changes();
            if now != seen {
                (seen, quiet_since) = (now, Instant::now());
            } else if seen != before && quiet_since.elapsed() >= Duration::from_millis(250) {
                break;
            }
        }
    }

    let pid = std::process::id();
    std::thread::Builder::new().name("ax-socket".into()).spawn(move || {
        let mut last_request: Option<Instant> = None;
        loop {
            let Some((waiting, request)) = server.take_request() else {
                if last_request.is_some_and(|at| at.elapsed() >= IDLE_OFF) {
                    control(Control::Off);
                    page.lock().unwrap().clear();
                    last_request = None;
                }
                // Sleep until someone connects, or a second passes (to see IDLE_OFF out).
                let mut fd = libc::pollfd { fd: server.fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: one valid pollfd for the duration of the call.
                unsafe { libc::poll(&mut fd, 1, 1000) };
                continue;
            };
            if last_request.is_none() {
                let before = page.lock().unwrap().changes();
                control(Control::On);
                let started = Instant::now();
                while !page.lock().unwrap().has_page() && started.elapsed() < FIRST_TREE_WAIT {
                    std::thread::sleep(Duration::from_millis(25));
                }
                settle(&page, before);
            }
            last_request = Some(Instant::now());
            let before = page.lock().unwrap().changes();
            let answer = page
                .lock()
                .unwrap()
                .answer(request, pid, &mut |request| control(Control::Act(request)));
            let reply = match answer {
                Answer::Reply(reply) => reply,
                Answer::Forwarded { action } => {
                    settle(&page, before);
                    let mut reply = page
                        .lock()
                        .unwrap()
                        .snapshot()
                        .map(|snapshot| snapshot.reply(pid))
                        .unwrap_or_else(|| serde_json::json!({"ok": true}));
                    reply["done"] = Value::String(action);
                    reply
                },
            };
            waiting.reply(&reply);
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;
