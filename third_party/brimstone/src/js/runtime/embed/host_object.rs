use crate::{
    extend_object,
    runtime::{
        Context, EvalResult, Handle, HeapPtr, PropertyKey, Realm, Value,
        embed::{
            call_resolve_hook, host,
            host_space::{HostAllocation, HostFlags, HostHeaderPtr},
            jobs, state,
        },
        error::type_error,
        gc::{HeapItem, HeapVisitor},
        object_value::{ObjectValue, VirtualObject},
        ordinary_object::{
            OrdinaryObject, init_object_pointer_fields, ordinary_define_own_property,
            ordinary_delete, ordinary_get, ordinary_get_own_property, ordinary_has_property,
            ordinary_own_property_keys, ordinary_set,
        },
        property::Property,
        property_descriptor::PropertyDescriptor,
        rust_vtables::extract_virtual_object_vtable,
        shape::Shape,
    },
};

extend_object! {
    /// An object allocated in the non-moving host space on behalf of the embedder. Its host data
    /// (class, flags, reserved slots, proxy handler and private) lives in the `HostHeader` directly
    /// before the object.
    ///
    /// Behaves as an ordinary object, except that a proxy handler routes every internal method to
    /// the host, a resolve hook is consulted on own property misses, and callable host objects
    /// are called through the host.
    pub struct HostObject {}
}

impl HostObject {
    pub const VIRTUAL_OBJECT_VTABLE: *const () = extract_virtual_object_vtable::<Self>();

    #[inline]
    pub fn header(&self) -> HostHeaderPtr {
        // SAFETY: Every `HostObject` is allocated by `alloc_host_object` in the host space (the
        // `HostObject` kind is only used for shapes of the embedding API), so `self` is the object
        // of a host space allocation, directly preceded by its header. Like every variable-sized
        // heap item (inline properties, string data), this reaches outside `size_of::<HostObject>()`
        // from `&self`, relying on the engine's premise that `&self` of a heap item carries the
        // provenance of its whole allocation.
        unsafe { HostHeaderPtr::from_object_ptr(self as *const HostObject) }
    }

    #[inline]
    pub fn is_callable(&self) -> bool {
        self.header()
            .flags()
            .intersects(HostFlags::CALLABLE | HostFlags::INTERNAL_JOB)
    }

    #[inline]
    pub fn is_constructor(&self) -> bool {
        self.header().flags().contains(HostFlags::CONSTRUCTOR)
    }

    #[inline]
    pub fn is_host_proxy(&self) -> bool {
        self.header().is_proxy()
    }

    #[inline]
    pub fn realm_ptr(&self) -> HeapPtr<Realm> {
        self.header().realm()
    }
}

/// Allocate and initialize an object in the host space. Does not allocate on the moving heap, so
/// cannot trigger a GC.
///
/// Panics during a GC (from a GC hook): the GC has already traced the host space, so the new
/// object's fields would never be visited and would point to moved objects afterwards, and its
/// region would be missing from the GC's view of the host space.
pub(crate) fn alloc_host_object<T>(
    cx: Context,
    shape: HeapPtr<Shape>,
    allocation: HostAllocation,
) -> HeapPtr<T> {
    let state = state(cx);
    assert!(!state.in_gc, "host object created during a garbage collection");

    let header = state.space.alloc(cx.heap.info(), allocation);

    // Host objects are GC cells in SpiderMonkey, counted in the zone's GC heap (gc/Allocator.cpp:496-501): a collection is due
    // once the host space reaches its threshold (`gc::accounting`).
    let used = state.space.used_bytes();
    let mut heap_cx = cx;
    if crate::runtime::gc::accounting::due(used, heap_cx.heap.accounting().host_threshold) {
        heap_cx.heap.request_collection();
    }

    // SAFETY: The header was just allocated and initialized, and nothing else references it yet.
    let mut object = unsafe { header.as_ref() }.object().cast::<ObjectValue>();
    init_object_pointer_fields(cx, object, shape);
    object.set_uninit_hash_code();

    object.cast()
}

/// Convert a descriptor answered by a host into a complete property.
fn descriptor_to_property(cx: Context, mut desc: PropertyDescriptor) -> EvalResult<Property> {
    desc.complete_property_descriptor(cx);
    Ok(desc.to_property(cx)?)
}

impl VirtualObject for Handle<HostObject> {
    #[inline]
    fn as_ordinary_object(&self) -> Handle<OrdinaryObject> {
        self.ordinary_object()
    }

    fn get_own_property(
        &self,
        cx: Context,
        key: Handle<PropertyKey>,
    ) -> EvalResult<Option<Property>> {
        let header = self.header();
        let object = self.as_object();

        if header.is_proxy() {
            let desc = host(cx).proxy_get_own_property(cx, header.handler(), object, key)?;
            return desc
                .map(|desc| descriptor_to_property(cx, desc))
                .transpose();
        }

        let property = ordinary_get_own_property(cx, object, key);
        if property.is_some() || !header.flags().contains(HostFlags::HAS_RESOLVE) {
            return Ok(property);
        }

        // Own property miss on an object with a resolve hook. Retry once if the hook defined it.
        if call_resolve_hook(cx, object, key)? {
            Ok(ordinary_get_own_property(cx, object, key))
        } else {
            Ok(None)
        }
    }

    fn define_own_property(
        &mut self,
        cx: Context,
        key: Handle<PropertyKey>,
        desc: PropertyDescriptor,
    ) -> EvalResult<bool> {
        let header = self.header();
        if header.is_proxy() {
            return host(cx).proxy_define_property(
                cx,
                header.handler(),
                self.as_object(),
                key,
                desc,
            );
        }

        ordinary_define_own_property(cx, self.as_object(), key, desc)
    }

    fn has_property(&self, cx: Context, key: Handle<PropertyKey>) -> EvalResult<bool> {
        let header = self.header();
        if header.is_proxy() {
            return host(cx).proxy_has(cx, header.handler(), self.as_object(), key);
        }

        ordinary_has_property(cx, self.as_object(), key)
    }

    fn get(
        &self,
        cx: Context,
        key: Handle<PropertyKey>,
        receiver: Handle<Value>,
    ) -> EvalResult<Handle<Value>> {
        let header = self.header();
        if header.is_proxy() {
            return host(cx).proxy_get(cx, header.handler(), self.as_object(), key, receiver);
        }

        ordinary_get(cx, self.as_object(), key, receiver)
    }

    fn set(
        &mut self,
        cx: Context,
        key: Handle<PropertyKey>,
        value: Handle<Value>,
        receiver: Handle<Value>,
    ) -> EvalResult<bool> {
        let header = self.header();
        if header.is_proxy() {
            return host(cx).proxy_set(
                cx,
                header.handler(),
                self.as_object(),
                key,
                value,
                receiver,
            );
        }

        ordinary_set(cx, self.as_object(), key, value, receiver)
    }

    fn delete(&mut self, cx: Context, key: Handle<PropertyKey>) -> EvalResult<bool> {
        let header = self.header();
        if header.is_proxy() {
            return host(cx).proxy_delete(cx, header.handler(), self.as_object(), key);
        }

        ordinary_delete(cx, self.as_object(), key)
    }

    fn own_property_keys(&self, cx: Context) -> EvalResult<Vec<Handle<Value>>> {
        let header = self.header();
        if header.is_proxy() {
            return host(cx).proxy_own_keys(cx, header.handler(), self.as_object());
        }

        Ok(ordinary_own_property_keys(cx, self.as_object())?)
    }

    fn call(
        &self,
        cx: Context,
        this_argument: Handle<Value>,
        arguments: &[Handle<Value>],
    ) -> EvalResult<Handle<Value>> {
        if self.header().flags().contains(HostFlags::INTERNAL_JOB) {
            return jobs::run_job(cx, self.as_object());
        }

        host(cx).call_host_object(cx, self.as_object(), this_argument, arguments, None)
    }

    fn construct(
        &self,
        cx: Context,
        arguments: &[Handle<Value>],
        new_target: Handle<ObjectValue>,
    ) -> EvalResult<Handle<ObjectValue>> {
        let result = host(cx).call_host_object(
            cx,
            self.as_object(),
            cx.undefined(),
            arguments,
            Some(new_target),
        )?;

        if !result.is_object() {
            return type_error(cx, "host constructor did not return an object");
        }

        Ok(result.as_object())
    }

    fn get_realm(&self, _: Context) -> EvalResult<HeapPtr<Realm>> {
        Ok(self.realm_ptr())
    }
}

impl HeapItem for HostObject {
    fn byte_size(object: HeapPtr<Self>) -> usize {
        object.object_byte_size()
    }

    /// Only visits the object fields. The header is visited separately by the GC when the object
    /// is marked, since host objects are never moved or scanned as part of a semispace.
    fn visit_pointers(mut object: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        object.visit_object_pointers(visitor);
    }
}
