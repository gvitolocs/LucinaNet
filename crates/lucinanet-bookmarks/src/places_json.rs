/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Firefox's places JSON backup. Shapes from `toolkit/components/places/BookmarkJSONUtils.sys.mjs` @33abb90
//! (`rootToFolderGuidMap`, tag splitting, children sorted by `index`) and `PlacesUtils.sys.mjs` @33abb90 (`TYPE_X_MOZ_PLACE*`).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::format::js;
use crate::netscape::resolve_dates;
use crate::store::{Bookmarks, normalize_keyword, normalize_tags, normalize_url};
use crate::{Error, Id, ImportOptions, ImportReport, Kind, MENU, OTHER, ROOT, TOOLBAR};

const CONTAINER: &str = "text/x-moz-place-container";
const PLACE: &str = "text/x-moz-place";
const SEPARATOR: &str = "text/x-moz-place-separator";
const MOBILE_TITLE: &str = "Mobile Bookmarks";

struct Ctx {
    opts: ImportOptions,
    now_us: i64,
    report: ImportReport,
    known: HashMap<Id, HashSet<String>>,
}

fn micros(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    v.as_i64().or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
}

fn children_of(v: &Value) -> Vec<&Value> {
    let mut kids: Vec<&Value> = v.get("children").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    // Firefox sorts children by `index` first; only trust it when every child carries one.
    if kids.iter().all(|k| k.get("index").and_then(Value::as_i64).is_some()) {
        kids.sort_by_key(|k| k.get("index").and_then(Value::as_i64).unwrap_or(0));
    }
    kids
}

fn import_children(bm: &mut Bookmarks, ctx: &mut Ctx, node: &Value, dest: Id) -> Result<(), Error> {
    for child in children_of(node) {
        import_item(bm, ctx, child, dest)?;
    }
    Ok(())
}

fn import_item(bm: &mut Bookmarks, ctx: &mut Ctx, v: &Value, dest: Id) -> Result<(), Error> {
    let Some(ty) = v.get("type").and_then(Value::as_str) else {
        ctx.report.skipped_invalid += 1;
        return Ok(());
    };
    let title = v.get("title").and_then(Value::as_str).unwrap_or("");
    let dates = resolve_dates(micros(v.get("dateAdded")), micros(v.get("lastModified")), ctx.now_us);
    match ty {
        CONTAINER => {
            let id = bm.insert_raw(dest, None, Kind::Folder, title, dates.0, dates.1, Vec::new(), None)?;
            ctx.report.folders += 1;
            import_children(bm, ctx, v, id)?;
        }
        SEPARATOR => {
            bm.insert_raw(dest, None, Kind::Separator, "", dates.0, dates.1, Vec::new(), None)?;
            ctx.report.separators += 1;
        }
        PLACE => {
            let uri = v.get("uri").and_then(Value::as_str).unwrap_or("");
            let is_place_uri = uri.trim_start().get(..6).is_some_and(|p| p.eq_ignore_ascii_case("place:"));
            // `javascript:` and `data:` pass: they are bookmarklets.
            let Some(url) = normalize_url(uri).ok().filter(|_| !is_place_uri) else {
                ctx.report.skipped_invalid += 1;
                return Ok(());
            };
            if ctx.opts.skip_duplicates {
                let known = &*bm;
                let set = ctx.known.entry(dest).or_insert_with(|| {
                    known
                        .children(dest)
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|c| match known.get(*c).map(|n| &n.kind) {
                            Some(Kind::Bookmark { url }) => Some(url.clone()),
                            _ => None,
                        })
                        .collect()
                });
                if !set.insert(url.clone()) {
                    ctx.report.skipped_duplicates += 1;
                    return Ok(());
                }
            }
            let tags = normalize_tags(v.get("tags").and_then(Value::as_str).unwrap_or("").split(','));
            let keyword = normalize_keyword(v.get("keyword").and_then(Value::as_str));
            bm.insert_raw(dest, None, Kind::Bookmark { url }, title, dates.0, dates.1, tags, keyword)?;
            ctx.report.bookmarks += 1;
        }
        _ => ctx.report.skipped_invalid += 1,
    }
    Ok(())
}

fn import_top(bm: &mut Bookmarks, ctx: &mut Ctx, node: &Value) -> Result<(), Error> {
    match node.get("root").and_then(Value::as_str) {
        Some("placesRoot") => {
            for child in children_of(node) {
                import_top(bm, ctx, child)?;
            }
        }
        Some("toolbarFolder") => import_children(bm, ctx, node, TOOLBAR)?,
        Some("bookmarksMenuFolder") => import_children(bm, ctx, node, MENU)?,
        Some("unfiledBookmarksFolder") => import_children(bm, ctx, node, OTHER)?,
        Some("mobileFolder") => {
            // Firefox backups always list the mobile root; an empty one is not worth a folder.
            if !children_of(node).is_empty() {
                let id = bm.insert_raw(OTHER, None, Kind::Folder, MOBILE_TITLE, ctx.now_us, ctx.now_us, Vec::new(), None)?;
                ctx.report.folders += 1;
                import_children(bm, ctx, node, id)?;
            }
        }
        // The tags root only mirrors tags that are already on the bookmarks.
        Some("tagsFolder") => {}
        _ => import_item(bm, ctx, node, OTHER)?,
    }
    Ok(())
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// The 12-character guid of a node: Firefox's own for ROOT and the built-in folders, otherwise the node id as 12 base-64
/// digits (alphabet of Firefox guids, 72 bits, so distinct ids give distinct guids).
fn guid(id: Id) -> String {
    match id {
        ROOT => "root________".into(),
        MENU => "menu________".into(),
        TOOLBAR => "toolbar_____".into(),
        OTHER => "unfiled_____".into(),
        _ => (0..12)
            .rev()
            .map(|i| {
                let shift = 6 * i;
                let digit = if shift >= 64 { 0 } else { ((id >> shift) & 63) as usize };
                B64.get(digit).copied().map_or('A', char::from)
            })
            .collect(),
    }
}

impl Bookmarks {
    /// Append a Firefox places JSON backup: `toolbarFolder`, `bookmarksMenuFolder` and `unfiledBookmarksFolder` children go into
    /// [`TOOLBAR`], [`MENU`] and [`OTHER`]; `mobileFolder` children into a new `Mobile Bookmarks` folder under [`OTHER`]. Existing
    /// bookmarks are never replaced. Unknown types, `place:` URIs and unparsable URLs are counted in `skipped_invalid`.
    pub fn import_places_json(&mut self, json: &str, opts: ImportOptions, now_us: i64) -> Result<ImportReport, Error> {
        let v: Value = serde_json::from_str(json).map_err(|e| Error::Format(e.to_string()))?;
        if !v.is_object() {
            return Err(Error::Format("places backup must be a JSON object".into()));
        }
        let mut ctx = Ctx { opts, now_us, report: ImportReport::default(), known: HashMap::new() };
        import_top(self, &mut ctx, &v)?;
        Ok(ctx.report)
    }

    fn write_places_node(&self, id: Id, index: usize, root: Option<&str>, out: &mut String) {
        let Some(e) = self.entries.get(&id) else { return };
        let n = &e.node;
        out.push_str(&format!(
            "{{\"guid\":\"{}\",\"title\":{},\"index\":{index},\"dateAdded\":{},\"lastModified\":{}",
            guid(id),
            js(&n.title),
            n.added_us,
            n.modified_us
        ));
        match &n.kind {
            Kind::Folder => {
                out.push_str(&format!(",\"typeCode\":2,\"type\":\"{CONTAINER}\""));
                if let Some(r) = root {
                    out.push_str(&format!(",\"root\":\"{r}\""));
                }
                out.push_str(",\"children\":[");
                for (i, c) in e.children.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    self.write_places_node(*c, i, None, out);
                }
                out.push(']');
            }
            Kind::Separator => out.push_str(&format!(",\"typeCode\":3,\"type\":\"{SEPARATOR}\"")),
            Kind::Bookmark { url } => {
                out.push_str(&format!(",\"typeCode\":1,\"type\":\"{PLACE}\""));
                if !n.tags.is_empty() {
                    out.push_str(&format!(",\"tags\":{}", js(&n.tags.join(","))));
                }
                if let Some(k) = &n.keyword {
                    out.push_str(&format!(",\"keyword\":{}", js(k)));
                }
                out.push_str(&format!(",\"uri\":{}", js(url)));
            }
        }
        out.push('}');
    }

    /// Firefox's backup shape: a `placesRoot` container whose children are the menu, toolbar and unfiled roots (Firefox's order),
    /// each carrying its `root` name. `guid` is [`guid`]'s deterministic 12 characters; there are no numeric `id` fields.
    /// Compact, deterministic, importable by [`Bookmarks::import_places_json`].
    pub fn export_places_json(&self) -> String {
        let mut out = format!(
            "{{\"guid\":\"{}\",\"title\":\"\",\"index\":0,\"dateAdded\":0,\"lastModified\":0,\"typeCode\":2,\"type\":\"{CONTAINER}\",\"root\":\"placesRoot\",\"children\":[",
            guid(ROOT)
        );
        let roots = [(MENU, "bookmarksMenuFolder"), (TOOLBAR, "toolbarFolder"), (OTHER, "unfiledBookmarksFolder")];
        for (i, (id, name)) in roots.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            self.write_places_node(*id, i, Some(name), &mut out);
        }
        out.push_str("]}");
        out
    }
}
