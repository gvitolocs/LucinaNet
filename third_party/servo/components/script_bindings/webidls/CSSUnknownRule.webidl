/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
/*
 * An at-rule the engine understands well enough to keep in a style sheet, but for which
 * there is no rule interface yet. `CSSRule.type` is UNKNOWN_RULE (0) for it, per
 * https://drafts.csswg.org/cssom/#dom-cssrule-type
 */

[Exposed=Window]
interface CSSUnknownRule : CSSRule {
};
