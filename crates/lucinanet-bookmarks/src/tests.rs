/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use crate::*;

const T0: i64 = 1_730_000_000_000_000;

fn titles(bm: &Bookmarks, folder: Id) -> Vec<String> {
    bm.children(folder)
        .unwrap()
        .iter()
        .map(|c| bm.get(*c).unwrap().title.clone())
        .collect()
}

type Shape = (usize, String, String, Vec<String>, Option<String>, i64, i64);

/// Everything below `folder` as (depth, kind, title-or-url, tags, keyword, added, modified), ids left out.
fn shape(bm: &Bookmarks, folder: Id) -> Vec<Shape> {
    fn go(bm: &Bookmarks, folder: Id, depth: usize, out: &mut Vec<Shape>) {
        for c in bm.children(folder).unwrap() {
            let n = bm.get(*c).unwrap();
            let kind = match &n.kind {
                Kind::Bookmark { url } => format!("b:{url}"),
                Kind::Folder => "f".to_string(),
                Kind::Separator => "s".to_string(),
            };
            out.push((depth, kind, n.title.clone(), n.tags.clone(), n.keyword.clone(), n.added_us, n.modified_us));
            if matches!(n.kind, Kind::Folder) {
                go(bm, *c, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    go(bm, folder, 0, &mut out);
    out
}

fn add(bm: &mut Bookmarks, parent: Id, title: &str, url: &str) -> Id {
    bm.add_bookmark(parent, None, title, url, T0).unwrap()
}

#[test]
fn new_has_builtin_folders_and_ids() {
    let bm = Bookmarks::new();
    assert_eq!(bm.get(ROOT).unwrap().parent, None);
    assert_eq!(bm.get(TOOLBAR).unwrap().title, "Bookmarks Toolbar");
    assert_eq!(bm.get(MENU).unwrap().title, "Bookmarks Menu");
    assert_eq!(bm.get(OTHER).unwrap().title, "Other Bookmarks");
    for id in [TOOLBAR, MENU, OTHER] {
        assert_eq!(bm.get(id).unwrap().parent, Some(ROOT));
        assert_eq!(bm.get(id).unwrap().kind, Kind::Folder);
    }
    assert_eq!(bm.children(ROOT).unwrap(), &[TOOLBAR, MENU, OTHER]);
    assert_eq!(bm.len_bookmarks(), 0);
    let mut bm = bm;
    assert_eq!(bm.add_folder(MENU, None, "f", T0).unwrap(), 4);
    assert!(bm.get(99).is_none());
}

#[test]
fn add_bookmark_appends_and_normalises_url() {
    let mut bm = Bookmarks::new();
    let a = bm.add_bookmark(TOOLBAR, None, "A", "  https://Example.org  ", T0).unwrap();
    let b = add(&mut bm, TOOLBAR, "B", "https://example.net/x");
    assert_eq!(bm.children(TOOLBAR).unwrap(), &[a, b]);
    assert_eq!(bm.get(a).unwrap().kind, Kind::Bookmark { url: "https://example.org/".into() });
    assert_eq!(bm.get(a).unwrap().added_us, T0);
    assert_eq!(bm.get(a).unwrap().parent, Some(TOOLBAR));
    assert_eq!(bm.len_bookmarks(), 2);
}

#[test]
fn add_bookmark_rejects_empty_and_garbage() {
    let mut bm = Bookmarks::new();
    assert_eq!(bm.add_bookmark(MENU, None, "x", "", T0), Err(Error::EmptyUrl));
    assert_eq!(bm.add_bookmark(MENU, None, "x", "   ", T0), Err(Error::EmptyUrl));
    assert_eq!(
        bm.add_bookmark(MENU, None, "x", "not a url", T0),
        Err(Error::InvalidUrl("not a url".into()))
    );
    assert!(bm.add_bookmark(MENU, None, "x", "no-scheme.example/path", T0).is_err());
    for ok in ["about:blank", "file:///tmp/x.html", "javascript:void(0)", "data:text/html,hi"] {
        assert!(bm.add_bookmark(MENU, None, "x", ok, T0).is_ok(), "{ok}");
    }
    assert_eq!(bm.add_bookmark(99, None, "x", "https://a.example/", T0), Err(Error::NoSuchNode(99)));
    assert_eq!(bm.len_bookmarks(), 4);
}

#[test]
fn add_at_index_inserts_and_bounds_check() {
    let mut bm = Bookmarks::new();
    let a = add(&mut bm, MENU, "a", "https://a.example/");
    let b = add(&mut bm, MENU, "b", "https://b.example/");
    let c = bm.add_bookmark(MENU, Some(1), "c", "https://c.example/", T0).unwrap();
    assert_eq!(bm.children(MENU).unwrap(), &[a, c, b]);
    let d = bm.add_bookmark(MENU, Some(3), "d", "https://d.example/", T0).unwrap();
    assert_eq!(bm.children(MENU).unwrap(), &[a, c, b, d]);
    let e = bm.add_folder(MENU, Some(0), "e", T0).unwrap();
    assert_eq!(bm.children(MENU).unwrap()[0], e);
    assert_eq!(bm.add_bookmark(MENU, Some(6), "x", "https://x.example/", T0), Err(Error::IndexOutOfRange));
    assert_eq!(bm.add_folder(MENU, Some(6), "x", T0), Err(Error::IndexOutOfRange));
    assert_eq!(bm.add_separator(MENU, Some(6), T0), Err(Error::IndexOutOfRange));
    assert_eq!(bm.children(MENU).unwrap().len(), 5);
}

#[test]
fn folders_nest_and_walk_is_depth_first() {
    let mut bm = Bookmarks::new();
    let f1 = bm.add_folder(TOOLBAR, None, "f1", T0).unwrap();
    add(&mut bm, f1, "x", "https://x.example/");
    let f2 = bm.add_folder(f1, None, "f2", T0).unwrap();
    add(&mut bm, f2, "y", "https://y.example/");
    add(&mut bm, TOOLBAR, "z", "https://z.example/");
    let walked: Vec<(usize, String)> = bm.walk().into_iter().map(|(d, n)| (d, n.title.clone())).collect();
    let want: Vec<(usize, String)> = [
        (0, "Bookmarks Toolbar"),
        (1, "f1"),
        (2, "x"),
        (2, "f2"),
        (3, "y"),
        (1, "z"),
        (0, "Bookmarks Menu"),
        (0, "Other Bookmarks"),
    ]
    .iter()
    .map(|(d, t)| (*d, t.to_string()))
    .collect();
    assert_eq!(walked, want);
}

#[test]
fn separator_cannot_have_children() {
    let mut bm = Bookmarks::new();
    let s = bm.add_separator(MENU, None, T0).unwrap();
    assert_eq!(bm.get(s).unwrap().title, "");
    assert_eq!(bm.add_bookmark(s, None, "x", "https://x.example/", T0), Err(Error::NotAFolder(s)));
    assert_eq!(bm.add_folder(s, None, "x", T0), Err(Error::NotAFolder(s)));
    assert_eq!(bm.children(s), Err(Error::NotAFolder(s)));
    let b = add(&mut bm, MENU, "b", "https://b.example/");
    assert_eq!(bm.add_separator(b, None, T0), Err(Error::NotAFolder(b)));
    assert_eq!(bm.rename(s, "name", T0), Err(Error::Format("separators have no title".into())));
    // Only the three built-in folders live in ROOT.
    assert_eq!(bm.add_folder(ROOT, None, "x", T0), Err(Error::ProtectedNode(ROOT)));
}

#[test]
fn remove_folder_removes_subtree_and_counts() {
    let mut bm = Bookmarks::new();
    let f = bm.add_folder(MENU, None, "f", T0).unwrap();
    let inner = bm.add_folder(f, None, "inner", T0).unwrap();
    let b1 = add(&mut bm, inner, "b1", "https://1.example/");
    add(&mut bm, f, "b2", "https://2.example/");
    bm.add_separator(f, None, T0).unwrap();
    let keep = add(&mut bm, MENU, "keep", "https://k.example/");
    assert_eq!(bm.remove(f), Ok(5));
    assert!(bm.get(f).is_none() && bm.get(inner).is_none() && bm.get(b1).is_none());
    assert_eq!(bm.children(MENU).unwrap(), &[keep]);
    assert_eq!(bm.remove(f), Err(Error::NoSuchNode(f)));
    assert_eq!(bm.len_bookmarks(), 1);
    // Ids are not reused.
    let again = bm.add_folder(MENU, None, "again", T0).unwrap();
    assert!(again > keep);
}

#[test]
fn builtin_folders_are_protected() {
    let mut bm = Bookmarks::new();
    let b = add(&mut bm, MENU, "b", "https://b.example/");
    for id in [ROOT, TOOLBAR, MENU, OTHER] {
        assert_eq!(bm.remove(id), Err(Error::ProtectedNode(id)));
        assert_eq!(bm.rename(id, "x", T0), Err(Error::ProtectedNode(id)));
        assert_eq!(bm.move_to(id, OTHER, None, T0), Err(Error::ProtectedNode(id)));
    }
    assert_eq!(bm.move_to(b, ROOT, None, T0), Err(Error::ProtectedNode(ROOT)));
    assert_eq!(bm.get(MENU).unwrap().title, "Bookmarks Menu");
}

#[test]
fn move_between_folders() {
    let mut bm = Bookmarks::new();
    let a = add(&mut bm, TOOLBAR, "a", "https://a.example/");
    let b = add(&mut bm, TOOLBAR, "b", "https://b.example/");
    let x = add(&mut bm, MENU, "x", "https://x.example/");
    bm.move_to(a, MENU, Some(0), T0 + 5).unwrap();
    assert_eq!(bm.children(TOOLBAR).unwrap(), &[b]);
    assert_eq!(bm.children(MENU).unwrap(), &[a, x]);
    assert_eq!(bm.get(a).unwrap().parent, Some(MENU));
    assert_eq!(bm.get(a).unwrap().modified_us, T0 + 5);
    assert_eq!(bm.get(a).unwrap().added_us, T0);
    bm.move_to(b, MENU, None, T0).unwrap();
    assert_eq!(bm.children(MENU).unwrap(), &[a, x, b]);
    assert_eq!(bm.move_to(a, OTHER, Some(1), T0), Err(Error::IndexOutOfRange));
    assert_eq!(bm.move_to(a, x, None, T0), Err(Error::NotAFolder(x)));
    assert_eq!(bm.move_to(77, OTHER, None, T0), Err(Error::NoSuchNode(77)));
}

#[test]
fn move_within_folder_index_semantics() {
    // Chromium BookmarkModel::Move: `index` is a slot in the list as it is before the node leaves it. A slot equal to the
    // node's own index or the one after it is a no-op; a later slot is one lower once the node is out. So in [a,b,c,d],
    // moving `a` to slot 2 ("before c") gives [b,a,c,d], and moving `d` to slot 0 gives [d,...].
    let mut bm = Bookmarks::new();
    let ids: Vec<Id> = ["a", "b", "c", "d"]
        .iter()
        .map(|t| add(&mut bm, MENU, t, &format!("https://{t}.example/")))
        .collect();
    let order = |bm: &Bookmarks| titles(bm, MENU).join("");
    bm.move_to(ids[0], MENU, Some(2), T0 + 1).unwrap();
    assert_eq!(order(&bm), "bacd");
    // `a` is at 1 now: slots 1 and 2 are no-ops, and leave modified alone.
    bm.move_to(ids[0], MENU, Some(1), T0 + 2).unwrap();
    bm.move_to(ids[0], MENU, Some(2), T0 + 2).unwrap();
    assert_eq!(order(&bm), "bacd");
    assert_eq!(bm.get(ids[0]).unwrap().modified_us, T0 + 1);
    bm.move_to(ids[3], MENU, Some(0), T0).unwrap();
    assert_eq!(order(&bm), "dbac");
    // End slot is len: `d` at 0 goes last.
    bm.move_to(ids[3], MENU, None, T0).unwrap();
    assert_eq!(order(&bm), "bacd");
    // Already last: slot len is old + 1, a no-op.
    bm.move_to(ids[3], MENU, Some(4), T0).unwrap();
    assert_eq!(order(&bm), "bacd");
    assert_eq!(bm.move_to(ids[3], MENU, Some(5), T0), Err(Error::IndexOutOfRange));
}

#[test]
fn move_into_own_subtree_is_cycle() {
    let mut bm = Bookmarks::new();
    let f1 = bm.add_folder(MENU, None, "f1", T0).unwrap();
    let f2 = bm.add_folder(f1, None, "f2", T0).unwrap();
    let f3 = bm.add_folder(f2, None, "f3", T0).unwrap();
    assert_eq!(bm.move_to(f1, f3, None, T0), Err(Error::Cycle));
    assert_eq!(bm.move_to(f1, f2, None, T0), Err(Error::Cycle));
    assert_eq!(bm.move_to(f1, f1, None, T0), Err(Error::Cycle));
    assert_eq!(bm.get(f1).unwrap().parent, Some(MENU));
    assert_eq!(bm.children(MENU).unwrap(), &[f1]);
    // A sibling move is fine.
    bm.move_to(f3, f1, None, T0).unwrap();
    assert_eq!(bm.children(f1).unwrap(), &[f2, f3]);
}

#[test]
fn rename_and_set_url_update_modified() {
    let mut bm = Bookmarks::new();
    let b = bm.add_bookmark(MENU, None, "old", "https://a.example/", 10).unwrap();
    bm.rename(b, "new", 20).unwrap();
    assert_eq!(bm.get(b).unwrap().title, "new");
    assert_eq!((bm.get(b).unwrap().added_us, bm.get(b).unwrap().modified_us), (10, 20));
    bm.set_url(b, "HTTPS://B.example", 30).unwrap();
    assert_eq!(bm.get(b).unwrap().kind, Kind::Bookmark { url: "https://b.example/".into() });
    assert_eq!(bm.get(b).unwrap().modified_us, 30);
    assert_eq!(bm.set_url(b, "", 40), Err(Error::EmptyUrl));
    assert_eq!(bm.get(b).unwrap().modified_us, 30);
    let f = bm.add_folder(MENU, None, "f", 50).unwrap();
    assert!(bm.set_url(f, "https://f.example/", 60).is_err());
    // Long titles are cut on a char boundary: 'é' is two bytes, 4096 is even, so cut at 4096 bytes exactly.
    bm.rename(b, &"é".repeat(3000), 70).unwrap();
    assert_eq!(bm.get(b).unwrap().title.len(), 4096);
    bm.rename(b, &format!("a{}", "é".repeat(3000)), 70).unwrap();
    assert_eq!(bm.get(b).unwrap().title.len(), 4095);
}

#[test]
fn tags_are_lowercased_sorted_deduped() {
    let mut bm = Bookmarks::new();
    let b = add(&mut bm, MENU, "b", "https://b.example/");
    bm.set_tags(b, &["Zed", " alpha ", "ALPHA", "", "Mid"], T0 + 1).unwrap();
    assert_eq!(bm.get(b).unwrap().tags, ["alpha", "mid", "zed"]);
    bm.set_keyword(b, Some("  Kw "), T0 + 2).unwrap();
    assert_eq!(bm.get(b).unwrap().keyword.as_deref(), Some("Kw"));
    bm.set_keyword(b, Some("  "), T0 + 3).unwrap();
    assert_eq!(bm.get(b).unwrap().keyword, None);
    let f = bm.add_folder(MENU, None, "f", T0).unwrap();
    assert!(matches!(bm.set_tags(f, &["x"], T0), Err(Error::Format(_))));
    assert!(matches!(bm.set_keyword(f, Some("x"), T0), Err(Error::Format(_))));
}

#[test]
fn find_by_url_normalises() {
    let mut bm = Bookmarks::new();
    let a = add(&mut bm, TOOLBAR, "a", "https://example.org/");
    let b = add(&mut bm, MENU, "b", "https://EXAMPLE.org");
    add(&mut bm, MENU, "c", "https://example.org/other");
    assert_eq!(bm.find_by_url("https://Example.org"), vec![a, b]);
    assert_eq!(bm.find_by_url("  https://example.org/  "), vec![a, b]);
    assert!(bm.is_bookmarked("https://example.org"));
    assert!(!bm.is_bookmarked("https://example.com/"));
    assert!(bm.find_by_url("not a url").is_empty());
    assert!(bm.find_by_url("").is_empty());
}

#[test]
fn is_bookmarked_false_for_folders() {
    let mut bm = Bookmarks::new();
    bm.add_folder(MENU, None, "https://example.org/", T0).unwrap();
    bm.add_separator(MENU, None, T0).unwrap();
    assert!(!bm.is_bookmarked("https://example.org/"));
    assert!(!bm.is_bookmarked("Bookmarks Menu"));
}

#[test]
fn search_words_all_must_match() {
    let mut bm = Bookmarks::new();
    let book = add(&mut bm, MENU, "Rust Book", "https://doc.rust-lang.org/book/");
    let forum = add(&mut bm, MENU, "Rust forum", "https://users.rust-lang.org/");
    add(&mut bm, MENU, "Cooking", "https://cook.example/");
    assert_eq!(bm.search("rust book", 10), vec![book]);
    assert_eq!(bm.search("BOOK RUST", 10), vec![book]);
    assert_eq!(bm.search("rust", 10), vec![book, forum]);
    assert_eq!(bm.search("rust cooking", 10), Vec::<Id>::new());
    // Words may match in different fields: title word plus URL word.
    assert_eq!(bm.search("forum users", 10), vec![forum]);
    bm.set_tags(forum, &["Community"], T0).unwrap();
    bm.set_keyword(forum, Some("rf"), T0).unwrap();
    assert_eq!(bm.search("community", 10), vec![forum]);
    assert_eq!(bm.search("rf", 10), vec![forum]);
    assert_eq!(bm.search("rust", 1), vec![book]);
    assert!(bm.search("rust", 0).is_empty());
}

#[test]
fn search_title_ranks_above_url() {
    let mut bm = Bookmarks::new();
    let url_only = add(&mut bm, MENU, "Docs", "https://example.org/rust");
    let title = add(&mut bm, MENU, "Rust", "https://other.example/");
    assert_eq!(bm.search("rust", 10), vec![title, url_only]);
    assert_eq!(bm.search("rust", 1), vec![title]);
}

#[test]
fn search_empty_query_is_empty() {
    let mut bm = Bookmarks::new();
    add(&mut bm, MENU, "a", "https://a.example/");
    assert!(bm.search("", 10).is_empty());
    assert!(bm.search("   \t ", 10).is_empty());
}

#[test]
fn dedupe_keeps_first_in_folder() {
    let mut bm = Bookmarks::new();
    let a = add(&mut bm, MENU, "a", "https://x.example");
    let b = add(&mut bm, MENU, "b", "https://y.example/");
    add(&mut bm, MENU, "c", "https://X.example/");
    let s = bm.add_separator(MENU, None, T0).unwrap();
    add(&mut bm, MENU, "d", "https://x.example/");
    let s2 = bm.add_separator(MENU, None, T0).unwrap();
    assert_eq!(bm.dedupe(MENU, false), 2);
    assert_eq!(bm.children(MENU).unwrap(), &[a, b, s, s2]);
    assert_eq!(bm.dedupe(MENU, false), 0);
    assert_eq!(bm.dedupe(99, true), 0);
}

#[test]
fn dedupe_recursive_only_when_asked() {
    let mut bm = Bookmarks::new();
    add(&mut bm, TOOLBAR, "a", "https://a.example/");
    let f = bm.add_folder(TOOLBAR, None, "f", T0).unwrap();
    add(&mut bm, f, "b1", "https://b.example/");
    add(&mut bm, f, "b2", "https://b.example/");
    assert_eq!(bm.dedupe(TOOLBAR, false), 0);
    assert_eq!(bm.children(f).unwrap().len(), 2);
    assert_eq!(bm.dedupe(TOOLBAR, true), 1);
    assert_eq!(titles(&bm, f), ["b1"]);
    assert_eq!(bm.children(ROOT).unwrap(), &[TOOLBAR, MENU, OTHER]);
}

#[test]
fn dedupe_ignores_other_folders() {
    let mut bm = Bookmarks::new();
    let a = add(&mut bm, TOOLBAR, "a", "https://same.example/");
    let b = add(&mut bm, MENU, "b", "https://same.example/");
    let f = bm.add_folder(MENU, None, "f", T0).unwrap();
    let c = add(&mut bm, f, "c", "https://same.example/");
    assert_eq!(bm.dedupe(TOOLBAR, true), 0);
    assert_eq!(bm.dedupe(MENU, false), 0);
    assert_eq!(bm.dedupe(ROOT, true), 0);
    assert!(bm.get(a).is_some() && bm.get(b).is_some() && bm.get(c).is_some());
}

fn rich() -> Bookmarks {
    let mut bm = Bookmarks::new();
    let b = bm.add_bookmark(TOOLBAR, None, "Ex \"ample\" é", "https://example.org/?a=1&b=2", T0).unwrap();
    bm.set_tags(b, &["a", "B"], T0 + 1).unwrap();
    bm.set_keyword(b, Some("ex"), T0 + 2).unwrap();
    let f = bm.add_folder(MENU, None, "Folder <1>", T0 + 3).unwrap();
    bm.add_separator(f, None, T0 + 4).unwrap();
    bm.add_bookmark(f, None, "", "https://empty-title.example/", T0 + 5).unwrap();
    bm.add_bookmark(OTHER, None, "bm", "javascript:void(0)", T0 + 6).unwrap();
    bm
}

#[test]
fn json_roundtrip_keeps_everything() {
    let mut bm = rich();
    let gone = bm.add_folder(MENU, None, "gone", T0).unwrap();
    bm.remove(gone).unwrap();
    let text = bm.to_json();
    let back = Bookmarks::from_json(&text).unwrap();
    assert_eq!(back.walk(), bm.walk());
    assert_eq!(back.to_json(), text);
    // next_id survives, so a removed id is not handed out again.
    let mut back = back;
    let fresh = back.add_folder(MENU, None, "n", T0).unwrap();
    assert!(fresh > gone);
}

#[test]
fn json_is_deterministic() {
    let mut bm = Bookmarks::new();
    let b = bm.add_bookmark(TOOLBAR, None, "Example", "https://example.org/", T0).unwrap();
    bm.set_tags(b, &["a"], T0).unwrap();
    bm.set_keyword(b, Some("ex"), T0).unwrap();
    let want = concat!(
        r#"{"version":1,"next_id":5,"nodes":["#,
        r#"{"id":1,"title":"Bookmarks Toolbar","type":"folder","added":0,"modified":1730000000000000,"children":["#,
        r#"{"id":4,"type":"bookmark","title":"Example","url":"https://example.org/","added":1730000000000000,"modified":1730000000000000,"tags":["a"],"keyword":"ex"}]},"#,
        r#"{"id":2,"title":"Bookmarks Menu","type":"folder","added":0,"modified":0,"children":[]},"#,
        r#"{"id":3,"title":"Other Bookmarks","type":"folder","added":0,"modified":0,"children":[]}]}"#
    );
    assert_eq!(bm.to_json(), want);
    assert_eq!(bm.to_json(), bm.to_json());
    assert_eq!(rich().to_json(), rich().to_json());
}

fn doc(version: u32, next_id: u64, toolbar: &str, menu: &str) -> String {
    format!(
        r#"{{"version":{version},"next_id":{next_id},"nodes":[
{{"id":1,"title":"t","type":"folder","children":[{toolbar}]}},
{{"id":2,"title":"m","type":"folder","children":[{menu}]}},
{{"id":3,"title":"o","type":"folder","children":[]}}]}}"#
    )
}

fn jb(id: u64, url: &str) -> String {
    format!(r#"{{"id":{id},"type":"bookmark","title":"b{id}","url":"{url}"}}"#)
}

#[test]
fn json_rejects_duplicate_ids() {
    let ok = doc(1, 10, &jb(4, "https://a.example/"), &jb(5, "https://b.example/"));
    assert!(Bookmarks::from_json(&ok).is_ok());
    let dup = doc(1, 10, &jb(4, "https://a.example/"), &jb(4, "https://b.example/"));
    assert!(matches!(Bookmarks::from_json(&dup), Err(Error::Format(_))));
    let builtin_clash = doc(1, 10, &jb(2, "https://a.example/"), "");
    assert!(matches!(Bookmarks::from_json(&builtin_clash), Err(Error::Format(_))));
    let zero = doc(1, 10, &jb(0, "https://a.example/"), "");
    assert!(matches!(Bookmarks::from_json(&zero), Err(Error::Format(_))));
    // next_id must be above every id.
    let low = doc(1, 5, &jb(5, "https://a.example/"), "");
    assert!(matches!(Bookmarks::from_json(&low), Err(Error::Format(_))));
    let exact = doc(1, 6, &jb(5, "https://a.example/"), "");
    assert!(Bookmarks::from_json(&exact).is_ok());
    // Bookmarks need a usable URL.
    let no_url = doc(1, 10, r#"{"id":4,"type":"bookmark","title":"x"}"#, "");
    assert!(matches!(Bookmarks::from_json(&no_url), Err(Error::Format(_))));
    let empty = doc(1, 10, &jb(4, ""), "");
    assert!(matches!(Bookmarks::from_json(&empty), Err(Error::Format(_))));
    assert!(matches!(Bookmarks::from_json("not json"), Err(Error::Format(_))));
}

#[test]
fn json_rejects_missing_builtin() {
    let two = r#"{"version":1,"next_id":4,"nodes":[
{"id":1,"title":"t","type":"folder"},{"id":2,"title":"m","type":"folder"}]}"#;
    assert!(matches!(Bookmarks::from_json(two), Err(Error::Format(_))));
    let wrong_order = r#"{"version":1,"next_id":4,"nodes":[
{"id":2,"title":"m","type":"folder"},{"id":1,"title":"t","type":"folder"},{"id":3,"title":"o","type":"folder"}]}"#;
    assert!(matches!(Bookmarks::from_json(wrong_order), Err(Error::Format(_))));
    let not_folder = r#"{"version":1,"next_id":4,"nodes":[
{"id":1,"title":"t","type":"folder"},{"id":2,"title":"m","type":"separator"},{"id":3,"title":"o","type":"folder"}]}"#;
    assert!(matches!(Bookmarks::from_json(not_folder), Err(Error::Format(_))));
}

#[test]
fn json_rejects_future_version() {
    assert!(matches!(Bookmarks::from_json(&doc(2, 10, "", "")), Err(Error::Format(_))));
    assert!(matches!(Bookmarks::from_json(&doc(0, 10, "", "")), Err(Error::Format(_))));
    assert!(Bookmarks::from_json(&doc(1, 4, "", "")).is_ok());
}

#[test]
fn json_ignores_unknown_fields() {
    let text = r#"{"version":1,"next_id":6,"future":{"a":1},"nodes":[
{"id":1,"title":"t","type":"folder","color":"red","children":[
  {"id":4,"type":"bookmark","title":"x","url":"https://a.example/","favicon":"data:","tags":["B","a"],"extra":[1,2]},
  {"id":5,"type":"separator","note":"hi"}]},
{"id":2,"title":"m","type":"folder"},{"id":3,"title":"o","type":"folder"}]}"#;
    let bm = Bookmarks::from_json(text).unwrap();
    assert_eq!(bm.children(TOOLBAR).unwrap(), &[4, 5]);
    assert_eq!(bm.get(4).unwrap().tags, ["a", "b"]);
    assert_eq!(bm.len_bookmarks(), 1);
}

const NETSCAPE: &str = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<!-- This is an automatically generated file. -->
<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=UTF-8">
<TITLE>Bookmarks</TITLE>
<H1>Bookmarks Menu</H1>

<DL><p>
    <DT><H3 ADD_DATE="1700000000" LAST_MODIFIED="1700000100" PERSONAL_TOOLBAR_FOLDER="true">Toolbar Stuff</H3>
    <DL><p>
        <DT><A HREF="https://example.org/a" ADD_DATE="1700000001">Alpha</A>
        <HR>
        <DT><A HREF="https://example.org/b">Beta</A>
    </DL><p>
    <DT><A HREF="https://example.net/">Gamma</A>
    <DD>a description that is ignored
</DL>
"#;

#[test]
fn netscape_import_basic_folders_links_separator() {
    let mut bm = Bookmarks::new();
    let r = bm.import_netscape_html(NETSCAPE, MENU, ImportOptions::default(), T0).unwrap();
    assert_eq!(
        r,
        ImportReport { bookmarks: 3, folders: 1, separators: 1, skipped_duplicates: 0, skipped_invalid: 0 }
    );
    assert_eq!(titles(&bm, MENU), ["Toolbar Stuff", "Gamma"]);
    let folder = bm.children(MENU).unwrap()[0];
    assert_eq!(titles(&bm, folder), ["Alpha", "", "Beta"]);
    let kids = bm.children(folder).unwrap();
    assert_eq!(bm.get(kids[1]).unwrap().kind, Kind::Separator);
    assert_eq!(bm.get(kids[2]).unwrap().kind, Kind::Bookmark { url: "https://example.org/b".into() });
    // The toolbar flag is read and ignored: nothing went into TOOLBAR.
    assert!(bm.children(TOOLBAR).unwrap().is_empty());
    assert_eq!(bm.len_bookmarks(), 3);
}

#[test]
fn netscape_import_tolerates_missing_closing_tags_and_uppercase() {
    let html = "<dl>\n<dt><h3 add_date='5'>Dir\n<dl>\n<dt><a href=https://one.example/ tags=x>One\n<dt><A HREF='https://two.example/'><b>Two</b> two\n<hr/>\n</dl>\n<DT><A\n HREF=\"https://three.example/\"\n>Three</a>\n<script>var x = '<a href=\"https://nope.example/\">no</a>';</script></dl>";
    let mut bm = Bookmarks::new();
    let r = bm.import_netscape_html(html, OTHER, ImportOptions::default(), T0).unwrap();
    assert_eq!((r.bookmarks, r.folders, r.separators), (3, 1, 1));
    assert_eq!(titles(&bm, OTHER), ["Dir", "Three"]);
    let dir = bm.children(OTHER).unwrap()[0];
    assert_eq!(titles(&bm, dir), ["One", "Two two", ""]);
    let one = bm.children(dir).unwrap()[0];
    assert_eq!(bm.get(one).unwrap().tags, ["x"]);
    assert_eq!(bm.get(dir).unwrap().added_us, 5_000_000);
}

#[test]
fn netscape_import_decodes_entities() {
    let html = r#"<DL><DT><A HREF="https://example.org/?a=1&amp;b=2" TAGS="t&amp;u">Tom &amp; Jerry &lt;3 &quot;q&quot; &#39;s&#39; &#x27;t&#x27; &#65; &bogus; &#xZZ; AT&T</A></DL>"#;
    let mut bm = Bookmarks::new();
    bm.import_netscape_html(html, MENU, ImportOptions::default(), T0).unwrap();
    let id = bm.children(MENU).unwrap()[0];
    let n = bm.get(id).unwrap();
    assert_eq!(n.kind, Kind::Bookmark { url: "https://example.org/?a=1&b=2".into() });
    assert_eq!(n.title, "Tom & Jerry <3 \"q\" 's' 't' A &bogus; &#xZZ; AT&T");
    assert_eq!(n.tags, ["t&u"]);
}

#[test]
fn netscape_import_dates_are_seconds() {
    let html = r#"<DL><DT><A HREF="https://a.example/" ADD_DATE="1700000000" LAST_MODIFIED="1700000100">a</A>
<DT><A HREF="https://b.example/" LAST_MODIFIED="1700000200">b</A>
<DT><A HREF="https://c.example/" ADD_DATE="1700000300">c</A></DL>"#;
    let mut bm = Bookmarks::new();
    bm.import_netscape_html(html, MENU, ImportOptions::default(), T0).unwrap();
    let k = bm.children(MENU).unwrap().to_vec();
    let d = |i: usize| (bm.get(k[i]).unwrap().added_us, bm.get(k[i]).unwrap().modified_us);
    assert_eq!(d(0), (1_700_000_000_000_000, 1_700_000_100_000_000));
    // Only LAST_MODIFIED: Firefox uses it for dateAdded too. Only ADD_DATE: modified follows added.
    assert_eq!(d(1), (1_700_000_200_000_000, 1_700_000_200_000_000));
    assert_eq!(d(2), (1_700_000_300_000_000, 1_700_000_300_000_000));
}

#[test]
fn netscape_import_bad_date_falls_back_to_now() {
    let html = r#"<DL><DT><A HREF="https://a.example/" ADD_DATE="yesterday" LAST_MODIFIED="">a</A>
<DT><A HREF="https://b.example/">b</A>
<DT><A HREF="https://c.example/" ADD_DATE="12abc">c</A></DL>"#;
    let mut bm = Bookmarks::new();
    bm.import_netscape_html(html, MENU, ImportOptions::default(), 42).unwrap();
    let k = bm.children(MENU).unwrap().to_vec();
    assert_eq!((bm.get(k[0]).unwrap().added_us, bm.get(k[0]).unwrap().modified_us), (42, 42));
    assert_eq!((bm.get(k[1]).unwrap().added_us, bm.get(k[1]).unwrap().modified_us), (42, 42));
    // parseInt reads the leading digits, as Firefox does.
    assert_eq!(bm.get(k[2]).unwrap().added_us, 12_000_000);
}

#[test]
fn netscape_import_tags_and_shortcuturl() {
    let html = r#"<DL><DT><A HREF="https://a.example/" TAGS="Foo, bar,,FOO" SHORTCUTURL="ex">a</A>
<DT><A HREF="https://b.example/" TAGS=",," SHORTCUTURL="  ">b</A></DL>"#;
    let mut bm = Bookmarks::new();
    bm.import_netscape_html(html, MENU, ImportOptions::default(), T0).unwrap();
    let k = bm.children(MENU).unwrap().to_vec();
    assert_eq!(bm.get(k[0]).unwrap().tags, ["bar", "foo"]);
    assert_eq!(bm.get(k[0]).unwrap().keyword.as_deref(), Some("ex"));
    assert!(bm.get(k[1]).unwrap().tags.is_empty());
    assert_eq!(bm.get(k[1]).unwrap().keyword, None);
}

#[test]
fn netscape_import_into_root_is_protected() {
    let mut bm = Bookmarks::new();
    assert_eq!(
        bm.import_netscape_html(NETSCAPE, ROOT, ImportOptions::default(), T0),
        Err(Error::ProtectedNode(ROOT))
    );
    assert_eq!(bm.import_netscape_html(NETSCAPE, 99, ImportOptions::default(), T0), Err(Error::NoSuchNode(99)));
    let b = add(&mut bm, MENU, "b", "https://b.example/");
    assert_eq!(bm.import_netscape_html(NETSCAPE, b, ImportOptions::default(), T0), Err(Error::NotAFolder(b)));
    assert_eq!(bm.len_bookmarks(), 1);
}

#[test]
fn netscape_import_skip_duplicates_counts() {
    let html = r#"<DL><DT><A HREF="https://dup.example">a</A><DT><A HREF="https://dup.example/">a2</A>
<DT><A HREF="https://new.example/">n</A><DT><A HREF="javascript:">bad? no, valid</A><DT><A HREF="nonsense">bad</A><DT><A NAME="anchor">x</A></DL>"#;
    let mut bm = Bookmarks::new();
    add(&mut bm, MENU, "existing", "https://dup.example/");
    let opts = ImportOptions { skip_duplicates: true };
    let r = bm.import_netscape_html(html, MENU, opts, T0).unwrap();
    assert_eq!(
        r,
        ImportReport { bookmarks: 2, folders: 0, separators: 0, skipped_duplicates: 2, skipped_invalid: 1 }
    );
    assert_eq!(titles(&bm, MENU), ["existing", "n", "bad? no, valid"]);
    // Same file into another folder: not a duplicate there, and a second pass duplicates nothing new.
    let r = bm.import_netscape_html(html, OTHER, opts, T0).unwrap();
    assert_eq!((r.bookmarks, r.skipped_duplicates), (3, 1));
    let r = bm.import_netscape_html(html, OTHER, ImportOptions::default(), T0).unwrap();
    assert_eq!((r.bookmarks, r.skipped_duplicates), (4, 0));
}

#[test]
fn netscape_export_matches_firefox_shape() {
    let mut bm = Bookmarks::new();
    let b = bm
        .add_bookmark(MENU, None, "Tom & \"Jerry\"", "https://example.org/?a=1&b=2", 1_700_000_000_500_000)
        .unwrap();
    bm.set_tags(b, &["B", "a"], 1_700_000_000_500_000).unwrap();
    bm.set_keyword(b, Some("ex"), 1_700_000_000_500_000).unwrap();
    bm.add_bookmark(TOOLBAR, None, "T", "https://t.example/", 0).unwrap();
    // Firefox lines: BookmarkHTMLUtils.sys.mjs @33abb90 _writeHeader, _writeContainer, _writeItem, _writeDateAttributes.
    let want = "<!DOCTYPE NETSCAPE-Bookmark-file-1>
<!-- This is an automatically generated file.
     It will be read and overwritten.
     DO NOT EDIT! -->
<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">
<meta http-equiv=\"Content-Security-Policy\"
      content=\"default-src 'self'; script-src 'none'; img-src data: *; object-src 'none'\"></meta>
<TITLE>Bookmarks</TITLE>
<H1>Bookmarks Menu</H1>

<DL><p>
    <DT><A HREF=\"https://example.org/?a=1&amp;b=2\" ADD_DATE=\"1700000000\" LAST_MODIFIED=\"1700000000\" SHORTCUTURL=\"ex\" TAGS=\"a,b\">Tom &amp; &quot;Jerry&quot;</A>
    <DT><H3 PERSONAL_TOOLBAR_FOLDER=\"true\">Bookmarks Toolbar</H3>
    <DL><p>
        <DT><A HREF=\"https://t.example/\">T</A>
    </DL><p>
</DL>
";
    assert_eq!(bm.export_netscape_html(), want);
    // Other Bookmarks appears, flagged, once it has content; empty roots are left out.
    bm.add_bookmark(OTHER, None, "O", "https://o.example/", 0).unwrap();
    let out = bm.export_netscape_html();
    assert!(out.contains("    <DT><H3 UNFILED_BOOKMARKS_FOLDER=\"true\">Other Bookmarks</H3>\n"));
    assert!(out.ends_with("    </DL><p>\n</DL>\n"));
    let empty = Bookmarks::new().export_netscape_html();
    assert!(empty.ends_with("<H1>Bookmarks Menu</H1>\n\n<DL><p>\n</DL>\n"));
}

#[test]
fn netscape_roundtrip_through_export() {
    let mut bm = rich();
    let f = bm.children(MENU).unwrap()[0];
    let inner = bm.add_folder(f, None, "in & out", T0 + 9).unwrap();
    let b = bm.add_bookmark(inner, None, "deep 'q'", "https://deep.example/a?b=\"c\"", T0 + 10).unwrap();
    bm.set_tags(b, &["x"], T0 + 10).unwrap();
    let html = bm.export_netscape_html();
    let mut back = Bookmarks::new();
    let r = back.import_netscape_html(&html, MENU, ImportOptions::default(), 7).unwrap();
    assert_eq!(r.skipped_invalid, 0);
    // Seconds only: compare times floored to the second.
    let secs = |v: Vec<Shape>| -> Vec<Shape> {
        v.into_iter()
            // Separators carry no dates in the file (Firefox never reads them for separators either).
            .map(|(d, k, t, g, w, a, m)| match k.as_str() {
                "s" => (d, k, t, g, w, 0, 0),
                _ => (d, k, t, g, w, a.div_euclid(1_000_000), m.div_euclid(1_000_000)),
            })
            .collect()
    };
    let menu_before = secs(shape(&bm, MENU));
    let imported = secs(shape(&back, MENU));
    // Menu children come first, unchanged; Toolbar and Other follow as ordinary folders.
    assert_eq!(&imported[..menu_before.len()], &menu_before[..]);
    let rest = &imported[menu_before.len()..];
    assert_eq!(rest[0].2, "Bookmarks Toolbar");
    assert_eq!(rest[1].1, "b:https://example.org/?a=1&b=2");
    assert_eq!((rest[1].3.clone(), rest[1].4.clone()), (vec!["a".to_string(), "b".to_string()], Some("ex".into())));
    assert_eq!(rest[2].2, "Other Bookmarks");
    assert_eq!(rest[3].1, "b:javascript:void(0)");
    assert_eq!(back.len_bookmarks(), bm.len_bookmarks());
}

#[test]
fn netscape_tokenizer_never_panics() {
    let nasty = [
        "", "<", "<a", "<a href", "<a href=", "<a href=\"", "<!--", "<!-- x", "<!", "</", "</>", "<<<>>>", "&", "&#", "&#x;", "&#99999999999;",
        "é<a href='é'>é</a>é", "<dl><dl><dl></dl></dl></dl></dl></dl>", "<h3>x", "<script>", "<style>x</style>",
        "<a href=\"https://a.example/\">\u{1F600}\u{1F600}",
    ];
    for s in nasty {
        let mut bm = Bookmarks::new();
        let _ = bm.import_netscape_html(s, MENU, ImportOptions::default(), 0);
    }
    // Deep nesting is flattened, not turned into a deep tree.
    let deep = "<dl><dt><h3>f</h3>".repeat(500) + "<dt><a href='https://a.example/'>a</a>";
    let mut bm = Bookmarks::new();
    bm.import_netscape_html(&deep, MENU, ImportOptions::default(), 0).unwrap();
    assert_eq!(bm.len_bookmarks(), 1);
    assert!(Bookmarks::from_json(&bm.to_json()).is_ok());
}

const PLACES: &str = r#"{"guid":"root________","title":"","index":0,"dateAdded":1700000000000000,"lastModified":1700000000000000,"typeCode":2,"type":"text/x-moz-place-container","root":"placesRoot","children":[
 {"guid":"menu________","title":"menu","index":0,"typeCode":2,"type":"text/x-moz-place-container","root":"bookmarksMenuFolder","children":[
   {"guid":"aaaaaaaaaaaa","title":"Second","index":1,"dateAdded":1700000002000000,"lastModified":1700000003000000,"typeCode":1,"type":"text/x-moz-place","uri":"https://example.org/two","tags":"Foo, bar ,,FOO","keyword":"two"},
   {"guid":"bbbbbbbbbbbb","title":"First","index":0,"dateAdded":1700000001000000,"lastModified":1700000001000000,"typeCode":1,"type":"text/x-moz-place","uri":"https://example.org/one"},
   {"guid":"cccccccccccc","title":"","index":2,"typeCode":3,"type":"text/x-moz-place-separator"}]},
 {"guid":"toolbar_____","title":"toolbar","index":1,"typeCode":2,"type":"text/x-moz-place-container","root":"toolbarFolder","children":[
   {"guid":"dddddddddddd","title":"Dir","index":0,"typeCode":2,"type":"text/x-moz-place-container","children":[
      {"title":"Deep","index":0,"typeCode":1,"type":"text/x-moz-place","uri":"https://example.net/deep"}]}]},
 {"guid":"unfiled_____","title":"unfiled","index":2,"typeCode":2,"type":"text/x-moz-place-container","root":"unfiledBookmarksFolder","children":[]},
 {"guid":"mobile______","title":"mobile","index":3,"typeCode":2,"type":"text/x-moz-place-container","root":"mobileFolder","children":[
   {"title":"Phone","index":0,"typeCode":1,"type":"text/x-moz-place","uri":"https://m.example.org/"}]},
 {"guid":"tags________","title":"","index":4,"typeCode":2,"type":"text/x-moz-place-container","root":"tagsFolder","children":[
   {"title":"foo","index":0,"typeCode":2,"type":"text/x-moz-place-container","children":[]}]}]}"#;

#[test]
fn places_json_import_maps_roots() {
    let mut bm = Bookmarks::new();
    let r = bm.import_places_json(PLACES, ImportOptions::default(), T0).unwrap();
    assert_eq!(
        r,
        ImportReport { bookmarks: 4, folders: 2, separators: 1, skipped_duplicates: 0, skipped_invalid: 0 }
    );
    // Menu children are sorted by `index` (Firefox sorts before inserting).
    assert_eq!(titles(&bm, MENU), ["First", "Second", ""]);
    assert_eq!(titles(&bm, TOOLBAR), ["Dir"]);
    let dir = bm.children(TOOLBAR).unwrap()[0];
    assert_eq!(titles(&bm, dir), ["Deep"]);
    assert_eq!(titles(&bm, OTHER), ["Mobile Bookmarks"]);
    let mobile = bm.children(OTHER).unwrap()[0];
    assert_eq!(titles(&bm, mobile), ["Phone"]);
    // The wrapper and the tags root are not imported as folders.
    assert_eq!(bm.children(ROOT).unwrap(), &[TOOLBAR, MENU, OTHER]);
    let mut count = 0;
    for (_, n) in bm.walk() {
        if matches!(n.kind, Kind::Folder) {
            count += 1;
        }
    }
    assert_eq!(count, 3 + 2);
}

#[test]
fn places_json_import_counts_invalid_and_place_uris() {
    let json = r#"{"root":"placesRoot","children":[{"root":"unfiledBookmarksFolder","children":[
 {"type":"text/x-moz-place","title":"q","uri":"place:sort=8&maxResults=10"},
 {"type":"text/x-moz-place","title":"Q","uri":"PLACE:type=6"},
 {"type":"text/x-moz-place-bogus","title":"?"},
 {"type":"text/x-moz-place","title":"junk","uri":"not a url"},
 {"type":"text/x-moz-place","title":"no uri"},
 {"title":"no type"},
 7,
 {"type":"text/x-moz-place","title":"bookmarklet","uri":"javascript:alert(1)"},
 {"type":"text/x-moz-place","title":"inline","uri":"data:text/html,hi"}]}]}"#;
    let mut bm = Bookmarks::new();
    let r = bm.import_places_json(json, ImportOptions::default(), T0).unwrap();
    assert_eq!((r.bookmarks, r.skipped_invalid), (2, 7));
    assert_eq!(titles(&bm, OTHER), ["bookmarklet", "inline"]);
    assert!(matches!(bm.import_places_json("[]", ImportOptions::default(), T0), Err(Error::Format(_))));
    assert!(matches!(bm.import_places_json("{", ImportOptions::default(), T0), Err(Error::Format(_))));
}

#[test]
fn places_json_tags_split_and_lowercased() {
    let mut bm = Bookmarks::new();
    bm.import_places_json(PLACES, ImportOptions::default(), T0).unwrap();
    let second = bm.children(MENU).unwrap()[1];
    assert_eq!(bm.get(second).unwrap().tags, ["bar", "foo"]);
    assert_eq!(bm.get(second).unwrap().keyword.as_deref(), Some("two"));
    let first = bm.children(MENU).unwrap()[0];
    assert!(bm.get(first).unwrap().tags.is_empty());
    let long = format!(r#"{{"root":"toolbarFolder","children":[{{"type":"text/x-moz-place","uri":"https://a.example/","tags":"keep,{}"}}]}}"#, "x".repeat(101));
    bm.import_places_json(&long, ImportOptions::default(), T0).unwrap();
    let last = *bm.children(TOOLBAR).unwrap().last().unwrap();
    assert_eq!(bm.get(last).unwrap().tags, ["keep"]);
}

#[test]
fn places_json_dates_are_microseconds() {
    let mut bm = Bookmarks::new();
    bm.import_places_json(PLACES, ImportOptions::default(), 99).unwrap();
    let k = bm.children(MENU).unwrap().to_vec();
    let d = |i: usize| (bm.get(k[i]).unwrap().added_us, bm.get(k[i]).unwrap().modified_us);
    assert_eq!(d(0), (1_700_000_001_000_000, 1_700_000_001_000_000));
    assert_eq!(d(1), (1_700_000_002_000_000, 1_700_000_003_000_000));
    // No dates in the file: now.
    assert_eq!(d(2), (99, 99));
    // lastModified before dateAdded is raised to dateAdded.
    let odd = r#"{"root":"toolbarFolder","children":[{"type":"text/x-moz-place","uri":"https://a.example/","dateAdded":500,"lastModified":100}]}"#;
    bm.import_places_json(odd, ImportOptions::default(), 99).unwrap();
    let id = bm.children(TOOLBAR).unwrap()[1];
    assert_eq!((bm.get(id).unwrap().added_us, bm.get(id).unwrap().modified_us), (500, 500));
}

#[test]
fn places_json_export_then_import_roundtrip() {
    let mut bm = rich();
    bm.add_bookmark(OTHER, None, "\u{1F600} uni", "https://uni.example/\u{e9}", T0 + 11).unwrap();
    let json = bm.export_places_json();
    assert!(json.contains("\"root\":\"placesRoot\"") && json.contains("\"root\":\"toolbarFolder\""));
    assert_eq!(json, bm.export_places_json());
    let mut back = Bookmarks::new();
    let r = back.import_places_json(&json, ImportOptions::default(), 5).unwrap();
    assert_eq!(r.skipped_invalid, 0);
    assert_eq!((r.bookmarks, r.separators), (bm.len_bookmarks(), 1));
    for root in [TOOLBAR, MENU, OTHER] {
        assert_eq!(shape(&back, root), shape(&bm, root), "root {root}");
    }
    // Every guid has 12 characters and they are unique.
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    fn guids(v: &serde_json::Value, out: &mut Vec<String>) {
        if let Some(g) = v.get("guid").and_then(|g| g.as_str()) {
            out.push(g.to_string());
        }
        for c in v.get("children").and_then(|c| c.as_array()).into_iter().flatten() {
            guids(c, out);
        }
    }
    let mut all = Vec::new();
    guids(&value, &mut all);
    assert!(all.iter().all(|g| g.len() == 12));
    let mut dedup = all.clone();
    dedup.sort();
    dedup.dedup();
    assert_eq!(dedup.len(), all.len());
    assert_eq!(value["children"][0]["guid"], "menu________");
}

#[test]
fn places_json_import_never_replaces_existing() {
    let mut bm = Bookmarks::new();
    let mine = add(&mut bm, MENU, "mine", "https://example.org/one");
    let other = add(&mut bm, TOOLBAR, "toolbar mine", "https://tb.example/");
    bm.import_places_json(PLACES, ImportOptions::default(), T0).unwrap();
    assert_eq!(bm.children(MENU).unwrap()[0], mine);
    assert_eq!(titles(&bm, MENU), ["mine", "First", "Second", ""]);
    assert_eq!(bm.children(TOOLBAR).unwrap()[0], other);
    // Without skip_duplicates a repeated import adds again; with it, the same URLs in the same folder are skipped.
    let r = bm.import_places_json(PLACES, ImportOptions { skip_duplicates: true }, T0).unwrap();
    assert_eq!(r.bookmarks, 1 + 1);
    assert_eq!(r.skipped_duplicates, 2);
    assert_eq!(bm.len_bookmarks(), 1 + 1 + 4 + 2);
}
