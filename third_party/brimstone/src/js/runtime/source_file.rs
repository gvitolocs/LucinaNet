use std::rc::Rc;

use crate::{
    must_a,
    parser::{loc::calculate_line_offsets, source::Source},
    runtime::{
        Context, Handle, HeapItemKind, HeapPtr,
        alloc_error::AllocResult,
        collections::{ArrayInstance, array::U32Array},
        gc::{AnyHeapItem, HeapItem, HeapVisitor},
        shape::Shape,
        string_value::FlatString,
    },
};

/// The texts of the context's source files, kept outside the moving heap (SpiderMonkey keeps its
/// `ScriptSource` text outside the GC heap too). Each entry holds a source file weakly and the
/// parser's `Source` strongly, so the text is shared with the parser (which keeps it to parse lazy
/// functions again) instead of copied into the heap, and no collection copies it.
#[derive(Default)]
pub struct SourceTexts {
    entries: Vec<(HeapPtr<SourceFile>, Rc<Source>)>,
}

impl SourceTexts {
    /// Update the source files after a GC moved the heap, dropping the texts of collected files.
    /// `resolve` returns the new location of a live item, or None if it was collected.
    pub fn sweep(
        &mut self,
        resolve: impl Fn(HeapPtr<AnyHeapItem>) -> Option<HeapPtr<AnyHeapItem>>,
    ) {
        self.entries
            .retain_mut(|(file, _)| match resolve(file.cast()) {
                Some(new_location) => {
                    *file = new_location.cast();
                    true
                }
                None => false,
            });
    }

    /// Number of source files whose text is kept.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes of the texts kept, each source counted once.
    pub fn text_bytes(&self) -> usize {
        let mut seen: Vec<*const Source> = Vec::new();
        let mut bytes = 0;
        for (_, source) in &self.entries {
            let ptr = Rc::as_ptr(source);
            if !seen.contains(&ptr) {
                seen.push(ptr);
                bytes += source.contents.len();
            }
        }
        bytes
    }
}

#[repr(C)]
pub struct SourceFile {
    shape: HeapPtr<Shape>,
    /// The path to the source file
    path: HeapPtr<FlatString>,
    /// The display name of the source file, if it is different from the path
    display_name: Option<HeapPtr<FlatString>>,
    /// Lazily generated array of line offsets for the source file
    line_offsets: Option<HeapPtr<U32Array>>,
    /// The WTF-8 source text, owned by an entry of `Context::source_texts` for as long as this
    /// file lives (the entry is swept only once the file is collected).
    contents_ptr: *const u8,
    contents_len: usize,
}

impl SourceFile {
    #[inline]
    pub fn new(mut cx: Context, source: &Rc<Source>) -> AllocResult<Handle<SourceFile>> {
        // Source guarantees that file path and display name are guaranteed to be within allowed
        // string length.
        let path = must_a!(cx.alloc_flat_string(source.file_path()));
        let display_name = if source.has_display_name() {
            Some(must_a!(cx.alloc_flat_string(source.display_name())))
        } else {
            None
        };

        let mut scope = cx.alloc_uninit::<SourceFile>()?;

        scope.shape = cx.shapes.get(HeapItemKind::SourceFile);
        scope.line_offsets = None;
        scope.path = *path;
        scope.display_name = display_name.map(|n| *n);

        let contents = source.contents.as_bytes();
        scope.contents_ptr = contents.as_ptr();
        scope.contents_len = contents.len();
        cx.source_texts.entries.push((scope, source.clone()));

        Ok(scope.to_handle())
    }

    #[inline]
    pub fn path(&self) -> Handle<FlatString> {
        self.path.to_handle()
    }

    #[inline]
    pub fn display_name(&self) -> Handle<FlatString> {
        self.display_name.as_ref().unwrap_or(&self.path).to_handle()
    }

    #[inline]
    pub fn line_offsets_ptr_raw(&self) -> Option<HeapPtr<U32Array>> {
        self.line_offsets
    }

    #[inline]
    pub fn contents_as_slice(&self) -> &[u8] {
        // SAFETY: the text belongs to a `Source` that the context's `source_texts` keep alive as
        // long as this file is alive, and a `Source`'s text never changes or moves.
        unsafe { std::slice::from_raw_parts(self.contents_ptr, self.contents_len) }
    }
}

impl Handle<SourceFile> {
    pub fn line_offsets_ptr(&mut self, cx: Context) -> AllocResult<HeapPtr<U32Array>> {
        if let Some(line_offsets) = self.line_offsets {
            return Ok(line_offsets);
        }

        // Lazily generate line offsets when first requested
        let raw_line_offsets = calculate_line_offsets(self.contents_as_slice());
        let line_offsets_object = U32Array::new_from_slice(cx, &raw_line_offsets)?;

        self.line_offsets = Some(line_offsets_object);

        Ok(line_offsets_object)
    }

    pub fn get_line(&mut self, cx: Context, line: usize) -> AllocResult<String> {
        let offsets = self.line_offsets_ptr(cx)?;
        let start = offsets.as_slice()[line] as usize;

        let end = if line + 1 < offsets.len() {
            // Exclude the newline character if one exists
            offsets.as_slice()[line + 1] as usize - 1
        } else {
            self.contents_len
        };

        let line_contents = &self.contents_as_slice()[start..end];
        Ok(String::from_utf8_lossy(line_contents).to_string())
    }
}

impl HeapItem for SourceFile {
    fn byte_size(_: HeapPtr<Self>) -> usize {
        size_of::<SourceFile>()
    }

    fn visit_pointers(mut source_file: HeapPtr<Self>, visitor: &mut impl HeapVisitor) {
        visitor.visit_pointer(&mut source_file.shape);
        visitor.visit_pointer(&mut source_file.path);
        visitor.visit_pointer_opt(&mut source_file.display_name);
        visitor.visit_pointer_opt(&mut source_file.line_offsets);
    }
}
