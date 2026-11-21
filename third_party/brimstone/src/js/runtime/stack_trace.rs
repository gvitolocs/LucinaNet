use crate::{
    handle_scope_guard, impl_array_instance, must_a,
    parser::loc::find_line_col_for_pos,
    runtime::{
        Context, Handle, HeapPtr,
        alloc_error::AllocResult,
        bytecode::{function::BytecodeFunction, source_map::BytecodeSourceMap},
        collections::ArrayInstance,
        gc::{HeapItem, HeapVisitor},
        intrinsics::{error_object::CachedStackTraceInfo, rust_runtime::RuntimeFunction},
        source_file::SourceFile,
    },
};

pub struct HeapStackFrameInfo {
    /// The function executing in the stack frame.
    function: HeapPtr<BytecodeFunction>,
    /// Offset of the pc or saved return address in the bytecode. This will be used to report the
    /// source location in this stack frame.
    ///
    /// Set to 0 if there is no BytecodeSourceMap for this frame.
    bytecode_offset: usize,
}

struct StackFrameInfo {
    function: Handle<BytecodeFunction>,
    bytecode_offset: usize,
}

impl StackFrameInfo {
    fn to_heap(&self) -> HeapStackFrameInfo {
        HeapStackFrameInfo {
            function: *self.function,
            bytecode_offset: self.bytecode_offset,
        }
    }
}

/// Gather the stack frame information for each stack frame in the current stack.
fn gather_current_stack_frames(mut cx: Context, skip_current_frame: bool) -> Vec<StackFrameInfo> {
    let mut frames = vec![];

    // We may want to skip the first stack frame e.g. when in an error constructor
    let (mut stack_frame_opt, mut pc) = if skip_current_frame {
        let stack_frame = cx.vm().stack_frame();
        (stack_frame.previous_frame(), stack_frame.return_address())
    } else {
        (Some(cx.vm().stack_frame()), cx.vm().published_pc())
    };

    let stack_trace_top = cx.vm().stack_trace_top();

    while let Some(stack_frame) = stack_frame_opt {
        // Stop if we've reached the first stack frame that should not be included in the trace
        if Some(stack_frame) == stack_trace_top {
            break;
        }

        // Skip the last frame if it's a dummy frame for the realm
        if stack_frame.previous_frame().is_none() {
            let function = stack_frame.closure().function_ptr();
            if let Some(id) = function.runtime_function_id() {
                if id == RuntimeFunction::ReturnUndefined.to_id() {
                    break;
                }
            }
        }

        // Skip dummy realm frames pushed above other frames, e.g. when an embedder enters a
        // realm or runs a job while JS frames are active
        let closure = stack_frame.closure();
        if closure.ptr_eq(&closure.function_ptr().realm_ptr().empty_function_ptr()) {
            pc = stack_frame.return_address();
            stack_frame_opt = stack_frame.previous_frame();
            continue;
        }

        // Gather the function and bytecode offset if necessary
        let function = stack_frame.closure().function();
        let bytecode_offset = if function.source_map_ptr().is_some() {
            pc as usize - function.bytecode().as_ptr() as usize
        } else {
            0
        };

        frames.push(StackFrameInfo { function, bytecode_offset });

        // Move to the parent stack frame
        pc = stack_frame.return_address();
        stack_frame_opt = stack_frame.previous_frame();
    }

    frames
}

/// Create a StackFrameInfoArray for the current stack.
///
/// This contains the information necessary to construct a full stack trace for the error if the
/// `stack` getter is ever called.
pub fn create_current_stack_frame_info(
    cx: Context,
    skip_current_frame: bool,
) -> AllocResult<HeapPtr<StackFrameInfoArray>> {
    handle_scope_guard!(cx);

    let frames = gather_current_stack_frames(cx, skip_current_frame);

    let mut array = StackFrameInfoArray::new_uninit(cx, frames.len())?;

    for (i, frame) in frames.iter().enumerate() {
        array.as_mut_slice()[i] = frame.to_heap();
    }

    Ok(array)
}

/// Create the string representation of a stack trace for an error, given the stack frame info
/// cached from the time that the error was created.
pub fn create_stack_trace(
    mut cx: Context,
    stack_frame_info: Handle<StackFrameInfoArray>,
) -> AllocResult<CachedStackTraceInfo> {
    // Do any preparatory work that may allocate
    prepare_for_stack_trace(cx, stack_frame_info)?;

    let mut stack_trace = String::new();
    let mut first_source_file_line_col = None;

    for (i, stack_frame) in stack_frame_info.as_slice().iter().enumerate() {
        // Each line of the stack trace starts indented
        stack_trace.push_str("  at ");

        // Followed by the name of the function
        let func = stack_frame.function;
        if let Some(name) = func.name() {
            stack_trace.push_str(&name.format()?);
        } else {
            stack_trace.push_str("<anonymous>");
        }

        stack_trace.push_str(" (");

        // Followed by the file where the function was defined
        if let Some(file_name) = func.source_file_ptr().map(|file| file.display_name()) {
            stack_trace.push_str(&file_name.to_string());
        } else {
            stack_trace.push_str("<native>");
        }

        // If we have source code positions for this function, add the line and column
        if let Some(source_map) = func.source_map_ptr() {
            // Map that bytecode offset to a source position using the source map
            let source_position =
                BytecodeSourceMap::get_source_position(source_map, stack_frame.bytecode_offset);

            if let Some(source_position) = source_position {
                // Get the line and column number for the source position. Will not allocate since
                // line offsets were already generated in `prepare_for_stack_trace`.
                let source_file = func.source_file_ptr().unwrap();
                let line_offsets = source_file.line_offsets_ptr_raw().unwrap();
                let (line, column) = find_line_col_for_pos(
                    source_file.contents_as_slice(),
                    source_position,
                    line_offsets.as_slice(),
                );

                // Append the line and column to the function name
                stack_trace.push_str(&format!(":{line}:{column}"));

                // Save the first source position that is found
                if first_source_file_line_col.is_none() {
                    let source_file = func.source_file_ptr().unwrap().to_handle();
                    first_source_file_line_col = Some((source_file, line, column));
                }
            }
        }

        stack_trace.push(')');

        if i != stack_frame_info.len() - 1 {
            stack_trace.push('\n');
        }
    }

    // Panic if the stack trace is too large to represent as a string
    let frames = must_a!(cx.alloc_string_ptr(&stack_trace));

    let source_file_line_col =
        first_source_file_line_col.map(|(file, line, col)| (*file, line, col));

    Ok(CachedStackTraceInfo { frames, source_file_line_col, frame_info: *stack_frame_info })
}

/// A stack frame as seen by an embedder (SpiderMonkey's `JS::CaptureCurrentStack` / SavedFrame).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedStackFrame {
    /// The function's display name, None for anonymous functions and top-level code.
    pub function_name: Option<String>,
    /// The display name of the script or module file, None for builtins.
    pub file_name: Option<String>,
    /// 1-based line, None when the frame has no source position (builtins).
    pub line: Option<u32>,
    /// 1-based column, None when the frame has no source position.
    pub column: Option<u32>,
    /// Whether the frame runs an async function or async generator. Brimstone resumes awaiting
    /// functions from the job queue without their callers, so frames older than an `await` are not
    /// part of a captured stack: an async frame at the bottom of a stack marks that boundary.
    pub is_async_function: bool,
    /// Whether the frame is a builtin or native function.
    pub is_native: bool,
}

/// Describe up to `max_frames` frames of stack frame info, newest first (0 means all).
pub fn describe_stack_frames(
    cx: Context,
    stack_frame_info: Handle<StackFrameInfoArray>,
    max_frames: usize,
) -> AllocResult<Vec<CapturedStackFrame>> {
    prepare_for_stack_trace(cx, stack_frame_info)?;

    let len = if max_frames == 0 {
        stack_frame_info.len()
    } else {
        stack_frame_info.len().min(max_frames)
    };

    let mut frames = Vec::with_capacity(len);

    for stack_frame in &stack_frame_info.as_slice()[..len] {
        let func = stack_frame.function;
        let function_name = match func.name() {
            // Top-level code is reported without a name, like SpiderMonkey (`<global>`, `<eval>`)
            Some(name) => Some(name.format()?)
                .filter(|name| !(name.is_empty() || name.starts_with('<') && name.ends_with('>'))),
            None => None,
        };

        let file_name = func
            .source_file_ptr()
            .map(|file| file.display_name().to_string());

        let mut line = None;
        let mut column = None;
        if let Some(source_map) = func.source_map_ptr() {
            if let Some(source_position) =
                BytecodeSourceMap::get_source_position(source_map, stack_frame.bytecode_offset)
            {
                // Line offsets were generated in `prepare_for_stack_trace`, so this does not
                // allocate
                let source_file = func.source_file_ptr().unwrap();
                let line_offsets = source_file.line_offsets_ptr_raw().unwrap();
                let (frame_line, frame_column) = find_line_col_for_pos(
                    source_file.contents_as_slice(),
                    source_position,
                    line_offsets.as_slice(),
                );
                line = Some(frame_line as u32);
                column = Some(frame_column as u32);
            }
        }

        frames.push(CapturedStackFrame {
            function_name,
            file_name,
            line,
            column,
            is_async_function: func.is_async(),
            is_native: func.runtime_function_id().is_some(),
        });
    }

    Ok(frames)
}

/// Capture up to `max_frames` frames of the current stack, newest first (0 means all).
pub fn capture_current_stack(
    cx: Context,
    max_frames: usize,
) -> AllocResult<Vec<CapturedStackFrame>> {
    handle_scope_guard!(cx);

    if cx.vm.as_ref().is_none_or(|vm| vm.is_stack_empty()) {
        return Ok(vec![]);
    }

    let stack_frame_info =
        create_current_stack_frame_info(cx, /* skip_current_frame */ false)?;
    describe_stack_frames(cx, stack_frame_info.to_handle(), max_frames)
}

/// Prepare for a stack trace to be formatted. Perform any allocations that will be needed, such as
/// calculating line offsets for all source files.
fn prepare_for_stack_trace(
    cx: Context,
    stack_frame_info: Handle<StackFrameInfoArray>,
) -> AllocResult<()> {
    // Handle is shared between iterations
    let mut source_file_handle: Handle<SourceFile> = Handle::empty(cx);

    // Generate the line offsets for all source files referenced in the stack trace. This may
    // allocate if line offsets have not been generated yet.
    for i in 0..stack_frame_info.len() {
        if let Some(source_file) = stack_frame_info.as_slice()[i].function.source_file_ptr() {
            source_file_handle.replace(source_file);
            source_file_handle.line_offsets_ptr(cx)?;
        }
    }

    Ok(())
}

// An array of stack frame entries which contain the information necessary to construct a full
// stack trace later if desired.
impl_array_instance!(StackFrameInfoArray, HeapStackFrameInfo);

impl HeapItem for StackFrameInfoArray {
    fn byte_size(array: HeapPtr<Self>) -> usize {
        Self::calculate_size_in_bytes(array.len())
    }

    fn visit_pointers(mut array: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        array.visit_array_pointers(visitor);

        for stack_frame in array.as_mut_slice() {
            visitor.visit_pointer(&mut stack_frame.function);
        }
    }
}
