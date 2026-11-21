// Which source position a call reports (the column of a stack frame, an error's call site). The rule is SpiderMonkey's
// BytecodeEmitter::getCoordNode (frontend/BytecodeEmitter.cpp:8289-8334): a call reports the position of its `(`, except
//  - `obj.name(...)`: the position of the property name;
//  - `name(...)`: the start of the callee name, unless the call has arguments and starts its statement (the callee sits at the
//    last statement separator, BytecodeSection.h:274), when it is the `(`.
// Computed, private and optional member callees, calls of calls and every other callee report the `(`.
//
// Verus checks this very file (`//@` lines, as in gc/sizing.rs; verification/call-position).

//@ global size_of usize == 8;

/// What kind of expression the callee is, as far as the rule cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum CalleeKind {
    /// A plain identifier.
    Name,
    /// `obj.name` with a plain (not computed, not private, not optional) property name.
    Dot,
    /// Anything else.
    Other,
}

/// Where in the call expression the position is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub enum CoordAt {
    /// The start of the callee name.
    Callee,
    /// The property name of a dot callee.
    Property,
    /// The `(` that opens the arguments.
    Paren,
}

/// SpiderMonkey's getCoordNode for a call (not `new`): `has_args` is whether the call has arguments, `at_statement_start`
/// whether the callee starts the statement the call is in.
pub fn call_coord(kind: CalleeKind, has_args: bool, at_statement_start: bool) -> CoordAt //@ret
//@ ensures (kind == CalleeKind::Dot) ==> ret == CoordAt::Property,
//@     (kind == CalleeKind::Other) ==> ret == CoordAt::Paren,
//@     (kind == CalleeKind::Name && (!has_args || !at_statement_start)) ==> ret == CoordAt::Callee,
//@     (kind == CalleeKind::Name && has_args && at_statement_start) ==> ret == CoordAt::Paren,
{
    match kind {
        CalleeKind::Dot => CoordAt::Property,
        CalleeKind::Name => {
            if !has_args || !at_statement_start { CoordAt::Callee } else { CoordAt::Paren }
        }
        CalleeKind::Other => CoordAt::Paren,
    }
}

/// The position of the `(` that opens the arguments of a call whose callee ends at `from`: past whitespace, closing
/// parentheses of a parenthesized callee, and the `?.` of an optional call. None if the source has something else there.
pub fn find_paren(src: &[u8], from: usize) -> Option<usize> //@ret
//@ ensures match ret { Some(p) => from <= p && p < src.len() && src[p as int] == 40u8, None => true },
{
    if from > src.len() {
        return None;
    }
    let mut i = from;
    while i < src.len()
    //@ invariant from <= i, i <= src.len(),
    //@ decreases src.len() - i,
    {
        let c = src[i];
        if c == b'(' {
            return Some(i);
        }
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' || c == b')' || c == b'?' || c == b'.' {
            i += 1;
        } else {
            return None;
        }
    }
    None
}
