use crate::runtime::{
    Arguments, Context, EvalResult, Handle, Realm, Value, alloc_error::AllocResult,
    error::type_error, gc::HeapVisitor, intrinsics::intrinsics::Intrinsic,
    regexp::matcher::Match, string_value::StringValue, to_string, type_utilities::same_value,
};

/// A RegExp legacy static property (annex B.2.4), named after the internal slot of the realm's
/// `%RegExp%` that holds its value.
#[derive(Clone, Copy)]
pub enum LegacyRegexpStatic {
    /// `RegExp.input` and `RegExp.$_` ([[RegExpInput]])
    Input,
    /// `RegExp.lastMatch` and `RegExp.$&` ([[RegExpLastMatch]])
    LastMatch,
    /// `RegExp.lastParen` and `RegExp.$+` ([[RegExpLastParen]])
    LastParen,
    /// `RegExp.leftContext` and ``RegExp.$` `` ([[RegExpLeftContext]])
    LeftContext,
    /// `RegExp.rightContext` and `RegExp.$'` ([[RegExpRightContext]])
    RightContext,
    /// `RegExp.$1` through `RegExp.$9` ([[RegExpParen1]] through [[RegExpParen9]]). The value is
    /// the 1-based capture group number.
    Paren(usize),
}

impl LegacyRegexpStatic {
    /// Number of numbered capture group properties, `RegExp.$1` through `RegExp.$9`.
    pub const NUM_PAREN_SLOTS: usize = 9;
}

/// RegExp legacy static properties (annex B.2.4). There is one set per realm, so that a realm's
/// `%RegExp%` only reports the matches that were made in that realm.
///
/// Only the input string and the positions the match found in it are recorded. Every other legacy
/// static property is a slice of the input, taken when the property is read, so that recording a
/// match does not allocate.
#[derive(Clone, Copy)]
pub struct LegacyRegexpStatics {
    /// [[RegExpInput]]: the string that the last legacy-enabled regexp was evaluated against.
    input: Value,

    /// Bounds in the input of the last recorded match.
    full_capture: Option<(u32, u32)>,

    /// Bounds of the first nine capture groups of the last recorded match, which are
    /// [[RegExpParen1]] through [[RegExpParen9]]. A group that did not take part in the match is
    /// `None`.
    parens: [Option<(u32, u32)>; LegacyRegexpStatic::NUM_PAREN_SLOTS],

    /// Bounds of the highest-numbered capture group that took part in the last recorded match,
    /// which is [[RegExpLastParen]].
    last_paren: Option<(u32, u32)>,

    /// Whether anything has been recorded yet. Before a regular expression that has
    /// [[LegacyFeaturesEnabled]] has matched there is nothing to report, and reading the properties
    /// throws.
    is_valid: bool,
}

impl LegacyRegexpStatics {
    /// The state of a realm in which no legacy match has been recorded yet.
    pub const fn new() -> Self {
        Self {
            input: Value::undefined(),
            full_capture: None,
            parens: [None; LegacyRegexpStatic::NUM_PAREN_SLOTS],
            last_paren: None,
            is_valid: false,
        }
    }

    pub fn visit_pointers(&mut self, visitor: &mut impl HeapVisitor) {
        visitor.visit_value(&mut self.input);
    }

    /// The value of one legacy static property, sliced out of the recorded input.
    fn value_of(
        &self,
        cx: Context,
        property: LegacyRegexpStatic,
    ) -> EvalResult<Handle<Value>> {
        let bounds = match property {
            LegacyRegexpStatic::Input => return Ok(self.input.to_handle(cx)),
            LegacyRegexpStatic::LastMatch => self.full_capture,
            LegacyRegexpStatic::LeftContext => self.full_capture.map(|(start, _)| (0, start)),
            LegacyRegexpStatic::RightContext => {
                let input = self.input.as_string().to_handle();
                self.full_capture.map(|(_, end)| (end, input.len()))
            }
            LegacyRegexpStatic::LastParen => self.last_paren,
            LegacyRegexpStatic::Paren(group) => self.parens[group - 1],
        };

        // A property that the recorded match did not set is the empty string.
        let Some((start, end)) = bounds else {
            return Ok(cx.names.empty_string().as_string().into());
        };

        let input = self.input.as_string().to_handle();
        Ok(input.substring(cx, start, end)?.into())
    }
}

/// Check the receiver of a legacy static property access, which is the common part of
/// GetLegacyRegExpStaticProperty and SetLegacyRegExpStaticProperty (annex B.2.4).
///
/// Returns the realm whose `%RegExp%` owns the properties.
fn legacy_regexp_static_realm(
    cx: Context,
    this_value: Handle<Value>,
    property_name: &str,
) -> EvalResult<Handle<Realm>> {
    // Step 1: Assert C is an object that has an internal slot named internalSlotName. `C` is this
    // realm's `%RegExp%`, the object that the slots belong to.
    let realm = cx.current_realm();
    let constructor = realm.get_intrinsic(Intrinsic::RegExpConstructor);

    // Step 2: If SameValue(C, thisValue) is false, throw a TypeError exception. This rejects a
    // subclass of `%RegExp%`, another realm's `%RegExp%`, a regexp instance, `RegExp.prototype` and
    // every other receiver.
    if !same_value(constructor.into(), this_value)? {
        return type_error(
            cx,
            &format!("{property_name} must be called on the realm's RegExp constructor"),
        );
    }

    Ok(realm)
}

/// GetLegacyRegExpStaticProperty (annex B.2.4)
pub fn legacy_static_getter(
    cx: Context,
    this_value: Handle<Value>,
    property: LegacyRegexpStatic,
    property_name: &str,
) -> EvalResult<Handle<Value>> {
    let realm = legacy_regexp_static_realm(cx, this_value, property_name)?;

    // Step 3: Return the value of C's internal slot. The slot holds nothing until a regular
    // expression that has [[LegacyFeaturesEnabled]] has been evaluated against a string.
    let statics = *realm.legacy_regexp_statics();
    if !statics.is_valid {
        return type_error(
            cx,
            &format!("{property_name} is only set once a regular expression has matched"),
        );
    }

    statics.value_of(cx, property)
}

/// SetLegacyRegExpStaticProperty (annex B.2.4). `RegExp.input` and `RegExp.$_` are the only legacy
/// static properties that can be written.
pub fn legacy_static_setter(
    cx: Context,
    this_value: Handle<Value>,
    arguments: Arguments,
    property_name: &str,
) -> EvalResult<Handle<Value>> {
    let mut realm = legacy_regexp_static_realm(cx, this_value, property_name)?;

    // Step 3: Set C's internal slot to the value, which is stored as a string like every other
    // legacy static property.
    let value_string = to_string(cx, arguments.get(cx, 0))?;

    // The properties that are slices of a match are dropped with it, because the positions that
    // were recorded do not refer to the new string.
    let statics = realm.legacy_regexp_statics_mut();
    statics.input = *value_string.as_value();
    statics.full_capture = None;
    statics.parens = [None; LegacyRegexpStatic::NUM_PAREN_SLOTS];
    statics.last_paren = None;
    statics.is_valid = true;

    Ok(cx.undefined())
}

/// Record the result of a match made by a regexp that has [[LegacyFeaturesEnabled]] (annex B.2.4).
///
/// Only the string and the positions are kept, so this does not allocate.
pub fn update_legacy_regexp_statics(
    cx: Context,
    string_value: Handle<StringValue>,
    match_: &Match,
) -> AllocResult<()> {
    let full_capture = match_.full_capture();

    // The implicit 0'th capture group covers the whole match and is not a parenthesis, so it is
    // neither a numbered property nor `lastParen`. The groups are walked in order, so the last
    // group seen is the highest-numbered one that took part in the match.
    let mut parens = [None; LegacyRegexpStatic::NUM_PAREN_SLOTS];
    let mut last_paren = None;

    for (group, capture) in match_.capture_groups.iter().enumerate().skip(1) {
        let Some(capture) = capture else { continue };

        let bounds = (capture.start, capture.end);
        last_paren = Some(bounds);

        if group <= LegacyRegexpStatic::NUM_PAREN_SLOTS {
            parens[group - 1] = Some(bounds);
        }
    }

    let mut realm = cx.current_realm();
    let statics = realm.legacy_regexp_statics_mut();
    statics.input = *string_value.as_value();
    statics.full_capture = Some((full_capture.start, full_capture.end));
    statics.parens = parens;
    statics.last_paren = last_paren;
    statics.is_valid = true;

    Ok(())
}
