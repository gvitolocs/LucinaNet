//! Native functions implemented by the host.
//!
//! A native is a builtin closure whose Rust runtime function is the single trampoline
//! `EmbedNatives::call`. The native pointer and the optional jit info pointer are stored in the
//! data of the native's own BytecodeFunction (where bytecode would be; runtime functions have
//! none), so reading them is a field access. Up to two reserved slots are stored on the function
//! object under private symbols, invisible to JS and kept up to date by the GC. The trampoline is
//! registered with the other runtime functions so heap serialization keeps working.

use std::ffi::c_void;

use crate::{
    must_a,
    runtime::{
        Context, Handle, HeapPtr, PropertyKey, Realm, SymbolValue, Value,
        alloc_error::AllocResult,
        bytecode::function::{BytecodeFunction, ClosureObject},
        embed::{host, state},
        error::type_error,
        function::{build_function_name, set_function_length, set_function_name},
        intrinsics::{intrinsics::Intrinsic, rust_runtime::RuntimeFunction},
        object_value::ObjectValue,
    },
    runtime_fn,
};

/// Maximum number of reserved slots of a native function (`js::NewFunctionWithReserved`).
pub const MAX_NATIVE_RESERVED_SLOTS: usize = 2;

pub(crate) fn new_private_symbol(
    mut cx: Context,
    description: &'static str,
) -> AllocResult<Handle<SymbolValue>> {
    let description = cx.alloc_static_string(description)?;
    SymbolValue::new(cx, Some(description), /* is_private */ true)
}

/// The arguments of a native call. `vp()` follows the SpiderMonkey layout: `vp[0]` is the callee,
/// `vp[1]` is `this` (undefined when constructing) and `vp[2..2 + argc]` are the arguments. The
/// return value is written to `vp[0]`.
///
/// The buffer is rooted and updated by the GC for the duration of the call.
pub struct NativeCall {
    vp: *mut Value,
    argc: u32,
    new_target: Option<Handle<ObjectValue>>,
}

impl NativeCall {
    /// Pointer to the value array in the NaN-boxed encoding (`Value::as_raw_bits`).
    pub fn vp(&mut self) -> *mut u64 {
        self.vp.cast()
    }

    pub fn argc(&self) -> u32 {
        self.argc
    }

    pub fn callee(&self) -> Value {
        self.slot(0)
    }

    pub fn this(&self) -> Value {
        self.slot(1)
    }

    /// The argument at `index`, or undefined if not provided.
    pub fn arg(&self, index: u32) -> Value {
        if index < self.argc {
            self.slot(2 + index as usize)
        } else {
            Value::undefined()
        }
    }

    pub fn set_return_value(&mut self, value: Value) {
        // SAFETY: vp always has at least two slots.
        unsafe { *self.vp = value };
    }

    pub fn is_constructing(&self) -> bool {
        self.new_target.is_some()
    }

    pub fn new_target(&self) -> Option<Handle<ObjectValue>> {
        self.new_target
    }

    fn slot(&self, index: usize) -> Value {
        debug_assert!(index < 2 + self.argc as usize);
        // SAFETY: Index is within the `2 + argc` slots of the buffer.
        unsafe { *self.vp.add(index) }
    }
}

/// Create a native function in a realm. `reserved_slots` must be at most 2.
pub fn new_native_function(
    cx: Context,
    realm: Handle<Realm>,
    native: *const c_void,
    nargs: u32,
    name: Option<Handle<PropertyKey>>,
    constructor: bool,
    reserved_slots: u8,
) -> AllocResult<Handle<ObjectValue>> {
    new_native_function_with_jit_info(
        cx,
        realm,
        native,
        std::ptr::null(),
        nargs,
        name,
        constructor,
        reserved_slots,
    )
}

/// Size of the data stored with each native: the native pointer and the jit info pointer.
const NATIVE_DATA_SIZE: usize = 2 * size_of::<usize>();

/// Create a native function carrying a `JSJitInfo` pointer (null for none), read back with
/// `native_jit_info`. `reserved_slots` must be at most 2.
///
/// The native and jit info pointers are stored in the function's data (where bytecode would be),
/// so reading them back is a field access, not a property lookup, and they move with the function.
pub fn new_native_function_with_jit_info(
    cx: Context,
    realm: Handle<Realm>,
    native: *const c_void,
    jit_info: *const c_void,
    nargs: u32,
    name: Option<Handle<PropertyKey>>,
    constructor: bool,
    reserved_slots: u8,
) -> AllocResult<Handle<ObjectValue>> {
    assert!(reserved_slots as usize <= MAX_NATIVE_RESERVED_SLOTS, "too many reserved slots");

    let mut data = [0u8; NATIVE_DATA_SIZE];
    data[..size_of::<usize>()].copy_from_slice(&(native as usize).to_ne_bytes());
    data[size_of::<usize>()..].copy_from_slice(&(jit_info as usize).to_ne_bytes());

    // The bytecode function's name is shown in stack traces. Assumes that names given by the host
    // are within the string length limit.
    let function_name = name
        .map(|name| Ok(must_a!(build_function_name(cx, name, None))))
        .transpose()?;

    let bytecode_function = BytecodeFunction::new_rust_runtime_function_with_data(
        cx,
        RuntimeFunction::EmbedNatives_call.to_id(),
        realm,
        constructor,
        function_name,
        /* function_length */ 0,
        &data,
    )?;

    let proto = realm.get_intrinsic(Intrinsic::FunctionPrototype);
    let closure = ClosureObject::new_without_properties(
        cx,
        bytecode_function,
        realm.default_global_scope(),
        Some(proto),
    )?;

    let mut function = closure.as_object();
    set_function_length(cx, function, nargs)?;

    let name = name.unwrap_or_else(|| cx.names.empty_string());
    must_a!(set_function_name(cx, function, name, None));

    for index in 0..reserved_slots as usize {
        let key = state(cx).native_reserved_keys[index].to_handle();
        function.private_element_set(cx, key, cx.undefined())?;
    }

    Ok(function)
}

/// The data of a native function, or None if the object is not one. A field access.
#[inline]
fn native_data(function: HeapPtr<ObjectValue>) -> Option<(usize, usize)> {
    let closure = function.as_opt::<ClosureObject>()?;
    let bytecode_function = closure.function_ptr();
    if bytecode_function.runtime_function_id() != Some(RuntimeFunction::EmbedNatives_call.to_id()) {
        return None;
    }

    let data = bytecode_function.runtime_function_data();
    let (native, jit_info) = data.split_at(size_of::<usize>());
    Some((
        usize::from_ne_bytes(native.try_into().unwrap()),
        usize::from_ne_bytes(jit_info[..size_of::<usize>()].try_into().unwrap()),
    ))
}

fn is_native_function(function: HeapPtr<ObjectValue>) -> bool {
    native_data(function).is_some()
}

/// The native pointer of a function created by `new_native_function`, or None for other values.
pub fn native_of(_: Context, function: Handle<ObjectValue>) -> Option<*const c_void> {
    native_data(*function).map(|(native, _)| native as *const c_void)
}

/// The jit info of a native function (`FUNCTION_VALUE_TO_JITINFO`), or None for any value that is
/// not a native with jit info. Never panics. A field access, cheap enough for every DOM call.
#[inline]
pub fn native_jit_info(_: Context, value: Value) -> Option<*const c_void> {
    if !value.is_object() {
        return None;
    }

    match native_data(value.as_object()) {
        Some((_, jit_info)) if jit_info != 0 => Some(jit_info as *const c_void),
        _ => None,
    }
}

/// Set (or clear, with null) the jit info of a native function. Returns false if the object is not
/// a native function.
pub fn set_native_jit_info(
    _: Context,
    function: Handle<ObjectValue>,
    jit_info: *const c_void,
) -> bool {
    if !is_native_function(*function) {
        return false;
    }

    let mut bytecode_function = function.cast::<ClosureObject>().function_ptr();
    bytecode_function.runtime_function_data_mut()[size_of::<usize>()..NATIVE_DATA_SIZE]
        .copy_from_slice(&(jit_info as usize).to_ne_bytes());
    true
}

/// Read a reserved slot of a native function, or None if the value is not a native function or
/// the slot does not exist. Never panics.
pub fn function_reserved_opt(cx: Context, function: Handle<Value>, index: usize) -> Option<Value> {
    if !function.is_object() || index >= MAX_NATIVE_RESERVED_SLOTS {
        return None;
    }

    let function = function.as_object();
    if !is_native_function(*function) {
        return None;
    }

    let key = state(cx).native_reserved_keys[index].to_handle();
    let property = function.private_element_find(cx, key)?;
    Some(*property.value())
}

/// Read a reserved slot of a native function. Panics if the function is not a native or the slot
/// does not exist.
pub fn function_reserved(cx: Context, function: Handle<ObjectValue>, index: usize) -> Value {
    assert!(is_native_function(*function), "not a native function");
    let key = state(cx).native_reserved_keys[index].to_handle();
    let property = function
        .private_element_find(cx, key)
        .expect("reserved slot out of range");
    *property.value()
}

/// Write a reserved slot of a native function. Panics if the function is not a native or the slot
/// does not exist.
pub fn set_function_reserved(
    cx: Context,
    mut function: Handle<ObjectValue>,
    index: usize,
    value: Handle<Value>,
) -> AllocResult<()> {
    assert!(is_native_function(*function), "not a native function");
    let key = state(cx).native_reserved_keys[index].to_handle();
    assert!(function.has_private_element(key), "reserved slot out of range");
    function.private_element_set(cx, key, value)
}

pub struct EmbedNatives;

impl EmbedNatives {
    runtime_fn! {
    /// The trampoline for every native function.
    fn call(cx, this_value, arguments) {
        let callee = cx.current_function();
        let new_target = cx.current_new_target();

        let Some((native, _)) = native_data(*callee) else {
            return type_error(cx, "not a native function");
        };
        let native = native as *const c_void;

        let argc = arguments.len();
        let mut vp = Vec::with_capacity(2 + argc);
        vp.push(*callee.as_value());
        vp.push(if new_target.is_some() {
            Value::undefined()
        } else {
            *this_value
        });
        vp.extend(arguments.iter().copied());

        // Root the buffer for the duration of the call. Nested native calls push their own buffer,
        // and the buffer's heap allocation does not move while it is on the stack.
        let native_vps = &mut state(cx).native_vps;
        native_vps.push(vp);
        let vp_ptr = native_vps.last_mut().unwrap().as_mut_ptr();

        let mut call = NativeCall { vp: vp_ptr, argc: argc as u32, new_target };
        let result = host(cx).call_native(cx, native, &mut call);

        let vp = state(cx).native_vps.pop().unwrap();
        debug_assert!(vp.as_ptr() == vp_ptr.cast_const());

        result?;

        let return_value = vp[0];
        if new_target.is_some() && !return_value.is_object() {
            return type_error(cx, "native constructor did not return an object");
        }

        Ok(return_value.to_handle(cx))
    }}
}
