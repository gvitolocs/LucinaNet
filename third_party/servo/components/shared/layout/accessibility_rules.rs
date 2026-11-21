/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The rules that decide what of a form field leaves the engine in the accessibility tree, which
//! accessibility actions a node advertises and accepts, and how long a string in the tree may be.
//!
//! These are pure functions so that they can be checked on their own. LucinaNet proves them with
//! Verus in this very file, not a copy of it (`verification/servo-ax/rules.rs` in LucinaNet; the
//! attributes below are only seen by Verus). What each promises is in its `ensures`.
//!
//! Not proved here: that the DOM side asks these rules for every field and every action. That is
//! what the tests in `components/servo/tests/accessibility.rs` check.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The longest string attribute (name, value, description) a node carries, in bytes. Chromium
/// clips string attributes at `kMaxStringAttributeLength` (10,000); we do the same, in bytes.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_STRING_BYTES: usize = 10_000;

/// Whether a field's value may be shown to assistive technology.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Secrecy {
    /// An ordinary field: its value is in the tree, as in every browser.
    Open,
    /// A password, a one-time code or a payment card field: no value and no length in the tree,
    /// and no value may be set through accessibility.
    Secret,
}

/// The accessibility actions this engine handles.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// AccessKit `Click`: the element's default action (Chromium's `DoDefault`).
    Click,
    Focus,
    Blur,
    /// AccessKit `SetValue`: replace a text field's value, or pick a `<select>`'s option.
    SetValue,
    ScrollIntoView,
    /// AccessKit `ScrollUp` / `ScrollDown`: a page of the node's scroll container.
    ScrollUp,
    ScrollDown,
    /// Anything else AccessKit defines; this engine refuses it.
    Other,
}

/// What the DOM says about the action's target, at the moment the action runs.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Target {
    /// The element has a default action: a link with `href`, a button, an input, an option, a
    /// `<summary>`, a `<label>`, or an element with an ARIA widget role.
    pub activatable: bool,
    /// The element is a focusable area (HTML's "focusable area").
    pub focusable: bool,
    /// The element takes a value: a text field, a `<textarea>`, a `<select>`, a range or a number.
    pub takes_value: bool,
    /// HTML's "actually disabled".
    pub disabled: bool,
    /// `readonly` on a field that takes a value.
    pub readonly: bool,
    /// The field is [`Secrecy::Secret`].
    pub secret: bool,
    /// The element is a scroll container, or the document's root.
    pub scrolls: bool,
    /// The node is hidden from the tree (`display: none`, `aria-hidden`, or `inert`).
    pub hidden: bool,
}

/// Why an action is refused.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// The node does not take this action.
    NotSupported,
    Hidden,
    Disabled,
    ReadOnly,
    /// A value may not be set on a secret field.
    Secret,
}

/// Which actions a node advertises in the tree.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Advertised {
    pub click: bool,
    pub focus: bool,
    pub set_value: bool,
    pub scroll_into_view: bool,
    pub scroll: bool,
}

#[cfg(verus_keep_ghost)]
verus! {

/// The specification of [`check_action`]: the action runs only on a visible node that takes it,
/// never sets a value on a secret, disabled or read-only field, and never clicks or focuses a
/// disabled element.
pub open spec fn allowed(t: Target, a: Act) -> bool {
    !t.hidden && match a {
        Act::Click => t.activatable && !t.disabled,
        Act::Focus => t.focusable && !t.disabled,
        Act::Blur => t.focusable,
        Act::SetValue => t.takes_value && !t.disabled && !t.readonly && !t.secret,
        Act::ScrollIntoView => true,
        Act::ScrollUp | Act::ScrollDown => t.scrolls,
        Act::Other => false,
    }
}

} // verus!

/// What a field's value shows in the tree: the value itself, or nothing for a secret field.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        // A secret field's value never leaves this function.
        secrecy == Secrecy::Secret ==> r == None::<&str>,
        secrecy == Secrecy::Open ==> r == Some(value),
))]
pub fn exposed_value(secrecy: Secrecy, value: &str) -> Option<&str> {
    match secrecy {
        Secrecy::Secret => None,
        Secrecy::Open => Some(value),
    }
}

/// Whether a field is secret, from what the caller found: `type=password`, and whether an
/// `autocomplete` token names a secret (see [`autocomplete_token_is_secret`]).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == Secrecy::Secret <==> (is_password_input || has_secret_autocomplete_token),
))]
pub fn secrecy(is_password_input: bool, has_secret_autocomplete_token: bool) -> Secrecy {
    if is_password_input || has_secret_autocomplete_token {
        Secrecy::Secret
    } else {
        Secrecy::Open
    }
}

/// Whether one `autocomplete` token names a secret: a password, a one-time code, or a payment
/// card's number, security code or expiry (HTML's autofill field names). Matching is ASCII
/// case-insensitive, as HTML's autofill processing is.
#[cfg_attr(verus_keep_ghost, verifier::external_body)]
pub fn autocomplete_token_is_secret(token: &str) -> bool {
    const SECRET_TOKENS: [&str; 9] = [
        "current-password",
        "new-password",
        "one-time-code",
        "cc-number",
        "cc-csc",
        "cc-exp",
        "cc-exp-month",
        "cc-exp-year",
        "transaction-amount",
    ];
    SECRET_TOKENS
        .iter()
        .any(|secret| secret.eq_ignore_ascii_case(token))
}

/// Whether an action may run on its target now. The DOM side asks this for every action before it
/// does anything.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        (r == Verdict::Allow) <==> allowed(t, a),
        a == Act::SetValue && t.secret && !t.hidden && t.takes_value ==> r == Verdict::Secret,
))]
pub fn check_action(t: Target, a: Act) -> Verdict {
    if t.hidden {
        return Verdict::Hidden;
    }
    match a {
        Act::Click => {
            if !t.activatable {
                Verdict::NotSupported
            } else if t.disabled {
                Verdict::Disabled
            } else {
                Verdict::Allow
            }
        },
        Act::Focus => {
            if !t.focusable {
                Verdict::NotSupported
            } else if t.disabled {
                Verdict::Disabled
            } else {
                Verdict::Allow
            }
        },
        Act::Blur => {
            if t.focusable {
                Verdict::Allow
            } else {
                Verdict::NotSupported
            }
        },
        Act::SetValue => {
            if !t.takes_value {
                Verdict::NotSupported
            } else if t.secret {
                Verdict::Secret
            } else if t.disabled {
                Verdict::Disabled
            } else if t.readonly {
                Verdict::ReadOnly
            } else {
                Verdict::Allow
            }
        },
        Act::ScrollIntoView => Verdict::Allow,
        Act::ScrollUp | Act::ScrollDown => {
            if t.scrolls {
                Verdict::Allow
            } else {
                Verdict::NotSupported
            }
        },
        Act::Other => Verdict::NotSupported,
    }
}

/// The actions a node advertises. Every advertised action is one [`check_action`] allows, and
/// every action it allows is advertised, so the tree never offers an action the engine refuses.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.click == allowed(t, Act::Click),
        r.focus == allowed(t, Act::Focus),
        r.set_value == allowed(t, Act::SetValue),
        r.scroll_into_view == allowed(t, Act::ScrollIntoView),
        r.scroll == allowed(t, Act::ScrollDown),
        r.scroll == allowed(t, Act::ScrollUp),
))]
pub fn advertised(t: Target) -> Advertised {
    Advertised {
        click: !t.hidden && t.activatable && !t.disabled,
        focus: !t.hidden && t.focusable && !t.disabled,
        set_value: !t.hidden && t.takes_value && !t.disabled && !t.readonly && !t.secret,
        scroll_into_view: !t.hidden,
        scroll: !t.hidden && t.scrolls,
    }
}

/// Where to cut a string of `len` bytes so that it fits in `max` bytes. The caller moves the cut
/// back to a character boundary, which only makes it shorter.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= max,
        r <= len,
        len <= max ==> r == len,
))]
pub fn clip_len(len: usize, max: usize) -> usize {
    if len <= max { len } else { max }
}

/// `text` clipped to [`MAX_STRING_BYTES`] at a character boundary.
#[cfg_attr(verus_keep_ghost, verifier::external_body)]
pub fn clip_str(text: &str) -> &str {
    let mut cut = clip_len(text.len(), MAX_STRING_BYTES);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_values_never_leave() {
        assert_eq!(exposed_value(Secrecy::Secret, "hunter2"), None);
        assert_eq!(exposed_value(Secrecy::Open, "milk"), Some("milk"));
        assert_eq!(secrecy(true, false), Secrecy::Secret);
        assert_eq!(secrecy(false, true), Secrecy::Secret);
        assert_eq!(secrecy(false, false), Secrecy::Open);
    }

    #[test]
    fn autocomplete_tokens() {
        assert!(autocomplete_token_is_secret("cc-number"));
        assert!(autocomplete_token_is_secret("One-Time-Code"));
        assert!(autocomplete_token_is_secret("current-password"));
        assert!(!autocomplete_token_is_secret("email"));
        assert!(!autocomplete_token_is_secret("cc-name"));
        assert!(!autocomplete_token_is_secret(""));
    }

    #[test]
    fn actions_on_a_password_field() {
        let field = Target {
            takes_value: true,
            focusable: true,
            activatable: true,
            secret: true,
            ..Default::default()
        };
        assert_eq!(check_action(field, Act::SetValue), Verdict::Secret);
        assert_eq!(check_action(field, Act::Focus), Verdict::Allow);
        assert!(!advertised(field).set_value);
    }

    #[test]
    fn advertised_matches_check() {
        for bits in 0u32..256 {
            let t = Target {
                activatable: bits & 1 != 0,
                focusable: bits & 2 != 0,
                takes_value: bits & 4 != 0,
                disabled: bits & 8 != 0,
                readonly: bits & 16 != 0,
                secret: bits & 32 != 0,
                scrolls: bits & 64 != 0,
                hidden: bits & 128 != 0,
            };
            let ad = advertised(t);
            assert_eq!(ad.click, check_action(t, Act::Click) == Verdict::Allow);
            assert_eq!(ad.focus, check_action(t, Act::Focus) == Verdict::Allow);
            assert_eq!(
                ad.set_value,
                check_action(t, Act::SetValue) == Verdict::Allow
            );
            assert_eq!(
                ad.scroll,
                check_action(t, Act::ScrollDown) == Verdict::Allow
            );
            assert_eq!(
                check_action(t, Act::Other),
                if t.hidden {
                    Verdict::Hidden
                } else {
                    Verdict::NotSupported
                }
            );
        }
    }

    #[test]
    fn clipping() {
        assert_eq!(clip_str("short"), "short");
        let long = "é".repeat(MAX_STRING_BYTES); // two bytes each
        let clipped = clip_str(&long);
        assert!(clipped.len() <= MAX_STRING_BYTES);
        assert!(clipped.len() >= MAX_STRING_BYTES - 1);
    }
}
