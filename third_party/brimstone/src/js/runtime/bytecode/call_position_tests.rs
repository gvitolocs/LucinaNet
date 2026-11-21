//! Tests of `call_position`: SpiderMonkey's getCoordNode cases and the paren search.
use super::call_position::{CalleeKind, CoordAt, call_coord, find_paren};

#[test]
fn coordinates_follow_spidermonkeys_get_coord_node() {
    // obj.name(...): the property name, whatever else.
    for (args, start) in [(false, false), (false, true), (true, false), (true, true)] {
        assert_eq!(call_coord(CalleeKind::Dot, args, start), CoordAt::Property);
        assert_eq!(call_coord(CalleeKind::Other, args, start), CoordAt::Paren);
    }
    // name(): the callee. name(args) inside an expression: the callee. name(args) starting its statement: the paren.
    assert_eq!(call_coord(CalleeKind::Name, false, true), CoordAt::Callee);
    assert_eq!(call_coord(CalleeKind::Name, true, false), CoordAt::Callee);
    assert_eq!(call_coord(CalleeKind::Name, false, false), CoordAt::Callee);
    assert_eq!(call_coord(CalleeKind::Name, true, true), CoordAt::Paren);
}

#[test]
fn the_paren_is_found_past_whitespace_closing_parens_and_optional_chaining() {
    let src = b"foo (a); (g)(b); f?.(c); h\n(d)";
    assert_eq!(find_paren(src, 3), Some(4)); // foo (a)
    assert_eq!(find_paren(src, 11), Some(12)); // (g)(b): from the end of g
    assert_eq!(find_paren(src, 18), Some(20)); // f?.(c): from the end of f
    assert_eq!(find_paren(src, 26), Some(27)); // h\n(d)
    // Something else in between, or the end of the source: no paren.
    assert_eq!(find_paren(b"a + b", 1), None);
    assert_eq!(find_paren(b"abc", 3), None);
    assert_eq!(find_paren(b"abc", 9), None);
}
