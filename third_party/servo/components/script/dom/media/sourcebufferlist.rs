/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use dom_struct::dom_struct;
use js::context::JSContext;
use script_bindings::cell::DomRefCell;
use script_bindings::reflector::reflect_dom_object;

use crate::dom::bindings::codegen::Bindings::SourceBufferListBinding::SourceBufferListMethods;
use crate::dom::bindings::root::{Dom, DomRoot};
use crate::dom::eventtarget::EventTarget;
use crate::dom::media::sourcebuffer::SourceBuffer;
use crate::dom::window::Window;

/// <https://www.w3.org/TR/media-source-2/#sourcebufferlist>
#[dom_struct]
pub(crate) struct SourceBufferList {
    eventtarget: EventTarget,
    buffers: DomRefCell<Vec<Dom<SourceBuffer>>>,
}

impl SourceBufferList {
    fn new_inherited() -> SourceBufferList {
        SourceBufferList {
            eventtarget: EventTarget::new_inherited(),
            buffers: DomRefCell::new(Vec::new()),
        }
    }

    pub(crate) fn new(cx: &mut JSContext, window: &Window) -> DomRoot<SourceBufferList> {
        reflect_dom_object(cx, Box::new(SourceBufferList::new_inherited()), window)
    }

    pub(crate) fn len(&self) -> usize {
        self.buffers.borrow().len()
    }

    pub(crate) fn item(&self, idx: usize) -> Option<DomRoot<SourceBuffer>> {
        self.buffers
            .borrow()
            .get(idx)
            .map(|buffer| DomRoot::from_ref(&**buffer))
    }

    pub(crate) fn contains(&self, buffer: &SourceBuffer) -> bool {
        self.buffers.borrow().iter().any(|b| &**b == buffer)
    }

    pub(crate) fn push(&self, buffer: &SourceBuffer) {
        self.buffers.borrow_mut().push(Dom::from_ref(buffer));
    }

    /// Removes `buffer`; answers whether it was in the list.
    pub(crate) fn remove(&self, buffer: &SourceBuffer) -> bool {
        let mut buffers = self.buffers.borrow_mut();
        let Some(index) = buffers.iter().position(|b| &**b == buffer) else {
            return false;
        };
        buffers.remove(index);
        true
    }

    /// All buffers, rooted, in order, so callers can run steps on them without the list borrowed.
    pub(crate) fn buffers(&self) -> Vec<DomRoot<SourceBuffer>> {
        self.buffers
            .borrow()
            .iter()
            .map(|buffer| DomRoot::from_ref(&**buffer))
            .collect()
    }

    pub(crate) fn clear(&self) {
        self.buffers.borrow_mut().clear();
    }
}

impl SourceBufferListMethods<crate::DomTypeHolder> for SourceBufferList {
    /// <https://www.w3.org/TR/media-source-2/#dom-sourcebufferlist-length>
    fn Length(&self) -> u32 {
        self.len() as u32
    }

    /// <https://www.w3.org/TR/media-source-2/#dfn-sourcebufferlist-getter>
    fn IndexedGetter(&self, index: u32) -> Option<DomRoot<SourceBuffer>> {
        self.item(index as usize)
    }

    // <https://www.w3.org/TR/media-source-2/#dom-sourcebufferlist-onaddsourcebuffer>
    event_handler!(addsourcebuffer, GetOnaddsourcebuffer, SetOnaddsourcebuffer);

    // <https://www.w3.org/TR/media-source-2/#dom-sourcebufferlist-onremovesourcebuffer>
    event_handler!(
        removesourcebuffer,
        GetOnremovesourcebuffer,
        SetOnremovesourcebuffer
    );
}
