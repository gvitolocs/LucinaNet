//! Tests of `source_names`: naming the expression that failed, from the source text alone.
use super::source_names::{callee_name, expression_name, new_callee_name};

/// The source with the reported position marked, and the position itself.
///
/// The byte after the mark is the position the instruction reports, which is what `call_position`
/// picks for the shape that is written: the `(` for a computed, optional or otherwise odd callee,
/// the property name for `obj.name(...)`, the callee start for a plain name inside an expression,
/// and the start of a spread element. The mark may only appear once, so a source that would need a
/// `|` operator is written with `+` instead.
fn marked(src: &str) -> (Vec<u8>, usize) {
    let pos = src.find('|').expect("test source must mark a position");
    assert_eq!(src.match_indices('|').count(), 1, "the mark must be the only `|` in the source");

    (src.replace('|', "").into_bytes(), pos)
}

fn callee_at(src: &str) -> Option<String> {
    let (src, pos) = marked(src);
    callee_name(&src, pos)
}

fn new_callee_at(src: &str) -> Option<String> {
    let (src, pos) = marked(src);
    new_callee_name(&src, pos)
}

fn expression_at(src: &str) -> Option<String> {
    let (src, pos) = marked(src);
    expression_name(&src, pos)
}

#[test]
fn a_plain_name_is_written_as_written() {
    // A call of a plain name reports its `(` at the start of a statement, and the name elsewhere.
    assert_eq!(callee_at("foo|(1, 2)"), Some("foo".to_owned()));
    assert_eq!(callee_at("y = |foo(1)"), Some("foo".to_owned()));
    assert_eq!(callee_at("|f()"), Some("f".to_owned()));
    // Nothing in front of the call to name.
    assert_eq!(callee_at("|()"), None);
    assert_eq!(callee_at("|"), None);
}

#[test]
fn a_member_chain_is_written_in_full() {
    // `obj.name(...)`: the call reports the property name it looked up.
    assert_eq!(callee_at("a.b.|c(1)"), Some("a.b.c".to_owned()));
    assert_eq!(callee_at("a.|b()"), Some("a.b".to_owned()));
    assert_eq!(
        callee_at("window.|sc_initFeedback()"),
        Some("window.sc_initFeedback".to_owned())
    );
    assert_eq!(callee_at("this.|foo()"), Some("this.foo".to_owned()));
}

#[test]
fn a_computed_key_collapses_to_a_name_when_it_is_one() {
    assert_eq!(callee_at("a[0]|()"), Some("a[0]".to_owned()));
    assert_eq!(
        callee_at("window['sc_initFeedback']|()"),
        Some("window.sc_initFeedback".to_owned())
    );
    assert_eq!(callee_at("a['b']['c']|()"), Some("a.b.c".to_owned()));
    assert_eq!(callee_at("a[key]|()"), Some("a[key]".to_owned()));
    assert_eq!(callee_at("a[1 + 2]|()"), Some("a[...]".to_owned()));
    // Double quotes and whitespace inside the brackets are handled alike.
    assert_eq!(callee_at("a[ \"b\" ]|()"), Some("a.b".to_owned()));
}

#[test]
fn a_call_in_the_chain_keeps_the_name_and_drops_the_arguments() {
    assert_eq!(callee_at("f()|(1)"), Some("f(...)".to_owned()));
    assert_eq!(callee_at("a.b(1).|c(d)"), Some("a.b(...).c".to_owned()));
    assert_eq!(callee_at("getObj().|x()"), Some("getObj(...).x".to_owned()));
    assert_eq!(callee_at("a['b'](1).|c()"), Some("a.b(...).c".to_owned()));
}

#[test]
fn a_receiver_that_is_not_a_name_becomes_the_intermediate_value() {
    assert_eq!(callee_at("(x + y).|z()"), Some("(intermediate value).z".to_owned()));
    assert_eq!(callee_at("(x + y)|()"), Some("(intermediate value)".to_owned()));
    assert_eq!(
        callee_at("(function () {})()|(1)"),
        Some("(intermediate value)(...)".to_owned())
    );
    assert_eq!(callee_at("`${a}`.|x()"), Some("(intermediate value).x".to_owned()));
    assert_eq!(callee_at("({}).|x()"), Some("(intermediate value).x".to_owned()));
}

#[test]
fn optional_chaining_is_named_like_the_chain_it_is_written_as() {
    // An optional member is not the plain `obj.name` form, so the call reports its `(`.
    assert_eq!(callee_at("a?.b|(c)"), Some("a.b".to_owned()));
    assert_eq!(callee_at("a?.b|(1)"), Some("a.b".to_owned()));
    assert_eq!(callee_at("a?.[0]|()"), Some("a[0]".to_owned()));
    assert_eq!(callee_at("a?.b?.c|()"), Some("a.b.c".to_owned()));
    // The call form of optional chaining, `a?.()`, has no property name to report.
    assert_eq!(callee_at("a?.|()"), Some("a".to_owned()));
}

#[test]
fn a_spread_is_not_mistaken_for_a_property_access() {
    assert_eq!(callee_at("{ ...a.|b(1) }"), Some("a.b".to_owned()));
}

#[test]
fn names_are_shortened_to_a_maximum_length() {
    let src = "aaaaaaaaaa.bbbbbbbbbb.cccccccccc.dddddddddd.eeeeeeeeee.|ffffffffff(1)";
    let name = callee_at(src).unwrap();

    assert!(name.starts_with("aaaaaaaaaa.bbbbbbbbbb."));
    assert_eq!(name.chars().count(), 60);
    assert!(name.ends_with("..."));
}

#[test]
fn new_expressions_are_named_from_the_new_keyword() {
    assert_eq!(new_callee_at("|new Foo(1)"), Some("Foo".to_owned()));
    assert_eq!(new_callee_at("|new a.b(1)"), Some("a.b".to_owned()));
    assert_eq!(new_callee_at("|new a.b(1).c"), Some("a.b(...).c".to_owned()));
    assert_eq!(new_callee_at("|new a[0]"), Some("a[0]".to_owned()));
    assert_eq!(new_callee_at("|new Foo"), Some("Foo".to_owned()));
    assert_eq!(new_callee_at("|new Foo()"), Some("Foo".to_owned()));
    // `super` reports its own keyword, which already names the callee.
    assert_eq!(new_callee_at("|super(1)"), Some("super".to_owned()));
    // A grouped callee has no name.
    assert_eq!(new_callee_at("|new (f())"), None);
    // A name that merely starts with `new` is not the keyword.
    assert_eq!(new_callee_at("|newtype(1)"), Some("newtype".to_owned()));
}

#[test]
fn an_expression_is_named_from_where_it_starts() {
    assert_eq!(expression_at("|a.b"), Some("a.b".to_owned()));
    assert_eq!(expression_at("|a[0]"), Some("a[0]".to_owned()));
    assert_eq!(expression_at("|a.b(1).c"), Some("a.b(...).c".to_owned()));
    // Nothing nameable.
    assert_eq!(expression_at("|5"), None);
    assert_eq!(expression_at("|[a, b] = c"), None);
    assert_eq!(expression_at("|{a} = b"), None);
}

#[test]
fn a_spread_element_names_the_expression_it_spreads() {
    // A spread element reports the position of its `...`.
    assert_eq!(expression_at("[|...items]"), Some("items".to_owned()));
    assert_eq!(expression_at("[|... items]"), Some("items".to_owned()));
    assert_eq!(expression_at("[|...host.value]"), Some("host.value".to_owned()));
    assert_eq!(expression_at("f(|...list().next)"), Some("list(...).next".to_owned()));
    // A `...` that is not spread, and a spread of something that is not a name.
    assert_eq!(expression_at("[|...]"), None);
    assert_eq!(expression_at("[|...[1]]"), None);
}

#[test]
fn brackets_inside_strings_do_not_confuse_the_search_for_a_match() {
    // The `(` in the argument is part of a string, not a nested call.
    assert_eq!(callee_at("f(\"(\")|()"), Some("f(...)".to_owned()));
    // A key that is not a name is not shown as one either.
    assert_eq!(callee_at("f[')']|()"), Some("f[...]".to_owned()));
    assert_eq!(callee_at("a['[']|()"), Some("a[...]".to_owned()));
}

#[test]
fn an_unterminated_chain_gives_up_instead_of_naming_the_wrong_thing() {
    assert_eq!(callee_at("a.b.|()"), None);
    assert_eq!(callee_at("]|()"), None);
    assert_eq!(callee_at(")|()"), None);
}
