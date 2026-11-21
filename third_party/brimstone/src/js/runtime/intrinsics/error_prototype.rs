use crate::{
    common::error::FormatOptions,
    intrinsic_methods,
    runtime::{
        Context, Handle,
        abstract_operations::get,
        alloc_error::AllocResult,
        error::{err_cannot_set_property, type_error},
        intrinsic_builder::IntrinsicBuilder,
        intrinsics::{
            error_object::ErrorObject, intrinsics::Intrinsic, rust_runtime::RuntimeFunction,
        },
        object_value::ObjectValue,
        property_descriptor::PropertyDescriptor,
        realm::Realm,
        string_value::StringValue,
        to_console_string,
        type_utilities::to_string,
    },
    runtime_fn,
};

pub struct ErrorPrototype;

impl ErrorPrototype {
    /// Properties of the Error Prototype Object (https://tc39.es/ecma262/#sec-properties-of-the-error-prototype-object)
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        // Constructor property is added once ErrorConstructor has been created
        intrinsic_methods!(cx, builder, {
            to_string ErrorPrototype_to_string (0),
        });

        builder.data(cx.names.name(), cx.names.error().as_string().into())?;
        builder.data(cx.names.message(), cx.names.empty_string().as_string().into())?;
        builder.getter_setter(
            cx.names.stack(),
            RuntimeFunction::ErrorPrototype_get_stack,
            RuntimeFunction::ErrorPrototype_set_stack,
        )?;

        builder.build()
    }

    runtime_fn! {
    /// Error.prototype.toString (https://tc39.es/ecma262/#sec-error.prototype.tostring)
    fn to_string(cx, this_value, _) {
        if !this_value.is_object() {
            return type_error(cx, "Error.prototype.toString must be called on an object");
        }

        let this_object = this_value.as_object();

        let name_value = get(cx, this_object, cx.names.name())?;
        let name_string = if name_value.is_undefined() {
            cx.names.error().as_string()
        } else {
            to_string(cx, name_value)?
        };

        let message_value = get(cx, this_object, cx.names.message())?;
        let message_string = if message_value.is_undefined() {
            cx.names.empty_string().as_string()
        } else {
            to_string(cx, message_value)?
        };

        if name_string.is_empty() {
            Ok(message_string.as_value())
        } else if message_string.is_empty() {
            Ok(name_string.as_value())
        } else {
            let separator = cx.alloc_static_string(": ")?;
            Ok(StringValue::concat_all(cx, &[name_string, separator, message_string])?.as_value())
        }
    }}

    runtime_fn! {
    fn get_stack(cx, this_value, _) {
        // Check that `stack` getter was called on an error object
        let Some(mut error) = this_value.as_opt::<ErrorObject>() else {
            return Ok(cx.undefined());
        };

        // Stack trace starts with error message on one line
        let mut stack_trace = format_error_one_line(cx, error)?;
        stack_trace.push('\n');

        // Followed by the stack trace
        stack_trace.push_str(&error.get_stack_trace(cx)?.frames.to_string());

        Ok(cx.alloc_string(&stack_trace)?.as_value())
    }}

    runtime_fn! {
    fn set_stack(cx, this_value, arguments) {
        if !this_value.is_object() {
            return Ok(cx.undefined());
        }

        let key = cx.names.stack();
        let value = arguments.get(cx, 0);

        // Shadow the prototype accessor with an own data property, as engines that store the stack
        // trace per-error do. The assigned value is then read back as-is and the lazily generated
        // trace is no longer reachable.
        let mut object = this_value.as_object();
        let desc = PropertyDescriptor::non_enumerable_data(value);
        if !object.define_own_property(cx, key, desc)? && caller_is_strict(cx) {
            return err_cannot_set_property(cx, key.format()?);
        }

        Ok(cx.undefined())
    }}
}

/// Whether the JavaScript code that reached the current runtime function is strict, matching the
/// check the VM makes when it reports a failed property assignment. Frames of other runtime
/// functions (e.g. `Reflect.set` forwarding the store) are skipped, and a stack that holds no
/// JavaScript frame at all reports as not strict.
fn caller_is_strict(mut cx: Context) -> bool {
    let mut frame_opt = cx.vm().stack_frame().previous_frame();

    while let Some(frame) = frame_opt {
        let function = frame.closure().function_ptr();
        if function.runtime_function_id().is_none() {
            return function.is_strict();
        }

        frame_opt = frame.previous_frame();
    }

    false
}

/// Format an error object into a one line string containing name and message
fn format_error_one_line(cx: Context, error: Handle<ErrorObject>) -> AllocResult<String> {
    let name = error_name(cx, error);
    let name_str = name.format().unwrap_or_default();

    Ok(match error_message(cx, error)? {
        Some(message) => format!("{name_str}: {message}"),
        None => name_str,
    })
}

pub fn error_name(cx: Context, error: Handle<ErrorObject>) -> Handle<StringValue> {
    match get(cx, error.as_object(), cx.names.name()) {
        Ok(name_value) if name_value.is_string() => name_value.as_string(),
        _ => cx.names.error().as_string(),
    }
}

pub fn error_message(cx: Context, error: Handle<ErrorObject>) -> AllocResult<Option<String>> {
    match get(cx, error.as_object(), cx.names.message()) {
        Ok(message_value) => {
            Ok(Some(to_console_string(cx, message_value, &FormatOptions::default())?))
        }
        Err(_) => Ok(None),
    }
}
