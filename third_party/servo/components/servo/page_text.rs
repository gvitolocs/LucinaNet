/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use embedder_traits::{PageText, PageTextId};
use rustc_hash::FxHashMap;
use servo_base::id::WebViewId;
use servo_constellation_traits::EmbedderToConstellationMessage;

use crate::proxies::ConstellationProxy;

/// Tracks the page text requests that are out in the engine and calls each callback once, with
/// the answer. The text is handed to the callback and kept nowhere here: the callback is removed
/// from the map before it runs, and nothing else holds the value.
pub(crate) struct PageTextRequests {
    current_id: PageTextId,
    constellation_proxy: ConstellationProxy,
    pending: FxHashMap<PageTextId, Box<dyn FnOnce(PageText)>>,
}

impl PageTextRequests {
    pub(crate) fn new(constellation_proxy: ConstellationProxy) -> Self {
        Self {
            current_id: PageTextId(0),
            constellation_proxy,
            pending: Default::default(),
        }
    }

    fn generate_id(&mut self) -> PageTextId {
        let next_id = PageTextId(self.current_id.0 + 1);
        std::mem::replace(&mut self.current_id, next_id)
    }

    pub(crate) fn request(
        &mut self,
        webview_id: WebViewId,
        limit_bytes: usize,
        callback: Box<dyn FnOnce(PageText)>,
    ) {
        let id = self.generate_id();
        self.pending.insert(id, callback);
        self.constellation_proxy
            .send(EmbedderToConstellationMessage::RequestPageText(
                webview_id,
                id,
                limit_bytes,
            ));
    }

    /// The callback for `id`, removed from the map so that it can be called after the borrow of
    /// this struct is released. `None` for an id that was already answered.
    pub(crate) fn take_callback(&mut self, id: PageTextId) -> Option<Box<dyn FnOnce(PageText)>> {
        self.pending.remove(&id)
    }
}
