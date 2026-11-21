//! Names of expressions, taken from the source text, used to build browser-style TypeError
//! messages (`window.sc_initFeedback is not a function`, `foo is not iterable`).
//!
//! Throwing bytecode instructions record the source position they report in the function's source
//! map, and the source text of every script stays alive for the heap's lifetime, so the expression
//! that failed can be recovered when an error is about to be created. Everything here runs on the
//! error path only: the scanner reads source bytes and never the heap.
//!
//! The name of an expression is the shortest form that still says what was meant:
//! - `foo` and `a.b.c` are written as they are;
//! - `window['sc_initFeedback']` collapses to `window.sc_initFeedback`, `a[0]` stays `a[0]`;
//! - a call inside the chain collapses its arguments, so `a.b(1).c` becomes `a.b(...).c`;
//! - a receiver that is not a name becomes `(intermediate value)`, as in `(x || y).z`;
//! - names longer than [`MAX_NAME_LEN`] are truncated with an ellipsis.

use crate::{
    parser::loc::{NO_POS, Pos},
    runtime::{
        Context, HeapPtr,
        bytecode::{source_map::BytecodeSourceMap, vm::VM},
        source_file::SourceFile,
    },
};

/// Longest name taken from the source text.
const MAX_NAME_LEN: usize = 60;

/// Written in place of an expression that has no name of its own.
const INTERMEDIATE_VALUE: &str = "(intermediate value)";

/// Written in place of arguments, and of keys too long to show.
const ELLIPSIS: &str = "...";

/// Longest computed property key shown in a name.
const MAX_KEY_LEN: usize = 20;

/// How far back the scanner will walk before giving up on naming an expression.
const MAX_CHAIN_DEPTH: u32 = 16;

/// How far back or forward the scanner will look for a matching bracket.
const MAX_BRACKET_SCAN: usize = 4096;

/// Part of an expression that is known by name, and the part that is not.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Chain {
    /// The whole expression is a name, such as `a.b`.
    Name(String),
    /// Only the tail of the expression is named, such as `.c`; its receiver is not a name.
    Partial(String),
}

/// The name of the callee of a call, from the position the call instruction reports. As
/// [`crate::runtime::bytecode::call_position`] decides, that position is the `(` that opens the
/// arguments, the name that is looked up, or the start of a plain callee.
pub fn callee_name(src: &[u8], pos: Pos) -> Option<String> {
    let byte = *src.get(pos)?;

    // A position on an identifier names the callee itself; anywhere else the callee ends there.
    let end = if is_ident_byte(byte) {
        ident_end(src, pos)
    } else {
        pos
    };

    render(walk_back(src, end, 0))
}

/// The name of the callee of a `new` expression, from the position of the `new` keyword.
pub fn new_callee_name(src: &[u8], pos: Pos) -> Option<String> {
    // `super(...)` reports its own keyword, which already is the callee.
    let start = skip_keyword(src, pos, b"new").unwrap_or(pos);
    let end = chain_end(src, start)?;

    render(walk_back(src, end, 0))
}

/// The name of the expression that starts at `pos`. A spread element reports the position of its
/// `...`, which is stepped over so that the expression that is spread is the one that is named.
pub fn expression_name(src: &[u8], pos: Pos) -> Option<String> {
    let end = chain_end(src, skip_spread(src, pos))?;

    render(walk_back(src, end, 0))
}

/// Step over the `...` of a spread element, if `pos` is on one.
fn skip_spread(src: &[u8], pos: Pos) -> Pos {
    if !src.get(pos..).is_some_and(|rest| rest.starts_with(b"...")) {
        return pos;
    }

    skip_whitespace_forward(src, pos + ELLIPSIS.len())
}

fn render(chain: Option<Chain>) -> Option<String> {
    let text = match chain? {
        Chain::Name(name) => name,
        Chain::Partial(suffix) => format!("{INTERMEDIATE_VALUE}{suffix}"),
    };

    Some(shorten(&text))
}

/// Name the expression ending at `end`, by walking backwards over its member accesses.
fn walk_back(src: &[u8], end: Pos, depth: u32) -> Option<Chain> {
    if depth >= MAX_CHAIN_DEPTH {
        return None;
    }

    let i = skip_whitespace_back(src, end);

    if i == 0 {
        return None;
    }

    match src[i - 1] {
        byte if is_ident_byte(byte) => {
            let start = ident_start(src, i);
            let before = skip_whitespace_back(src, start);

            // A `.` in front of the name makes it a property of whatever comes before it.
            if before > 0 && src[before - 1] == b'.' && !is_spread(src, before - 1) {
                let dot = skip_optional_marker(src, before - 1);
                let name = text_of(src, start, i)?;

                return Some(push_member(walk_back(src, dot, depth + 1), &format!(".{name}")));
            }

            Some(Chain::Name(text_of(src, start, i)?))
        }
        b']' => {
            let open = matching_open(src, i - 1, b'[', b']')?;
            let suffix = computed_suffix(src, open + 1, i - 1)?;
            let base = walk_back(src, skip_property_marker(src, open), depth + 1);

            Some(push_member(base, &suffix))
        }
        b')' => {
            let open = matching_open(src, i - 1, b'(', b')')?;
            let before = skip_whitespace_back(src, open);

            // A `(` is a call only when something callable precedes it; otherwise it groups.
            let is_call = before > 0 && {
                let byte = src[before - 1];
                is_ident_byte(byte) || matches!(byte, b')' | b']')
            };

            if !is_call {
                return Some(Chain::Partial(String::new()));
            }

            match walk_back(src, before, depth + 1) {
                Some(Chain::Name(name)) => Some(Chain::Name(format!("{name}({ELLIPSIS})"))),
                Some(Chain::Partial(suffix)) => {
                    Some(Chain::Partial(format!("{suffix}({ELLIPSIS})")))
                }
                // The callee of the call is not a name either, but this is still a call of something.
                None => Some(Chain::Partial(String::new())),
            }
        }
        // The `?.` of an optional call of the receiver itself, such as `a?.(x)`.
        b'.' => {
            let marker = skip_optional_marker(src, i - 1);

            if marker == i - 1 {
                return None;
            }

            walk_back(src, marker, depth + 1)
        }
        _ => None,
    }
}

/// Add a member access in front of an expression whose receiver may not be nameable.
fn push_member(base: Option<Chain>, suffix: &str) -> Chain {
    match base {
        Some(Chain::Name(name)) => Chain::Name(format!("{name}{suffix}")),
        _ => Chain::Partial(suffix.to_owned()),
    }
}

/// The end of the member chain that starts at `start`, stopping before the arguments of a call.
fn chain_end(src: &[u8], start: Pos) -> Option<Pos> {
    let first = skip_whitespace_forward(src, start);

    if !src
        .get(first)
        .is_some_and(|byte| is_ident_start_byte(*byte))
    {
        return None;
    }

    let mut end = ident_end(src, first);

    loop {
        let i = skip_whitespace_forward(src, end);

        let name_index = match src.get(i) {
            Some(b'[') => {
                end = matching_close(src, i, b'[', b']')? + 1;
                continue;
            }
            Some(b'(') => {
                let close = matching_close(src, i, b'(', b')')?;

                // The chain continues past a call only if something follows its arguments.
                if !matches!(
                    src.get(skip_whitespace_forward(src, close + 1)),
                    Some(b'.' | b'?' | b'[')
                ) {
                    return Some(end);
                }

                end = close + 1;
                continue;
            }
            Some(b'.') if !is_spread(src, i) => i + 1,
            Some(b'?') if src.get(i + 1) == Some(&b'.') => i + 2,
            _ => return Some(end),
        };

        let mut j = skip_whitespace_forward(src, name_index);

        // Private names are spelled `#field`.
        if src.get(j) == Some(&b'#') {
            j = skip_whitespace_forward(src, j + 1);
        }

        if !src.get(j).is_some_and(|byte| is_ident_start_byte(*byte)) {
            return Some(end);
        }

        end = ident_end(src, j);
    }
}

/// Render a computed key `[expr]` as the shortest form that still identifies it.
fn computed_suffix(src: &[u8], start: Pos, end: Pos) -> Option<String> {
    let (start, end) = trim(src, start, end);
    let key = text_of(src, start, end)?;

    if key.len() <= MAX_KEY_LEN {
        if let Some(name) = identifier_of_string_literal(&key) {
            return Some(format!(".{name}"));
        }

        if key.chars().all(|c| is_ident_char(c) || c == '.') {
            return Some(format!("[{key}]"));
        }
    }

    Some(format!("[{ELLIPSIS}]"))
}

/// The name a string literal stands for as a computed property key, if it is a valid name.
fn identifier_of_string_literal(key: &str) -> Option<&str> {
    let bytes = key.as_bytes();
    let quote = bytes
        .first()
        .copied()
        .filter(|byte| matches!(byte, b'\'' | b'"'))?;

    if bytes.len() < 2 || bytes[bytes.len() - 1] != quote {
        return None;
    }

    let inner = &key[1..bytes.len() - 1];

    if !inner.chars().next().is_some_and(is_ident_start_char) {
        return None;
    }

    inner.chars().all(is_ident_char).then_some(inner)
}

fn shorten(name: &str) -> String {
    if name.len() <= MAX_NAME_LEN {
        return name.to_owned();
    }

    let mut cut = MAX_NAME_LEN - ELLIPSIS.len();
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }

    format!("{}{ELLIPSIS}", &name[..cut])
}

/// Whether the `.` at `i` is the last byte of a `...` spread operator.
fn is_spread(src: &[u8], i: Pos) -> bool {
    i >= 2 && src[i - 1] == b'.' && src[i - 2] == b'.'
}

/// Step back over the `?` of an optional member access such as `a?.b`.
fn skip_optional_marker(src: &[u8], dot: Pos) -> Pos {
    if dot > 0 && src[dot - 1] == b'?' {
        skip_whitespace_back(src, dot - 1)
    } else {
        dot
    }
}

/// Step back over the `?.` of an optional computed access such as `a?.[b]`.
fn skip_property_marker(src: &[u8], open: Pos) -> Pos {
    let mut i = skip_whitespace_back(src, open);

    if i > 0 && src[i - 1] == b'.' && !is_spread(src, i - 1) {
        i = skip_whitespace_back(src, i - 1);
    }

    skip_optional_marker(src, i)
}

/// Bytes above ASCII are taken as part of a name, so that a name in any script is named.
fn is_ident_byte(byte: u8) -> bool {
    is_ident_char(byte as char)
}

fn is_ident_start_byte(byte: u8) -> bool {
    is_ident_start_char(byte as char)
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$') || !c.is_ascii()
}

fn is_ident_start_char(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '_' | '$') || !c.is_ascii()
}

fn ident_end(src: &[u8], start: Pos) -> Pos {
    let mut i = start;
    while src.get(i).is_some_and(|byte| is_ident_byte(*byte)) {
        i += 1;
    }
    i
}

fn ident_start(src: &[u8], end: Pos) -> Pos {
    let mut i = end;
    while i > 0 && is_ident_byte(src[i - 1]) {
        i -= 1;
    }
    i
}

fn skip_whitespace_back(src: &[u8], end: Pos) -> Pos {
    let mut i = end.min(src.len());
    while i > 0 && matches!(src[i - 1], b' ' | b'\t' | b'\n' | b'\r') {
        i -= 1;
    }
    i
}

fn skip_whitespace_forward(src: &[u8], start: Pos) -> Pos {
    let mut i = start;
    while src
        .get(i)
        .is_some_and(|byte| matches!(*byte, b' ' | b'\t' | b'\n' | b'\r'))
    {
        i += 1;
    }
    i
}

fn trim(src: &[u8], start: Pos, end: Pos) -> (Pos, Pos) {
    (skip_whitespace_forward(src, start), skip_whitespace_back(src, end))
}

fn text_of(src: &[u8], start: Pos, end: Pos) -> Option<String> {
    if end <= start {
        return None;
    }

    Some(String::from_utf8_lossy(src.get(start..end)?).into_owned())
}

/// Find the `open` that matches the `close` at `i`. String literals are skipped, comments are not;
/// a chain whose brackets do not balance simply does not get named.
fn matching_open(src: &[u8], i: Pos, open: u8, close: u8) -> Option<Pos> {
    let mut depth = 0usize;
    let mut j = i;

    loop {
        match src[j] {
            byte if byte == close => depth += 1,
            byte if byte == open => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            quote @ (b'\'' | b'"' | b'`') => {
                j = find_quote_start(src, j, quote);
            }
            _ => {}
        }

        if i - j >= MAX_BRACKET_SCAN {
            return None;
        }

        if j == 0 {
            return None;
        }

        j -= 1;
    }
}

/// Find the `close` that matches the `open` at `i`.
fn matching_close(src: &[u8], i: Pos, open: u8, close: u8) -> Option<Pos> {
    let mut depth = 0usize;
    let mut j = i;

    while j < src.len() {
        match src[j] {
            byte if byte == open => depth += 1,
            byte if byte == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            quote @ (b'\'' | b'"' | b'`') => {
                j = find_quote_end(src, j, quote)?;
            }
            _ => {}
        }

        if j - i >= MAX_BRACKET_SCAN {
            return None;
        }

        j += 1;
    }

    None
}

/// The opening quote of the string literal that ends at `i`.
fn find_quote_start(src: &[u8], i: Pos, quote: u8) -> Pos {
    let mut j = i;
    while j > 0 {
        j -= 1;
        if src[j] == quote && (j == 0 || src[j - 1] != b'\\') {
            return j;
        }
    }
    0
}

/// The closing quote of the string literal that starts at `i`.
fn find_quote_end(src: &[u8], i: Pos, quote: u8) -> Option<Pos> {
    let mut j = i + 1;
    while j < src.len() {
        if src[j] == quote && src[j - 1] != b'\\' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Skip the keyword at `pos`, if that is what is written there.
fn skip_keyword(src: &[u8], pos: Pos, keyword: &[u8]) -> Option<Pos> {
    let rest = src.get(pos..)?;

    if !rest.starts_with(keyword) {
        return None;
    }

    let after = pos + keyword.len();

    if src.get(after).is_some_and(|byte| is_ident_byte(*byte)) {
        return None;
    }

    Some(skip_whitespace_forward(src, after))
}

/// The source file and position reported by the instruction that is about to throw.
fn error_site(cx: Context) -> Option<(HeapPtr<SourceFile>, Pos)> {
    let vm: &VM = cx.vm.as_ref()?;

    if vm.is_stack_empty() {
        return None;
    }

    let function = vm.stack_frame().closure().function_ptr();
    let source_map = function.source_map_ptr()?;
    let source_file = function.source_file_ptr()?;

    let bytecode_offset = vm.published_pc() as usize - function.bytecode().as_ptr() as usize;
    let pos = BytecodeSourceMap::get_source_position(source_map, bytecode_offset)?;

    (pos != NO_POS).then_some((source_file, pos))
}

/// `<callee> is not a function`, naming the callee of the call that is throwing.
pub fn not_a_function_message(cx: Context) -> Option<String> {
    callee_message(cx, false).map(|name| format!("{name} is not a function"))
}

/// `<callee> is not a constructor`, naming the callee of the `new` expression that is throwing.
pub fn not_a_constructor_message(cx: Context) -> Option<String> {
    callee_message(cx, true).map(|name| format!("{name} is not a constructor"))
}

fn callee_message(cx: Context, as_constructor: bool) -> Option<String> {
    let (source_file, pos) = error_site(cx)?;
    let src = source_file.contents_as_slice();

    if as_constructor {
        new_callee_name(src, pos)
    } else {
        callee_name(src, pos)
    }
}

/// `<expr> is not iterable`, naming the iterated expression, or `fallback` when that expression has
/// no source text to name it by.
pub fn not_iterable_message(cx: Context, fallback: &str) -> String {
    let named = error_site(cx)
        .and_then(|(source_file, pos)| expression_name(source_file.contents_as_slice(), pos));

    match named {
        Some(name) => format!("{name} is not iterable"),
        None => fallback.to_owned(),
    }
}
