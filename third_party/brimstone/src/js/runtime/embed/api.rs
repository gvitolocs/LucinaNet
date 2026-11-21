//! Re-exports and thin wrappers of engine operations an embedder needs.

pub use num_bigint::BigInt;
use num_traits::{FromPrimitive, ToPrimitive};

use super::with_realm;
// --- Core types ---
pub use crate::common::string::StringWidth;
// --- Property operations ---
pub use crate::runtime::abstract_operations::{
    create_data_property, define_property_or_throw, delete_property_or_throw, get,
    has_own_property, has_property, is_extensible, length_of_array_like, set,
};
pub use crate::runtime::alloc_error::AllocResult;
pub use crate::runtime::builtin_names::BuiltinSymbols;
use crate::runtime::intrinsics::typed_array_prototype::{
    is_typed_array_out_of_bounds, make_typed_array_with_buffer_witness_record,
};
pub use crate::runtime::object_value::ObjectValue;
pub use crate::runtime::{
    Context, EvalResult, Handle, HeapPtr, PropertyDescriptor, PropertyFlags, PropertyKey, Realm,
    Value,
};
use crate::runtime::{
    abstract_operations::construct as construct_object,
    array_object::{self},
    collections::hash_map::BsHashMapField,
    interned_strings::InternedStrings,
    intrinsics::date_object::time_clip,
    ordinary_object::ordinary_object_create,
    promise_object::{self, PromiseState},
    string_object::StringObject,
};

/// Set a property without throwing on failure, returning whether the set succeeded.
pub fn set_property(
    cx: Context,
    mut object: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
    value: Handle<Value>,
    receiver: Handle<Value>,
) -> EvalResult<bool> {
    object.set(cx, key, value, receiver)
}

/// The full own property descriptor of `key`, or None if the object does not have the property.
pub fn get_own_property_descriptor(
    cx: Context,
    object: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
) -> EvalResult<Option<PropertyDescriptor>> {
    Ok(object
        .get_own_property(cx, key)?
        .map(|property| property.to_property_descriptor()))
}

/// The object's own property keys, as values (strings, symbols and array indices).
pub fn own_property_keys(
    cx: Context,
    object: Handle<ObjectValue>,
) -> EvalResult<Vec<Handle<Value>>> {
    object.own_property_keys(cx)
}

/// The object's prototype, or None if it has none.
pub fn get_prototype_of(
    cx: Context,
    object: Handle<ObjectValue>,
) -> EvalResult<Option<Handle<ObjectValue>>> {
    object.get_prototype_of(cx)
}

/// Set the object's prototype, returning whether the operation succeeded.
pub fn set_prototype_of(
    cx: Context,
    mut object: Handle<ObjectValue>,
    prototype: Option<Handle<ObjectValue>>,
) -> EvalResult<bool> {
    object.set_prototype_of(cx, prototype)
}

/// Make the object non-extensible, returning whether the operation succeeded.
pub fn prevent_extensions(cx: Context, mut object: Handle<ObjectValue>) -> EvalResult<bool> {
    object.prevent_extensions(cx)
}

/// A property key from a string.
pub fn property_key_from_string(cx: Context, string: &str) -> EvalResult<Handle<PropertyKey>> {
    let mut cx = cx;
    Ok(PropertyKey::string_handle(cx, cx.alloc_string(string)?)?)
}

/// A property key from a symbol.
pub fn property_key_from_symbol(symbol: Handle<SymbolValue>) -> Handle<PropertyKey> {
    PropertyKey::symbol(symbol)
}

/// A property key from an array index.
pub fn property_key_from_index(cx: Context, index: u32) -> AllocResult<Handle<PropertyKey>> {
    PropertyKey::array_index_handle(cx, index)
}

// --- Conversions ---

// --- Strings ---
pub use crate::runtime::string_value::{FlatString, StringValue};
pub use crate::runtime::type_utilities::{
    ToPrimitivePreferredType, is_array, is_callable, is_constructor_value, same_value,
    same_value_zero_non_allocating, to_boolean, to_int32, to_number, to_object, to_primitive,
    to_property_key, to_string, to_uint32,
};

/// Create a one-byte (Latin-1) string from raw bytes.
pub fn string_from_latin1(cx: Context, bytes: &[u8]) -> EvalResult<Handle<FlatString>> {
    Ok(FlatString::from_one_byte_slice(cx, bytes)?.to_handle())
}

/// Create a string from UTF-16 code units.
pub fn string_from_code_units(cx: Context, code_units: &[u16]) -> EvalResult<Handle<FlatString>> {
    Ok(FlatString::new_two_byte(cx, code_units)?.to_handle())
}

/// Create a string from a UTF-8 string slice.
pub fn string_from_utf8(cx: Context, string: &str) -> EvalResult<Handle<FlatString>> {
    Ok(FlatString::from_wtf8(cx, string.as_bytes())?.to_handle())
}

/// The length of a string in code units.
pub fn string_length(string: Handle<StringValue>) -> u32 {
    string.len()
}

/// Whether a string is stored as one-byte (Latin-1) code units.
pub fn string_is_one_byte(string: Handle<StringValue>) -> bool {
    string.width() == StringWidth::OneByte
}

/// The characters of a flattened string, as Latin-1 bytes or UTF-16 code units.
pub enum StringChars<'a> {
    Latin1(&'a [u8]),
    TwoByte(&'a [u16]),
}

/// The characters of a flat string, borrowed from the string's storage.
///
/// # Safety
/// `string` must point to a live flat string, and for as long as the returned slice is used (all of
/// `'a`) nothing may allocate on the JS heap or run a GC: a GC moves the string and reuses or frees
/// its old storage. Copy the characters out before calling anything that may allocate.
pub unsafe fn flat_string_chars<'a>(string: HeapPtr<FlatString>) -> StringChars<'a> {
    let len = string.len() as usize;
    match string.width() {
        StringWidth::OneByte => {
            // SAFETY: `one_byte_data` points to `len` initialized bytes inside the FlatString
            // allocation, which the caller guarantees is neither moved, freed nor written during
            // `'a` (no GC and no heap allocation).
            unsafe { StringChars::Latin1(std::slice::from_raw_parts(string.one_byte_data(), len)) }
        }
        StringWidth::TwoByte => {
            // SAFETY: `two_byte_data` points to `len` initialized code units inside the FlatString
            // allocation, which the caller guarantees is neither moved, freed nor written during
            // `'a` (no GC and no heap allocation).
            unsafe { StringChars::TwoByte(std::slice::from_raw_parts(string.two_byte_data(), len)) }
        }
    }
}

/// A lossy Rust `String` copy of a string (invalid code units become U+FFFD).
pub fn string_to_rust_string(string: Handle<StringValue>) -> EvalResult<String> {
    Ok(string.flatten_to_handle()?.to_wtf8_string().to_string())
}

// --- Symbols ---

pub use crate::runtime::symbol_value::SymbolValue;

/// Create a new symbol with an optional description.
pub fn new_symbol(cx: Context, description: Option<&str>) -> EvalResult<Handle<SymbolValue>> {
    let mut cx = cx;
    let description = description.map(|d| cx.alloc_string(d)).transpose()?;
    Ok(SymbolValue::new(cx, description, /* is_private */ false)?)
}

/// The `Symbol.for` global registry lookup: returns the registered symbol for `key`, creating and
/// registering a new one if none exists.
pub fn symbol_for(cx: Context, key: &str) -> EvalResult<Handle<SymbolValue>> {
    let mut cx = cx;
    let string_key = cx.alloc_string(key)?.flatten_to_handle()?;
    if let Some(symbol) = cx.global_symbol_registry().get(&*string_key) {
        return Ok(symbol.to_handle());
    }

    let new_symbol =
        SymbolValue::new(cx, Some(string_key.as_string()), /* is_private */ false)?;
    cx.global_symbol_registry_field()
        .maybe_grow_for_insertion(cx)?
        .insert_without_growing(*string_key, *new_symbol);

    Ok(new_symbol)
}

/// The description of a symbol, if it has one.
pub fn symbol_description(symbol: Handle<SymbolValue>) -> Option<Handle<FlatString>> {
    symbol.description()
}

// --- BigInt ---

pub use crate::runtime::bigint_value::BigIntValue;

/// Create a BigInt from an i64.
pub fn bigint_from_i64(cx: Context, value: i64) -> AllocResult<Handle<BigIntValue>> {
    BigIntValue::new(cx, BigInt::from(value))
}

/// Create a BigInt from a u64.
pub fn bigint_from_u64(cx: Context, value: u64) -> AllocResult<Handle<BigIntValue>> {
    BigIntValue::new(cx, BigInt::from(value))
}

/// Create a BigInt from an f64, returning None for non-integral or non-finite values.
pub fn bigint_from_f64(cx: Context, value: f64) -> AllocResult<Option<Handle<BigIntValue>>> {
    if !value.is_finite() || value.fract() != 0.0 {
        return Ok(None);
    }
    match BigInt::from_f64(value) {
        Some(bigint) => Ok(Some(BigIntValue::new(cx, bigint)?)),
        None => Ok(None),
    }
}

/// The i64 value of a BigInt, or None if it does not fit.
pub fn bigint_to_i64(bigint: Handle<BigIntValue>) -> Option<i64> {
    bigint.bigint().to_i64()
}

/// The u64 value of a BigInt, or None if it is negative or does not fit.
pub fn bigint_to_u64(bigint: Handle<BigIntValue>) -> Option<u64> {
    bigint.bigint().to_u64()
}

/// The f64 value of a BigInt, lossy for large values, or None if not representable.
pub fn bigint_to_f64(bigint: Handle<BigIntValue>) -> Option<f64> {
    bigint.bigint().to_f64()
}

// --- Arrays ---

pub use crate::runtime::array_object::{ArrayObject, create_array_from_list};

/// Create a new array with the given length in a realm.
pub fn array_with_length(
    cx: Context,
    realm: Handle<Realm>,
    length: u64,
) -> EvalResult<Handle<ArrayObject>> {
    with_realm(cx, realm, |cx| array_object::array_create(cx, length, None))
}

// --- ArrayBuffer and typed arrays ---

pub use crate::runtime::collections::array::ByteArray;
pub use crate::runtime::intrinsics::array_buffer_object::ArrayBufferObject;
pub use crate::runtime::intrinsics::typed_array::{DynTypedArray, TypedArray, TypedArrayKind};

/// Create a new zero-filled ArrayBuffer of `byte_length` bytes in a realm.
pub fn array_buffer_new(
    cx: Context,
    realm: Handle<Realm>,
    byte_length: usize,
) -> EvalResult<Handle<ArrayBufferObject>> {
    with_realm(cx, realm, |cx| {
        let constructor = cx.get_intrinsic(Intrinsic::ArrayBufferConstructor);
        ArrayBufferObject::new(cx, constructor, byte_length, /* max_byte_length */ None, None)
    })
}

/// The byte length of an ArrayBuffer.
pub fn array_buffer_byte_length(buffer: Handle<ArrayBufferObject>) -> usize {
    buffer.byte_length()
}

/// Whether an ArrayBuffer is detached.
pub fn array_buffer_is_detached(buffer: Handle<ArrayBufferObject>) -> bool {
    buffer.is_detached()
}

/// A pointer to the buffer's backing data, or None if the buffer is detached.
///
/// SpiderMonkey's contract for `JS::GetArrayBufferData` (`JS::AutoRequireNoGC`): the pointer is
/// valid only until the embedder next runs JS, allows a GC, calls `WebAssembly.Memory.grow` or calls
/// into wasm. The bytes of a heap buffer move with a GC; the bytes of a wasm memory's buffer move
/// when the memory grows, which can only happen while JS (or wasm) runs, and the buffer is then
/// detached (WebAssembly design §3.2-3.3).
pub fn array_buffer_data_ptr(buffer: Handle<ArrayBufferObject>) -> Option<*mut u8> {
    buffer.raw_data_ptr()
}

pub use crate::runtime::intrinsics::array_buffer_object::{DetachKey, EXTERNAL_ALIGNMENT};

/// Who owns the memory of an external ArrayBuffer (`array_buffer_new_external`).
pub enum ExternalOwner {
    /// The embedder owns the memory (SpiderMonkey's `NewArrayBufferWithUserOwnedContents`): it
    /// keeps the memory valid until it detaches the buffer (`array_buffer_detach`), and frees it
    /// afterwards itself.
    None,
    /// An engine cell owns the memory (a wasm store); the buffer keeps the cell alive while
    /// attached. The buffer gets the `WasmMemory` detach key.
    Cell(Handle<ObjectValue>),
    /// The engine owns the memory from now on (SpiderMonkey's `NewExternalArrayBuffer` with a
    /// `BufferContentsFreeFunc`): the closure frees it, exactly once, after the buffer has been
    /// detached or has died and been collected, or when the context is destroyed. It runs without
    /// JS on the stack, possibly inside a GC: it must not call into the engine or touch any
    /// ArrayBuffer's bytes. If creating the buffer fails, the closure is dropped without being
    /// called and the caller keeps the memory.
    Free(Box<dyn FnOnce()>),
}

/// Create a fixed-length ArrayBuffer over `byte_length` bytes of memory at `data` that does not
/// live on the JS heap (WebAssembly design §3.1). The bytes are neither copied nor initialized.
///
/// The detach key is `WasmMemory` for `ExternalOwner::Cell` and `None` otherwise: a buffer without
/// a key can be detached by `array_buffer_detach`, and `transfer`/`transferToFixedLength` copy its
/// bytes into a heap buffer first. Throws a TypeError if `data` is null or not aligned to 8 bytes
/// (`EXTERNAL_ALIGNMENT`) while `byte_length` is not 0; a zero-length buffer ignores `data`.
///
/// # Safety
/// While the buffer is attached, the `byte_length` bytes at `data` must be readable and writable,
/// and nothing else may read or write them while the engine runs: no Rust reference to them, and
/// no other attached ArrayBuffer covering any of them. For `ExternalOwner::None` the embedder keeps
/// the memory allocated until it detaches the buffer; for `Free` until the closure runs; for `Cell`
/// the owner of the memory keeps it allocated and matching the buffer (invariant B1) until it
/// detaches the buffer.
pub unsafe fn array_buffer_new_external(
    cx: Context,
    realm: Handle<Realm>,
    data: *mut u8,
    byte_length: usize,
    owner: ExternalOwner,
) -> EvalResult<Handle<ArrayBufferObject>> {
    with_realm(cx, realm, |cx| {
        let constructor = cx.get_intrinsic(Intrinsic::ArrayBufferConstructor);
        let new_external = |cx, owner: Option<Handle<ObjectValue>>, key| {
            // SAFETY: the caller's contract (`# Safety` above) is `new_external`'s, for `data` and
            // `byte_length` as given; null and misaligned data are refused there.
            unsafe {
                ArrayBufferObject::new_external(cx, constructor, data, byte_length, owner, key)
            }
        };

        match owner {
            ExternalOwner::None => new_external(cx, None, DetachKey::None),
            ExternalOwner::Cell(cell) => {
                debug_assert!(
                    super::is_engine_cell(*cell),
                    "the owner of a wasm memory buffer is an engine cell"
                );
                new_external(cx, Some(cell), DetachKey::WasmMemory)
            }
            ExternalOwner::Free(free) => crate::handle_scope!(cx, {
                // The cell that runs the closure when it dies. It cannot be collected before the
                // buffer holds it: it is behind a handle while the buffer is allocated. That handle
                // belongs to this inner scope, so afterwards only the buffer keeps the cell alive.
                let cell = super::new_engine_cell(cx, &super::EXTERNAL_FREE_CELL_HOOKS, *realm, 0);
                super::register_external_free(cx, *cell, free);
                let buffer = new_external(cx, Some(cell), DetachKey::None);
                if buffer.is_err() {
                    // No buffer: the caller keeps the memory, and the closure is dropped uncalled
                    super::forget_external_free(cx, *cell);
                }
                buffer
            }),
        }
    })
}

/// Whether an ArrayBuffer is attached and its bytes live outside the JS heap.
pub fn array_buffer_is_external(buffer: Handle<ArrayBufferObject>) -> bool {
    buffer.is_external()
}

/// The detach key of an ArrayBuffer: `WasmMemory` for the buffer of a `WebAssembly.Memory`,
/// `None` otherwise (and once detached). Embedders refuse to transfer, steal or detach a buffer
/// with the `WasmMemory` key (structured-clone transfer, `JS::StealArrayBufferContents`).
pub fn array_buffer_detach_key(buffer: Handle<ArrayBufferObject>) -> DetachKey {
    buffer.detach_key()
}

/// The constructor intrinsic for a typed array kind.
fn typed_array_constructor_intrinsic(kind: TypedArrayKind) -> Intrinsic {
    match kind {
        TypedArrayKind::Int8 => Intrinsic::Int8ArrayConstructor,
        TypedArrayKind::UInt8 => Intrinsic::UInt8ArrayConstructor,
        TypedArrayKind::UInt8Clamped => Intrinsic::UInt8ClampedArrayConstructor,
        TypedArrayKind::Int16 => Intrinsic::Int16ArrayConstructor,
        TypedArrayKind::UInt16 => Intrinsic::UInt16ArrayConstructor,
        TypedArrayKind::Int32 => Intrinsic::Int32ArrayConstructor,
        TypedArrayKind::UInt32 => Intrinsic::UInt32ArrayConstructor,
        TypedArrayKind::BigInt64 => Intrinsic::BigInt64ArrayConstructor,
        TypedArrayKind::BigUInt64 => Intrinsic::BigUInt64ArrayConstructor,
        TypedArrayKind::Float16 => Intrinsic::Float16ArrayConstructor,
        TypedArrayKind::Float32 => Intrinsic::Float32ArrayConstructor,
        TypedArrayKind::Float64 => Intrinsic::Float64ArrayConstructor,
    }
}

/// Create a typed array of `kind` viewing `buffer` at `byte_offset`, with an optional element
/// length (None views the rest of the buffer).
pub fn create_typed_array(
    cx: Context,
    realm: Handle<Realm>,
    kind: TypedArrayKind,
    buffer: Handle<ArrayBufferObject>,
    byte_offset: usize,
    length: Option<usize>,
) -> EvalResult<Handle<ObjectValue>> {
    with_realm(cx, realm, |cx| {
        let constructor = realm.get_intrinsic(typed_array_constructor_intrinsic(kind));
        let length = match length {
            Some(length) => Value::number(length as f64).to_handle(cx),
            None => cx.undefined(),
        };
        let byte_offset = Value::number(byte_offset as f64).to_handle(cx);
        let arguments = [buffer.as_value(), byte_offset, length];
        construct_object(cx, constructor, &arguments, Some(constructor))
    })
}

/// Whether a value is a typed array object.
pub fn is_typed_array(value: Handle<Value>) -> bool {
    value.is_object() && value.as_object().is_typed_array()
}

/// The kind of a typed array object, or None if the object is not a typed array.
pub fn typed_array_kind(array: Handle<ObjectValue>) -> Option<TypedArrayKind> {
    if array.is_typed_array() {
        Some(array.as_typed_array().kind())
    } else {
        None
    }
}

/// The element length of a typed array, or None if it is not a typed array or has auto length.
pub fn typed_array_element_length(array: Handle<ObjectValue>) -> Option<usize> {
    if array.is_typed_array() {
        array.as_typed_array().array_length()
    } else {
        None
    }
}

/// The byte offset of a typed array within its buffer, or None if it is not a typed array.
pub fn typed_array_byte_offset(array: Handle<ObjectValue>) -> Option<usize> {
    if array.is_typed_array() {
        Some(array.as_typed_array().byte_offset())
    } else {
        None
    }
}

/// The byte length of a typed array, or None if it is not a typed array or has auto length.
pub fn typed_array_byte_length(array: Handle<ObjectValue>) -> Option<usize> {
    if array.is_typed_array() {
        array.as_typed_array().byte_length()
    } else {
        None
    }
}

/// A pointer to the typed array's data within its buffer, or None if it is not a typed array, its
/// buffer is detached, or it is out of bounds (a resizable buffer shrank under it). Valid under the
/// contract of `array_buffer_data_ptr`.
pub fn typed_array_data_ptr(array: Handle<ObjectValue>) -> Option<*mut u8> {
    if !array.is_typed_array() {
        return None;
    }

    let record = make_typed_array_with_buffer_witness_record(array.as_typed_array());
    if is_typed_array_out_of_bounds(&record) {
        return None;
    }

    Some(array.as_typed_array().data().as_ptr() as *mut u8)
}

// --- Promises ---

pub use crate::runtime::promise_object::PromiseObject;

/// The settlement state of a promise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromiseStatus {
    Pending,
    Fulfilled,
    Rejected,
}

/// Create a new pending promise in a realm.
pub fn new_promise(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<PromiseObject>> {
    with_realm(cx, realm, |cx| Ok(PromiseObject::new_pending(cx)?.to_handle()))
}

/// Resolve a promise with a value (the promise resolve function semantics).
pub fn promise_resolve(
    cx: Context,
    promise: Handle<PromiseObject>,
    value: Handle<Value>,
) -> EvalResult<()> {
    promise_object::resolve(cx, promise, value)
}

/// Reject a promise with a reason, as SpiderMonkey's `JS::RejectPromise` (`PromiseObject::reject`) does: a promise that is
/// already settled, or already resolved to another promise or thenable, is left alone (its default reject function would
/// do nothing). Returns whether the promise was rejected.
pub fn promise_reject(
    cx: Context,
    mut promise: Handle<PromiseObject>,
    value: Handle<Value>,
) -> bool {
    match &promise.state {
        PromiseState::Pending { already_resolved: false, .. } => {
            promise.reject(cx, *value);
            true
        }
        _ => false,
    }
}

/// The settlement state of a promise.
pub fn promise_state(promise: Handle<PromiseObject>) -> PromiseStatus {
    match &promise.state {
        PromiseState::Pending { .. } => PromiseStatus::Pending,
        PromiseState::Fulfilled { .. } => PromiseStatus::Fulfilled,
        PromiseState::Rejected { .. } => PromiseStatus::Rejected,
    }
}

/// The result value of a settled promise, or None while pending.
pub fn promise_result(cx: Context, promise: Handle<PromiseObject>) -> Option<Handle<Value>> {
    match &promise.state {
        PromiseState::Fulfilled { result } | PromiseState::Rejected { result } => {
            Some(result.to_handle(cx))
        }
        PromiseState::Pending { .. } => None,
    }
}

/// Whether a value is a promise object.
pub fn is_promise(value: Handle<Value>) -> bool {
    value.is_object() && value.as_object().is::<PromiseObject>()
}

/// The promise object of a value, if it is one.
pub fn as_promise(value: Handle<Value>) -> Option<Handle<PromiseObject>> {
    if value.is_object() {
        value.as_object().as_opt::<PromiseObject>()
    } else {
        None
    }
}

// --- Errors ---

pub use crate::runtime::intrinsics::error_object::ErrorObject;

/// Create an error object of the given kind with a message in a realm.
fn new_error_of_kind(
    cx: Context,
    realm: Handle<Realm>,
    constructor: Intrinsic,
    message: &str,
) -> EvalResult<Handle<ObjectValue>> {
    with_realm(cx, realm, |mut cx| {
        let constructor = realm.get_intrinsic(constructor);
        let message = cx.alloc_string(message)?.as_value();
        construct_object(cx, constructor, &[message], Some(constructor))
    })
}

/// Create an Error object with a message in a realm.
pub fn new_error(
    cx: Context,
    realm: Handle<Realm>,
    message: &str,
) -> EvalResult<Handle<ObjectValue>> {
    new_error_of_kind(cx, realm, Intrinsic::ErrorConstructor, message)
}

/// Create a TypeError object with a message in a realm.
pub fn new_type_error(
    cx: Context,
    realm: Handle<Realm>,
    message: &str,
) -> EvalResult<Handle<ObjectValue>> {
    new_error_of_kind(cx, realm, Intrinsic::TypeErrorConstructor, message)
}

/// Create a RangeError object with a message in a realm.
pub fn new_range_error(
    cx: Context,
    realm: Handle<Realm>,
    message: &str,
) -> EvalResult<Handle<ObjectValue>> {
    new_error_of_kind(cx, realm, Intrinsic::RangeErrorConstructor, message)
}

/// Create a SyntaxError object with a message in a realm.
pub fn new_syntax_error(
    cx: Context,
    realm: Handle<Realm>,
    message: &str,
) -> EvalResult<Handle<ObjectValue>> {
    new_error_of_kind(cx, realm, Intrinsic::SyntaxErrorConstructor, message)
}

/// Create an Error object with a message plus `fileName` and `lineNumber` data properties.
pub fn new_error_with_location(
    cx: Context,
    realm: Handle<Realm>,
    message: &str,
    file_name: &str,
    line: u32,
) -> EvalResult<Handle<ObjectValue>> {
    let error = new_error(cx, realm, message)?;
    with_realm(cx, realm, |mut cx| {
        let file_name_value = cx.alloc_string(file_name)?.as_value();
        let file_name_key = PropertyKey::string_handle(cx, cx.alloc_string("fileName")?)?;
        create_data_property(cx, error, file_name_key, file_name_value)?;

        let line_key = PropertyKey::string_handle(cx, cx.alloc_string("lineNumber")?)?;
        create_data_property(cx, error, line_key, Value::number(line as f64).to_handle(cx))?;
        Ok(error)
    })
}

/// Read an error object's `message` property as a string, or None if it has none.
pub fn error_message(
    cx: Context,
    error: Handle<ObjectValue>,
) -> EvalResult<Option<Handle<StringValue>>> {
    let message = get(cx, error, cx.names.message())?;
    if message.is_undefined() {
        return Ok(None);
    }
    Ok(Some(to_string(cx, message)?))
}

// --- Function helpers ---

/// The `name` property of a function as a string, or None if it has none.
pub fn function_name(
    cx: Context,
    function: Handle<ObjectValue>,
) -> EvalResult<Option<Handle<StringValue>>> {
    let name = get(cx, function, cx.names.name())?;
    if name.is_undefined() {
        return Ok(None);
    }
    Ok(Some(to_string(cx, name)?))
}

/// The `length` property of a function as a number, or None if it has none.
pub fn function_length(cx: Context, function: Handle<ObjectValue>) -> EvalResult<Option<f64>> {
    let length = get(cx, function, cx.names.length())?;
    if length.is_undefined() {
        return Ok(None);
    }
    Ok(Some(to_number(cx, length)?.as_number()))
}

/// Create an ordinary object with Object.prototype in the current realm.
pub fn new_object(cx: Context) -> AllocResult<Handle<ObjectValue>> {
    ordinary_object_create(cx)
}

/// The [[StringData]] of a String exotic object, or None if the object is not one (ECMA-262 10.5).
/// The engine's object kind answers the question, not the prototype: `String.prototype`, a
/// `new String(s)` whose prototype was replaced and a String object of another realm all have the
/// slot, while an ordinary object with the same visible properties does not.
pub fn string_object_data(object: Handle<ObjectValue>) -> Option<Handle<StringValue>> {
    let string = object.as_opt::<StringObject>()?;
    Some(string.string_data())
}

// --- Running code (re-exported from `embed`) ---

pub use super::{call, compile_function, construct, evaluate_script, global_of_realm};

/// [[DefineOwnProperty]] with a full or partial descriptor, returning whether it succeeded.
pub fn define_property(
    cx: Context,
    mut object: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
    desc: PropertyDescriptor,
) -> EvalResult<bool> {
    object.define_own_property(cx, key, desc)
}

/// [[Delete]], returning whether it succeeded.
pub fn delete_property(
    cx: Context,
    mut object: Handle<ObjectValue>,
    key: Handle<PropertyKey>,
) -> EvalResult<bool> {
    object.delete(cx, key)
}

/// A stable identity hash of an object (does not change when the object moves).
pub fn object_identity_hash(mut object: HeapPtr<ObjectValue>) -> u32 {
    object.hash_code().get()
}

// --- Intrinsics ---

pub use crate::runtime::intrinsics::intrinsics::Intrinsic;

/// An intrinsic object of a realm, e.g. `Intrinsic::ObjectPrototype` or
/// `Intrinsic::FunctionPrototype`.
pub fn intrinsic(realm: Handle<Realm>, intrinsic: Intrinsic) -> Handle<ObjectValue> {
    realm.get_intrinsic(intrinsic)
}

// --- More strings ---

/// Flatten a string (a rope is copied into one buffer, which may allocate and GC).
pub fn flatten_string(string: Handle<StringValue>) -> AllocResult<Handle<FlatString>> {
    string.flatten_to_handle()
}

/// The interned (atomized) string with the same contents. Equal atoms are the same pointer.
pub fn atomize(cx: Context, string: Handle<StringValue>) -> AllocResult<Handle<FlatString>> {
    let flat_string = string.flatten()?;
    Ok(InternedStrings::get(cx, flat_string)?.to_handle())
}

/// Atomize a Rust string.
pub fn atomize_str(mut cx: Context, string: &str) -> EvalResult<Handle<FlatString>> {
    let flat_string = cx.alloc_string_ptr(string)?;
    Ok(InternedStrings::get(cx, flat_string)?.to_handle())
}

/// A property key for a string. Array index strings become array index keys.
pub fn property_key_from_atom(
    cx: Context,
    string: Handle<FlatString>,
) -> AllocResult<Handle<PropertyKey>> {
    PropertyKey::string_handle(cx, string.as_string())
}

/// Call `f` with the code units of a string without allocating on the JS heap, so no GC can run
/// and raw heap pointers held by the caller stay valid. Flat strings (and already flattened
/// ropes) are borrowed; unflattened ropes are copied into a Rust buffer by walking the tree.
///
/// # Safety
/// `string` must point to a live string, and `f` must not allocate on the JS heap or run a GC (it
/// may get a borrow of the string's storage, which a GC moves or frees).
pub unsafe fn with_string_chars_no_gc<R>(
    string: HeapPtr<StringValue>,
    f: impl FnOnce(StringChars<'_>) -> R,
) -> R {
    if let Some(flat_string) = string.as_flat_no_gc() {
        // SAFETY: The slice is only used by `f`, which the caller guarantees does not allocate on
        // the JS heap or run a GC.
        return f(unsafe { flat_string_chars(flat_string) });
    }

    match string.width() {
        StringWidth::OneByte => {
            let mut bytes = Vec::with_capacity(string.len() as usize);
            // SAFETY: Each piece is copied into the Rust buffer at once; walking the rope does not
            // allocate on the JS heap.
            string.for_each_flat_piece_no_gc(|piece| match unsafe { flat_string_chars(piece) } {
                StringChars::Latin1(piece_bytes) => bytes.extend_from_slice(piece_bytes),
                StringChars::TwoByte(units) => bytes.extend(units.iter().map(|unit| *unit as u8)),
            });
            f(StringChars::Latin1(&bytes))
        }
        StringWidth::TwoByte => {
            let mut units = Vec::with_capacity(string.len() as usize);
            // SAFETY: As above.
            string.for_each_flat_piece_no_gc(|piece| match unsafe { flat_string_chars(piece) } {
                StringChars::Latin1(bytes) => units.extend(bytes.iter().map(|byte| *byte as u16)),
                StringChars::TwoByte(piece_units) => units.extend_from_slice(piece_units),
            });
            f(StringChars::TwoByte(&units))
        }
    }
}

// --- More symbols ---

/// Well-known symbols, numbered like SpiderMonkey's `JS::SymbolCode`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum WellKnownSymbol {
    IsConcatSpreadable = 0,
    Iterator,
    Match,
    Replace,
    Search,
    Species,
    HasInstance,
    Split,
    ToPrimitive,
    ToStringTag,
    Unscopables,
    AsyncIterator,
    MatchAll,
}

impl WellKnownSymbol {
    /// The well-known symbol with this code, if any.
    pub fn from_code(code: u32) -> Option<WellKnownSymbol> {
        use WellKnownSymbol::*;
        const ALL: [WellKnownSymbol; 13] = [
            IsConcatSpreadable,
            Iterator,
            Match,
            Replace,
            Search,
            Species,
            HasInstance,
            Split,
            ToPrimitive,
            ToStringTag,
            Unscopables,
            AsyncIterator,
            MatchAll,
        ];
        ALL.get(code as usize).copied()
    }
}

/// A well-known symbol of the context.
pub fn well_known_symbol(cx: Context, symbol: WellKnownSymbol) -> Handle<SymbolValue> {
    let symbols = &cx.symbols;
    let key = match symbol {
        WellKnownSymbol::IsConcatSpreadable => symbols.is_concat_spreadable(),
        WellKnownSymbol::Iterator => symbols.iterator(),
        WellKnownSymbol::Match => symbols.match_(),
        WellKnownSymbol::Replace => symbols.replace(),
        WellKnownSymbol::Search => symbols.search(),
        WellKnownSymbol::Species => symbols.species(),
        WellKnownSymbol::HasInstance => symbols.has_instance(),
        WellKnownSymbol::Split => symbols.split(),
        WellKnownSymbol::ToPrimitive => symbols.to_primitive(),
        WellKnownSymbol::ToStringTag => symbols.to_string_tag(),
        WellKnownSymbol::Unscopables => symbols.unscopables(),
        WellKnownSymbol::AsyncIterator => symbols.async_iterator(),
        WellKnownSymbol::MatchAll => symbols.match_all(),
    };
    key.as_symbol().to_handle()
}

/// `Symbol.keyFor`: the registry key of a symbol created with `Symbol.for`, if any.
pub fn symbol_key_for(cx: Context, symbol: Handle<SymbolValue>) -> Option<Handle<FlatString>> {
    cx.global_symbol_registry()
        .iter_gc_unsafe()
        .find(|(_, registered)| registered.ptr_eq(&*symbol))
        .map(|(key, _)| key.to_handle())
}

// --- More BigInt ---

/// The digits of a BigInt in a radix (2 to 36).
pub fn bigint_to_string(
    mut cx: Context,
    bigint: Handle<BigIntValue>,
    radix: u32,
) -> EvalResult<Handle<StringValue>> {
    let text = bigint.bigint().to_str_radix(radix);
    cx.alloc_string(&text)
}

// --- More ArrayBuffer, DataView ---

pub use crate::runtime::intrinsics::data_view_object::DataViewObject;

/// Detach an ArrayBuffer, dropping its contents (for an external buffer: dropping the reference to
/// its memory). Returns false, changing nothing, if the buffer has the `WasmMemory` detach key: only
/// its memory may detach it, and callers then throw a TypeError as the spec's DetachArrayBuffer
/// does on a key mismatch.
pub fn array_buffer_detach(mut buffer: Handle<ArrayBufferObject>) -> bool {
    if buffer.detach_key() == DetachKey::WasmMemory {
        return false;
    }

    buffer.detach();
    true
}

/// Create a DataView over `buffer` at `byte_offset`, with an optional byte length (None views the
/// rest of the buffer).
pub fn create_data_view(
    cx: Context,
    realm: Handle<Realm>,
    buffer: Handle<ArrayBufferObject>,
    byte_offset: usize,
    byte_length: Option<usize>,
) -> EvalResult<Handle<ObjectValue>> {
    with_realm(cx, realm, |cx| {
        let constructor = realm.get_intrinsic(Intrinsic::DataViewConstructor);
        let byte_length = match byte_length {
            Some(length) => Value::number(length as f64).to_handle(cx),
            None => cx.undefined(),
        };
        let byte_offset = Value::number(byte_offset as f64).to_handle(cx);
        construct_object(cx, constructor, &[buffer.as_value(), byte_offset, byte_length], None)
    })
}

/// The DataView of an object, if it is one.
pub fn as_data_view(object: Handle<ObjectValue>) -> Option<Handle<DataViewObject>> {
    object.as_opt::<DataViewObject>()
}

/// The byte offset of a DataView within its buffer.
pub fn data_view_byte_offset(view: Handle<DataViewObject>) -> usize {
    view.byte_offset()
}

/// The byte length of a DataView. A length-tracking view reports the rest of its buffer.
pub fn data_view_byte_length(view: Handle<DataViewObject>) -> usize {
    match view.byte_length() {
        Some(length) => length,
        None => view
            .viewed_array_buffer()
            .byte_length()
            .saturating_sub(view.byte_offset()),
    }
}

/// A pointer to a DataView's first byte, or None if its buffer is detached. Valid under the
/// contract of `array_buffer_data_ptr`.
pub fn data_view_data_ptr(view: Handle<DataViewObject>) -> Option<*mut u8> {
    let buffer = view.viewed_array_buffer();
    let data = array_buffer_data_ptr(buffer)?;
    // SAFETY: The byte offset of a DataView is within its buffer.
    Some(unsafe { data.add(view.byte_offset()) })
}

// --- More promises ---

/// `promise.then(on_fulfilled, on_rejected)`, returning the derived promise.
pub fn promise_then(
    cx: Context,
    promise: Handle<PromiseObject>,
    on_fulfilled: Option<Handle<ObjectValue>>,
    on_rejected: Option<Handle<ObjectValue>>,
) -> EvalResult<Handle<Value>> {
    let realm = super::realm_of_object(cx, *promise.as_object()).to_handle();
    with_realm(cx, realm, |cx| {
        let then = get(cx, promise.as_object(), cx.names.then())?;
        let on_fulfilled = on_fulfilled.map_or(cx.undefined(), |f| f.as_value());
        let on_rejected = on_rejected.map_or(cx.undefined(), |f| f.as_value());
        super::call(cx, then, promise.as_value(), &[on_fulfilled, on_rejected])
    })
}

// --- More errors ---

/// Create an Error with a message plus `fileName`, `lineNumber` and `columnNumber` properties.
pub fn new_error_with_position(
    cx: Context,
    realm: Handle<Realm>,
    message: &str,
    file_name: &str,
    line: u32,
    column: u32,
) -> EvalResult<Handle<ObjectValue>> {
    let error = new_error_with_location(cx, realm, message, file_name, line)?;
    let mut cx = cx;
    let column_key = PropertyKey::string_handle(cx, cx.alloc_string("columnNumber")?)?;
    create_data_property(cx, error, column_key, Value::number(column as f64).to_handle(cx))?;
    Ok(error)
}

/// The `stack` property of an error as a string, if it has one.
pub fn error_stack(
    mut cx: Context,
    error: Handle<ObjectValue>,
) -> EvalResult<Option<Handle<StringValue>>> {
    let key = PropertyKey::string_handle(cx, cx.alloc_string("stack")?)?;
    let stack = get(cx, error, key)?;
    if stack.is_undefined() {
        return Ok(None);
    }
    Ok(Some(to_string(cx, stack)?))
}

// --- JSON ---

fn call_json_function(
    cx: Context,
    realm: Handle<Realm>,
    name: &str,
    arguments: &[Handle<Value>],
) -> EvalResult<Handle<Value>> {
    with_realm(cx, realm, |mut cx| {
        let json = realm.get_intrinsic(Intrinsic::JSON);
        let key = PropertyKey::string_handle(cx, cx.alloc_string(name)?)?;
        let function = get(cx, json, key)?;
        super::call(cx, function, json.as_value(), arguments)
    })
}

/// `JSON.parse(text)` in a realm.
pub fn json_parse(cx: Context, realm: Handle<Realm>, text: &str) -> EvalResult<Handle<Value>> {
    let mut cx = cx;
    let text = cx.alloc_string(text)?.as_value();
    call_json_function(cx, realm, "parse", &[text])
}

/// `JSON.stringify(value)` in a realm. Undefined when the value has no JSON representation.
pub fn json_stringify(
    cx: Context,
    realm: Handle<Realm>,
    value: Handle<Value>,
) -> EvalResult<Handle<Value>> {
    call_json_function(cx, realm, "stringify", &[value])
}

// --- Dates ---

pub use crate::runtime::intrinsics::date_object::DateObject;

/// A Date for a time in milliseconds since the epoch (clipped like `new Date(ms)`).
pub fn new_date(cx: Context, realm: Handle<Realm>, ms: f64) -> EvalResult<Handle<ObjectValue>> {
    with_realm(cx, realm, |cx| {
        let constructor = realm.get_intrinsic(Intrinsic::DateConstructor);
        let date = DateObject::new_from_constructor(cx, constructor, time_clip(ms))?;
        Ok(date.as_object().to_handle())
    })
}

/// The time value of a Date in milliseconds (NaN for an invalid date), or None if not a Date.
pub fn date_value(object: Handle<ObjectValue>) -> Option<f64> {
    object.as_opt::<DateObject>().map(|date| date.date_value())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        ContextBuilder,
        embed::{evaluate_script, with_realm},
    };

    #[test]
    fn api_property_operations() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            let realm = cx.initial_realm();
            let mut object = evaluate_script(cx, realm, "({ a: 1 })", "test.js", 1)
                .unwrap()
                .as_object();

            let key = property_key_from_string(cx, "a").unwrap();
            let value = get(cx, object, key).unwrap();
            assert_eq!(value.as_number(), 1.0);

            let two = cx.smi(2);
            assert!(set_property(cx, object, key, two, object.as_value()).unwrap());
            assert_eq!(get(cx, object, key).unwrap().as_number(), 2.0);

            let b_key = property_key_from_string(cx, "b").unwrap();
            assert!(create_data_property(cx, object, b_key, cx.smi(3)).unwrap());
            assert!(has_property(cx, object, b_key).unwrap());
            assert!(has_own_property(cx, object, b_key).unwrap());

            let desc = get_own_property_descriptor(cx, object, b_key)
                .unwrap()
                .unwrap();
            assert_eq!(desc.value.unwrap().as_number(), 3.0);
            assert!(desc.is_writable.unwrap());

            let keys = own_property_keys(cx, object).unwrap();
            assert_eq!(keys.len(), 2);

            assert!(object.delete(cx, b_key).unwrap());
            assert!(!has_own_property(cx, object, b_key).unwrap());

            assert!(get_prototype_of(cx, object).unwrap().is_some());
            assert!(is_extensible(cx, object).unwrap());
            assert!(prevent_extensions(cx, object).unwrap());
            assert!(!is_extensible(cx, object).unwrap());
        });
        cx.drop();
    }

    #[test]
    fn api_strings() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            let latin1 = string_from_latin1(cx, b"hello").unwrap();
            assert_eq!(string_length(latin1.as_string()), 5);
            assert!(string_is_one_byte(latin1.as_string()));
            // SAFETY: The slice is compared at once, nothing allocates while it is used.
            match unsafe { flat_string_chars(*latin1) } {
                StringChars::Latin1(bytes) => assert_eq!(bytes, b"hello"),
                StringChars::TwoByte(_) => panic!("expected Latin-1"),
            }
            assert_eq!(string_to_rust_string(latin1.as_string()).unwrap(), "hello");

            // Euro sign is not Latin-1, so this is a two-byte string
            let utf16 = string_from_code_units(cx, &[0x41, 0x20AC]).unwrap();
            assert!(!string_is_one_byte(utf16.as_string()));
            // SAFETY: As above.
            match unsafe { flat_string_chars(*utf16) } {
                StringChars::TwoByte(units) => assert_eq!(units, &[0x41, 0x20AC]),
                StringChars::Latin1(_) => panic!("expected two-byte"),
            }
            assert_eq!(string_to_rust_string(utf16.as_string()).unwrap(), "A\u{20AC}");

            let utf8 = string_from_utf8(cx, "h\u{00E9}llo\u{20AC}").unwrap();
            assert!(!string_is_one_byte(utf8.as_string()));
            assert_eq!(string_to_rust_string(utf8.as_string()).unwrap(), "h\u{00E9}llo\u{20AC}");
        });
        cx.drop();
    }

    #[test]
    fn api_string_object_data() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            let data = |source: &str| {
                let object = eval_in(cx, source).as_object();
                string_object_data(object).map(|s| string_to_rust_string(s).unwrap())
            };
            assert_eq!(data("new String('ab')").as_deref(), Some("ab"));
            // The internal slot is what answers, not the prototype: String.prototype is a String
            // object holding the empty string, and reparenting a String object keeps its data.
            assert_eq!(data("String.prototype").as_deref(), Some(""));
            assert_eq!(
                data("Object.setPrototypeOf(new String('cd'), null)").as_deref(),
                Some("cd")
            );
            assert_eq!(
                data("Object.setPrototypeOf(new String('ef'), Object.prototype)").as_deref(),
                Some("ef")
            );
            // Other kinds have no [[StringData]]: an ordinary object that looks like a string
            // indexed by property, and a symbol wrapper.
            assert_eq!(data("({ 0: 'a', 1: 'b', length: 2 })"), None);
            assert_eq!(data("Object(Symbol('s'))"), None);
        });
        cx.drop();
    }

    #[test]
    fn api_bigint() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            let from_i64 = bigint_from_i64(cx, -42).unwrap();
            assert_eq!(bigint_to_i64(from_i64).unwrap(), -42);

            let from_u64 = bigint_from_u64(cx, u64::MAX).unwrap();
            assert_eq!(bigint_to_u64(from_u64).unwrap(), u64::MAX);

            let from_f64 = bigint_from_f64(cx, 1024.0).unwrap().unwrap();
            assert_eq!(bigint_to_i64(from_f64).unwrap(), 1024);
            assert!(bigint_from_f64(cx, 1.5).unwrap().is_none());
            assert!(bigint_from_f64(cx, f64::NAN).unwrap().is_none());

            let big = bigint_from_u64(cx, u64::MAX).unwrap();
            assert!(bigint_to_f64(big).unwrap() > 1e19);
        });
        cx.drop();
    }

    #[test]
    fn api_arrays() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            let realm = cx.initial_realm();
            let elements = [cx.smi(10), cx.smi(20), cx.smi(30)];
            let array = create_array_from_list(cx, &elements).unwrap();
            let array_object = array.as_object();
            assert!(is_array(cx, array_object.as_value()).unwrap());
            assert_eq!(length_of_array_like(cx, array_object).unwrap(), 3);

            let empty = array_with_length(cx, realm, 5).unwrap();
            assert_eq!(length_of_array_like(cx, empty.as_object()).unwrap(), 5);
        });
        cx.drop();
    }

    #[test]
    fn api_array_buffer_and_typed_array() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            let realm = cx.initial_realm();
            let buffer = array_buffer_new(cx, realm, 4).unwrap();
            assert_eq!(array_buffer_byte_length(buffer), 4);
            assert!(!array_buffer_is_detached(buffer));

            let ptr = array_buffer_data_ptr(buffer).unwrap();
            // SAFETY: `ptr` points to 4 writable bytes of the buffer's backing data.
            unsafe { std::ptr::write(ptr, 42) };

            let view =
                create_typed_array(cx, realm, TypedArrayKind::UInt8, buffer, 0, Some(4)).unwrap();
            assert!(is_typed_array(view.as_value()));
            assert!(matches!(typed_array_kind(view), Some(TypedArrayKind::UInt8)));
            assert_eq!(typed_array_element_length(view), Some(4));
            assert_eq!(typed_array_byte_offset(view), Some(0));
            assert_eq!(typed_array_byte_length(view), Some(4));

            let data_ptr = typed_array_data_ptr(view).unwrap();
            // SAFETY: `data_ptr` points to the first of 4 readable bytes of the view.
            assert_eq!(unsafe { std::ptr::read(data_ptr) }, 42);
        });
        cx.drop();
    }

    #[test]
    fn api_promise() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            let realm = cx.initial_realm();
            let promise = new_promise(cx, realm).unwrap();
            assert_eq!(promise_state(promise), PromiseStatus::Pending);
            assert!(promise_result(cx, promise).is_none());
            assert!(is_promise(promise.as_value()));

            promise_resolve(cx, promise, cx.smi(7)).unwrap();
            assert_eq!(promise_state(promise), PromiseStatus::Fulfilled);
            assert_eq!(promise_result(cx, promise).unwrap().as_number(), 7.0);
        });
        cx.drop();
    }

    #[test]
    fn api_errors() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            let realm = cx.initial_realm();
            let error = new_type_error(cx, realm, "boom").unwrap();
            let message = error_message(cx, error).unwrap().unwrap();
            assert_eq!(string_to_rust_string(message).unwrap(), "boom");

            let located = new_error_with_location(cx, realm, "bad", "file.js", 12).unwrap();
            let file_key = property_key_from_string(cx, "fileName").unwrap();
            let file = get(cx, located, file_key).unwrap();
            assert_eq!(string_to_rust_string(file.as_string()).unwrap(), "file.js");
            let line_key = property_key_from_string(cx, "lineNumber").unwrap();
            let line = get(cx, located, line_key).unwrap();
            assert_eq!(line.as_number(), 12.0);
        });
        cx.drop();
    }

    fn eval_in(cx: Context, source: &str) -> Handle<Value> {
        evaluate_script(cx, cx.initial_realm(), source, "api.js", 1).unwrap()
    }

    fn rust_string(cx: Context, value: Handle<Value>) -> String {
        string_to_rust_string(to_string(cx, value).unwrap()).unwrap()
    }

    #[test]
    fn api_running_code_and_definitions() {
        let cx = ContextBuilder::new().build().unwrap();
        let realm = cx.initial_realm();
        with_realm(cx, realm, |cx| {
            let function =
                compile_function(cx, realm, Some("twice"), &["x"], "return x * 2;", "f.js", 1)
                    .unwrap();
            let result = call(cx, function.as_value(), cx.undefined(), &[cx.smi(21)]).unwrap();
            assert_eq!(result.as_number(), 42.0);

            let ctor = eval_in(cx, "(function Point(x) { this.x = x; })");
            let point = construct(cx, ctor, &[cx.smi(3)], None).unwrap();
            let x_key = property_key_from_string(cx, "x").unwrap();
            assert_eq!(get(cx, point, x_key).unwrap().as_number(), 3.0);

            // Accessor descriptors come back complete
            let object = eval_in(cx, "({ get g() { return 1; } })").as_object();
            let g_key = property_key_from_string(cx, "g").unwrap();
            let desc = get_own_property_descriptor(cx, object, g_key)
                .unwrap()
                .unwrap();
            assert!(desc.get.is_some() && desc.set.is_none());
            assert_eq!(desc.is_enumerable, Some(true));
            assert_eq!(desc.is_configurable, Some(true));
            assert!(desc.value.is_none());

            let frozen = PropertyDescriptor::data(cx.smi(5), PropertyFlags::empty());
            let k_key = property_key_from_string(cx, "k").unwrap();
            assert!(define_property(cx, object, k_key, frozen).unwrap());
            let desc = get_own_property_descriptor(cx, object, k_key)
                .unwrap()
                .unwrap();
            assert_eq!(desc.is_writable, Some(false));
            assert!(!delete_property(cx, object, k_key).unwrap());

            let hash = object_identity_hash(*object);
            crate::runtime::embed::gc(cx);
            assert_eq!(object_identity_hash(*object), hash);

            let proto = intrinsic(realm, Intrinsic::ObjectPrototype);
            assert!(
                get_prototype_of(cx, object)
                    .unwrap()
                    .unwrap()
                    .ptr_eq(&proto)
            );
            assert!(global_of_realm(*realm).ptr_eq(&realm.global_object().as_object()));
            assert!(to_boolean(*cx.smi(1)));
            assert_eq!(to_number(cx, eval_in(cx, "'12'")).unwrap().as_number(), 12.0);
            assert!(to_object(cx, cx.smi(1)).unwrap().is_callable() == false);
            let key = to_property_key(cx, cx.smi(7)).unwrap();
            assert!(key.is_array_index());
        });
        cx.drop();
    }

    #[test]
    fn api_atoms_symbols_bigint_string() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            let a = atomize_str(cx, "hello").unwrap();
            let b = atomize(cx, string_from_utf8(cx, "hello").unwrap().as_string()).unwrap();
            assert!(a.ptr_eq(&b));
            let key = property_key_from_atom(cx, a).unwrap();
            assert!(key.is_string());

            let rope = eval_in(cx, "var r = 'ab'; r += 'cd'; r");
            let flat = flatten_string(rope.as_string()).unwrap();
            assert_eq!(string_to_rust_string(flat.as_string()).unwrap(), "abcd");

            let iterator = well_known_symbol(cx, WellKnownSymbol::Iterator);
            let js_iterator = eval_in(cx, "Symbol.iterator");
            assert!(iterator.ptr_eq(&js_iterator.as_symbol()));
            assert_eq!(WellKnownSymbol::from_code(12), Some(WellKnownSymbol::MatchAll));
            assert_eq!(WellKnownSymbol::from_code(13), None);
            let description = symbol_description(iterator).unwrap();
            assert_eq!(string_to_rust_string(description.as_string()).unwrap(), "Symbol.iterator");

            let registered = symbol_for(cx, "app").unwrap();
            assert!(registered.ptr_eq(&symbol_for(cx, "app").unwrap()));
            let js_registered = eval_in(cx, "Symbol.for('app')");
            assert!(registered.ptr_eq(&js_registered.as_symbol()));
            let key_for = symbol_key_for(cx, registered).unwrap();
            assert_eq!(string_to_rust_string(key_for.as_string()).unwrap(), "app");
            assert!(symbol_key_for(cx, new_symbol(cx, Some("x")).unwrap()).is_none());

            let big = bigint_from_i64(cx, -255).unwrap();
            let text = bigint_to_string(cx, big, 16).unwrap();
            assert_eq!(string_to_rust_string(text).unwrap(), "-ff");
        });
        cx.drop();
    }

    #[test]
    fn api_buffers_data_views_dates_json_promises_errors() {
        let cx = ContextBuilder::new().build().unwrap();
        let realm = cx.initial_realm();
        with_realm(cx, realm, |cx| {
            let buffer = array_buffer_new(cx, realm, 16).unwrap();
            let view = create_data_view(cx, realm, buffer, 4, Some(8)).unwrap();
            let view = as_data_view(view).unwrap();
            assert_eq!(data_view_byte_offset(view), 4);
            assert_eq!(data_view_byte_length(view), 8);
            let view_ptr = data_view_data_ptr(view).unwrap();
            let buffer_ptr = array_buffer_data_ptr(buffer).unwrap();
            assert_eq!(view_ptr as usize - buffer_ptr as usize, 4);

            array_buffer_detach(buffer);
            assert!(array_buffer_is_detached(buffer));
            assert!(data_view_data_ptr(view).is_none());

            let date = new_date(cx, realm, 86_400_000.0).unwrap();
            assert_eq!(date_value(date), Some(86_400_000.0));
            assert!(date_value(new_object(cx).unwrap()).is_none());

            let parsed = json_parse(cx, realm, r#"{"a":[1,2]}"#).unwrap();
            let text = json_stringify(cx, realm, parsed).unwrap();
            assert_eq!(rust_string(cx, text), r#"{"a":[1,2]}"#);
            assert!(json_parse(cx, realm, "{").is_err());

            let promise = new_promise(cx, realm).unwrap();
            let on_fulfilled = eval_in(cx, "(function (v) { globalThis.seen = v; })").as_object();
            let derived = promise_then(cx, promise, Some(on_fulfilled), None).unwrap();
            assert!(is_promise(derived));
            promise_reject(cx, new_promise(cx, realm).unwrap(), cx.smi(1));
            promise_resolve(cx, promise, cx.smi(9)).unwrap();
            let mut cx = cx;
            cx.run_all_tasks().unwrap();
            assert_eq!(eval_in(cx, "seen").as_number(), 9.0);

            let error = new_error_with_position(cx, realm, "bad", "e.js", 3, 7).unwrap();
            let column_key = property_key_from_string(cx, "columnNumber").unwrap();
            assert_eq!(get(cx, error, column_key).unwrap().as_number(), 7.0);
            let stack = error_stack(cx, eval_in(cx, "new Error('s')").as_object()).unwrap();
            assert!(
                string_to_rust_string(stack.unwrap())
                    .unwrap()
                    .contains("api.js")
            );
        });
        cx.drop();
    }

    #[test]
    fn api_string_chars_without_gc_on_deep_rope() {
        let cx = ContextBuilder::new().build().unwrap();
        with_realm(cx, cx.initial_realm(), |cx| {
            // A 100k piece left-deep rope with one two-byte piece. With a GC on every allocation
            // building it would take quadratic time, so use fewer pieces there.
            #[cfg(not(feature = "gc_stress_test"))]
            const PIECES: usize = 100_000;
            #[cfg(feature = "gc_stress_test")]
            const PIECES: usize = 2_000;

            let mut rope = string_from_latin1(cx, b"a").unwrap().as_string();
            for index in 1..PIECES {
                crate::handle_scope_guard!(cx);
                let piece = if index == PIECES / 2 {
                    string_from_code_units(cx, &[0x20AC]).unwrap()
                } else {
                    string_from_latin1(cx, b"b").unwrap()
                };
                let concat = StringValue::concat(cx, rope, piece.as_string()).unwrap();
                rope.replace(*concat);
            }

            assert!(rope.as_flat_no_gc().is_none(), "rope was flattened");
            // SAFETY: The closures below only read the chars, they do not allocate on the JS heap.
            let (len, euro_index, last) = unsafe {
                with_string_chars_no_gc(*rope, |chars| match chars {
                    StringChars::TwoByte(units) => (
                        units.len(),
                        units.iter().position(|unit| *unit == 0x20AC),
                        *units.last().unwrap(),
                    ),
                    StringChars::Latin1(_) => panic!("expected two-byte chars"),
                })
            };
            assert_eq!(len, PIECES);
            assert_eq!(euro_index, Some(PIECES / 2));
            assert_eq!(last, b'b' as u16);

            // Still a rope afterwards, and flat strings are borrowed
            assert!(rope.as_flat_no_gc().is_none());
            let flat = string_from_latin1(cx, b"xyz").unwrap();
            // SAFETY: As above.
            let borrowed = unsafe {
                with_string_chars_no_gc(*flat.as_string(), |chars| match chars {
                    StringChars::Latin1(bytes) => bytes.as_ptr() == flat_latin1_ptr(*flat),
                    StringChars::TwoByte(_) => false,
                })
            };
            assert!(borrowed);

            // One-byte ropes stay one-byte
            let small = eval_in(cx, "var q = 'x'; q += 'yz'; q");
            // SAFETY: As above.
            unsafe {
                with_string_chars_no_gc(*small.as_string(), |chars| match chars {
                    StringChars::Latin1(bytes) => assert_eq!(bytes, b"xyz"),
                    StringChars::TwoByte(_) => panic!("expected Latin-1"),
                })
            };
        });
        cx.drop();
    }

    fn flat_latin1_ptr(string: HeapPtr<FlatString>) -> *const u8 {
        // SAFETY: Only the address is kept; the slice is dropped at once.
        match unsafe { flat_string_chars(string) } {
            StringChars::Latin1(bytes) => bytes.as_ptr(),
            StringChars::TwoByte(_) => std::ptr::null(),
        }
    }
}
