//! LucinaNet's accessibility socket by hand, with the requests `lucina-devd` sends (`lucina-ax`'s protocol), for the build host,
//! where there is no `/run/lucina` and so no `lucina-ax` command target:
//!
//! ```text
//! axctl <socket> tree                                the page as Lucina reads it (the reply's text)
//! axctl <socket> json                                the whole reply
//! axctl <socket> find <role> <name>                  the id of the first node with that role and name
//! axctl <socket> act <id> <action> [value]           act on the current tree, then print the page as it answered
//! ```
//!
//! `<action>` is `click`, `focus`, `blur`, `set_value`, `scroll_into_view`, `scroll_down` or `scroll_up`; `<role>` is AccessKit's name, such as
//! `SearchInput` or `Button`.

use std::path::Path;
use std::time::Duration;

use lucinanet_ax::lucina_ax::accesskit::TreeUpdate;
use serde_json::{Value, json};

fn ask(socket: &str, request: Value) -> Value {
    lucinanet_ax::lucina_ax::proto::request(Path::new(socket), &request, Duration::from_secs(10))
        .expect("LucinaNet's accessibility socket answers")
}

fn find(reply: &Value, role: &str, name: &str) -> Option<String> {
    let update: TreeUpdate = serde_json::from_value(reply["update"].clone()).ok()?;
    update
        .nodes
        .iter()
        .find(|(_, node)| {
            format!("{:?}", node.role()).eq_ignore_ascii_case(role) && node.label() == Some(name)
        })
        .map(|(id, _)| lucinanet_ax::lucina_ax::tree::id_text(id.0))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: axctl <socket> tree|json|find <role> <name>|act <id> <action> [value]";
    let (Some(socket), Some(command)) = (args.get(1), args.get(2)) else {
        eprintln!("{usage}");
        std::process::exit(2);
    };
    let tree = ask(socket, json!({"op": "tree"}));
    match (command.as_str(), args.get(3), args.get(4)) {
        ("tree", _, _) => print!("{}", tree["text"].as_str().unwrap_or("")),
        ("json", _, _) => println!("{tree}"),
        ("find", Some(role), Some(name)) => match find(&tree, role, name) {
            Some(id) => println!("{id}"),
            None => {
                eprintln!("no {role} named {name:?}");
                std::process::exit(1);
            }
        },
        ("act", Some(id), Some(action)) => {
            let mut request =
                json!({"op": "act", "id": id, "action": action, "version": tree["version"]});
            if let Some(value) = args.get(5) {
                request["value"] = json!(value);
            }
            let reply = ask(socket, request);
            print!("{}", reply["text"].as_str().unwrap_or(""));
            eprintln!(
                "ok={} done={} error={} why={}",
                reply["ok"], reply["done"], reply["error"], reply["why"]
            );
            if reply["ok"] != json!(true) {
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
}
