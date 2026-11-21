/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Pure HTML-AAM / WAI-ARIA role mapping tables.
//!
//! This file depends only on `accesskit` so it can be compiled and tested on its own.
//! It follows the W3C HTML-AAM element-to-role table as Chromium implements it
//! (third_party/blink/renderer/modules/accessibility/ax_node_object.cc,
//! `NativeRoleIgnoringAria`), and WAI-ARIA 1.2 for ARIA role tokens.

use accesskit::Role;

/// Facts about the element's surroundings that change its role (HTML-AAM's "scoped to" and attribute rules).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HtmlRoleContext {
    /// The element has an accessible name (aria-label, aria-labelledby or title). Changes <section>, <form>.
    pub has_accessible_name: bool,
    /// An ancestor is <article>, <aside>, <main>, <nav> or <section> (or has one of their ARIA roles).
    /// Changes <header>/<footer>.
    pub in_sectioning_content: bool,
    /// The nearest ancestor <select> is shown as a list box (multiple, or size > 1). Changes <option>.
    pub in_list_box_select: bool,
    /// The nearest ancestor <table> has role grid/treegrid. Changes <td>.
    pub in_grid: bool,
    /// For <th>: the header cell is a row header (scope=row, or first in its row with no scope and the column is
    /// not a header column). The caller computes this; you just map it.
    pub is_row_header: bool,
}

/// The role HTML-AAM gives an HTML element, ignoring any `role` attribute.
/// `tag` is the lower-case local name; `attr(name)` returns the attribute's value if present.
pub fn html_role(tag: &str, attr: &dyn Fn(&str) -> Option<String>, ctx: &HtmlRoleContext) -> Role {
    match tag {
        "a" | "area" => {
            if attr("href").is_some() {
                Role::Link
            } else {
                Role::GenericContainer
            }
        },
        "abbr" => Role::Abbr,
        // HTML-AAM maps <address> to generic; AccessKit has no closer role.
        "address" => Role::GenericContainer,
        "article" => Role::Article,
        "aside" => Role::Complementary,
        "b" | "i" | "u" | "span" => Role::GenericContainer,
        // HTML-AAM maps <bdi>/<bdo> to generic.
        "bdi" | "bdo" => Role::GenericContainer,
        "blockquote" => Role::Blockquote,
        "body" => Role::RootWebArea,
        "br" => Role::LineBreak,
        "button" => Role::Button,
        "canvas" => Role::Canvas,
        "caption" => Role::Caption,
        // HTML-AAM maps <cite> to generic.
        "cite" => Role::GenericContainer,
        "code" => Role::Code,
        // HTML-AAM maps <data> to generic.
        "data" => Role::GenericContainer,
        "datalist" => Role::ListBox,
        "dd" => Role::Definition,
        "del" => Role::ContentDeletion,
        "details" => Role::Details,
        "dfn" => Role::Term,
        "dialog" => Role::Dialog,
        "div" => Role::GenericContainer,
        "dl" => Role::DescriptionList,
        "dt" => Role::Term,
        "em" => Role::Emphasis,
        "embed" => Role::EmbeddedObject,
        "fieldset" => Role::Group,
        "figcaption" => Role::FigureCaption,
        "figure" => Role::Figure,
        "footer" => {
            if ctx.in_sectioning_content {
                Role::GenericContainer
            } else {
                Role::ContentInfo
            }
        },
        // HTML-AAM says <form> is a form even without an accessible name, as Chromium implements it.
        "form" => Role::Form,
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Role::Heading,
        "header" => {
            if ctx.in_sectioning_content {
                Role::GenericContainer
            } else {
                Role::Banner
            }
        },
        "hgroup" => Role::Group,
        "hr" => Role::Splitter,
        // HTML-AAM maps <html> to generic; the web area role belongs to <body>.
        "html" => Role::GenericContainer,
        "iframe" => Role::Iframe,
        "img" => {
            // alt="" with no title makes the image presentational.
            let alt = attr("alt");
            let presentational = alt.as_deref() == Some("") && attr("title").is_none();
            if presentational {
                Role::GenericContainer
            } else {
                Role::Image
            }
        },
        "input" => input_role(attr),
        "ins" => Role::ContentInsertion,
        "label" => Role::Label,
        "legend" => Role::Legend,
        "li" => Role::ListItem,
        "main" => Role::Main,
        "mark" => Role::Mark,
        "menu" => Role::List,
        "meter" => Role::Meter,
        "nav" => Role::Navigation,
        "ol" | "ul" => Role::List,
        "optgroup" => Role::Group,
        "option" => {
            if ctx.in_list_box_select {
                Role::ListBoxOption
            } else {
                Role::MenuListOption
            }
        },
        "output" => Role::Status,
        "p" => Role::Paragraph,
        // HTML-AAM maps <pre> to generic.
        "pre" => Role::GenericContainer,
        "progress" => Role::ProgressIndicator,
        // HTML-AAM maps <q> to generic.
        "q" => Role::GenericContainer,
        "rp" | "rt" => Role::GenericContainer,
        "ruby" => Role::Ruby,
        // HTML-AAM maps <s> and <samp> to generic.
        "s" | "samp" => Role::GenericContainer,
        "search" => Role::Search,
        "section" => {
            if ctx.has_accessible_name {
                Role::Region
            } else {
                Role::GenericContainer
            }
        },
        "select" => {
            let multiple = attr("multiple").is_some();
            let size_gt_1 = attr("size")
                .and_then(|s| s.trim().parse::<u32>().ok())
                .is_some_and(|n| n > 1);
            if multiple || size_gt_1 {
                Role::ListBox
            } else {
                Role::ComboBox
            }
        },
        // HTML-AAM maps <small> to generic.
        "small" => Role::GenericContainer,
        "strong" => Role::Strong,
        // AccessKit has no Subscript/Superscript roles; HTML-AAM maps these to generic.
        "sub" | "sup" => Role::GenericContainer,
        "summary" => Role::DisclosureTriangle,
        // Chromium exposes an <svg> root as a graphics document.
        "svg" => Role::GraphicsDocument,
        "table" => Role::Table,
        "tbody" | "thead" | "tfoot" => Role::RowGroup,
        "td" => {
            if ctx.in_grid {
                Role::GridCell
            } else {
                Role::Cell
            }
        },
        "textarea" => Role::MultilineTextInput,
        "th" => {
            if ctx.is_row_header {
                Role::RowHeader
            } else {
                Role::ColumnHeader
            }
        },
        "time" => Role::Time,
        "tr" => Role::Row,
        "video" => Role::Video,
        "audio" => Role::Audio,
        _ => Role::GenericContainer,
    }
}

/// The role for an `<input>` element, keyed by its `type` attribute.
/// A missing or unknown type defaults to text, compared ASCII case-insensitively.
fn input_role(attr: &dyn Fn(&str) -> Option<String>) -> Role {
    let ty = attr("type").unwrap_or_default();
    let ty = ty.trim();
    let matches = |name: &str| ty.eq_ignore_ascii_case(name);
    if matches("button") ||
        matches("submit") ||
        matches("reset") ||
        matches("image") ||
        matches("file")
    {
        // Chromium maps input type=image and type=file to a plain button.
        Role::Button
    } else if matches("checkbox") {
        Role::CheckBox
    } else if matches("radio") {
        Role::RadioButton
    } else if matches("range") {
        Role::Slider
    } else if matches("number") {
        Role::SpinButton
    } else if matches("search") {
        Role::SearchInput
    } else if matches("email") {
        Role::EmailInput
    } else if matches("tel") {
        Role::PhoneNumberInput
    } else if matches("url") {
        Role::UrlInput
    } else if matches("password") {
        Role::PasswordInput
    } else if matches("date") {
        Role::DateInput
    } else if matches("datetime-local") {
        Role::DateTimeInput
    } else if matches("month") {
        Role::MonthInput
    } else if matches("week") {
        Role::WeekInput
    } else if matches("time") {
        Role::TimeInput
    } else if matches("color") {
        Role::ColorWell
    } else if matches("hidden") {
        Role::GenericContainer
    } else {
        // type=text, missing, or unknown: a text field. With a `list` attribute
        // Chromium exposes it as an editable combo box.
        if attr("list").is_some() {
            Role::EditableComboBox
        } else {
            Role::TextInput
        }
    }
}

/// The role for one ARIA `role` token (lower case), or None if the token is not a
/// WAI-ARIA 1.2 / dpub / graphics role we map. The caller takes the first token
/// that maps, as ARIA's fallback roles say.
pub fn aria_role(token: &str) -> Option<Role> {
    Some(match token {
        "alert" => Role::Alert,
        "alertdialog" => Role::AlertDialog,
        "application" => Role::Application,
        "article" => Role::Article,
        "banner" => Role::Banner,
        "blockquote" => Role::Blockquote,
        "button" => Role::Button,
        "caption" => Role::Caption,
        "cell" => Role::Cell,
        "checkbox" => Role::CheckBox,
        "code" => Role::Code,
        "columnheader" => Role::ColumnHeader,
        "combobox" => Role::ComboBox,
        "complementary" => Role::Complementary,
        "contentinfo" => Role::ContentInfo,
        "definition" => Role::Definition,
        "deletion" => Role::ContentDeletion,
        "dialog" => Role::Dialog,
        // ARIA directory is a list of links; AccessKit has no closer role.
        "directory" => Role::List,
        "document" => Role::Document,
        "emphasis" => Role::Emphasis,
        "feed" => Role::Feed,
        "figure" => Role::Figure,
        "form" => Role::Form,
        // Upstream Servo maps "generic" to Role::Section; we follow Chromium and map
        // it to GenericContainer.
        "generic" => Role::GenericContainer,
        "grid" => Role::Grid,
        "gridcell" => Role::GridCell,
        "group" => Role::Group,
        "heading" => Role::Heading,
        "img" | "image" => Role::Image,
        "insertion" => Role::ContentInsertion,
        "link" => Role::Link,
        "list" => Role::List,
        "listbox" => Role::ListBox,
        "listitem" => Role::ListItem,
        "log" => Role::Log,
        "main" => Role::Main,
        "marquee" => Role::Marquee,
        "math" => Role::Math,
        "menu" => Role::Menu,
        "menubar" => Role::MenuBar,
        "menuitem" => Role::MenuItem,
        "menuitemcheckbox" => Role::MenuItemCheckBox,
        "menuitemradio" => Role::MenuItemRadio,
        "meter" => Role::Meter,
        "navigation" => Role::Navigation,
        "none" | "presentation" => Role::GenericContainer,
        "note" => Role::Note,
        "option" => Role::ListBoxOption,
        "paragraph" => Role::Paragraph,
        "progressbar" => Role::ProgressIndicator,
        "radio" => Role::RadioButton,
        "radiogroup" => Role::RadioGroup,
        "region" => Role::Region,
        "row" => Role::Row,
        "rowgroup" => Role::RowGroup,
        "rowheader" => Role::RowHeader,
        "scrollbar" => Role::ScrollBar,
        "search" => Role::Search,
        "searchbox" => Role::SearchInput,
        "separator" => Role::Splitter,
        "slider" => Role::Slider,
        "spinbutton" => Role::SpinButton,
        "status" => Role::Status,
        "strong" => Role::Strong,
        // AccessKit has no Subscript/Superscript roles; the closest is a generic container.
        "subscript" | "superscript" => Role::GenericContainer,
        "switch" => Role::Switch,
        "tab" => Role::Tab,
        "table" => Role::Table,
        "tablist" => Role::TabList,
        "tabpanel" => Role::TabPanel,
        "term" => Role::Term,
        "textbox" => Role::TextInput,
        "time" => Role::Time,
        "timer" => Role::Timer,
        "toolbar" => Role::Toolbar,
        "tooltip" => Role::Tooltip,
        "tree" => Role::Tree,
        "treegrid" => Role::TreeGrid,
        "treeitem" => Role::TreeItem,
        _ => return None,
    })
}

/// Roles whose accessible name comes from their content (WAI-ARIA 1.2
/// "Roles Supporting Name from Content"). Label/Legend/Caption/FigureCaption are
/// named from content too in Chromium for their own name.
pub fn name_from_contents(role: Role) -> bool {
    matches!(
        role,
        Role::Button |
            Role::Cell |
            Role::CheckBox |
            Role::ColumnHeader |
            Role::GridCell |
            Role::Heading |
            Role::Link |
            Role::MenuItem |
            Role::MenuItemCheckBox |
            Role::MenuItemRadio |
            Role::ListBoxOption |
            Role::MenuListOption |
            Role::RadioButton |
            Role::Row |
            Role::RowHeader |
            Role::Switch |
            Role::Tab |
            Role::Tooltip |
            Role::TreeItem |
            Role::DisclosureTriangle |
            Role::Label |
            Role::Legend |
            Role::Caption |
            Role::FigureCaption
    )
}

/// Roles a person can act on (for tests and for the caller deciding default actions):
/// links, buttons, all inputs, options, menu items, tabs, tree items, switches,
/// sliders, spin buttons, combo boxes, list boxes, disclosure triangles.
pub fn is_interactive(role: Role) -> bool {
    matches!(
        role,
        Role::Link |
            Role::Button |
            Role::DefaultButton |
            Role::TextInput |
            Role::MultilineTextInput |
            Role::SearchInput |
            Role::EmailInput |
            Role::PhoneNumberInput |
            Role::UrlInput |
            Role::PasswordInput |
            Role::DateInput |
            Role::DateTimeInput |
            Role::TimeInput |
            Role::NumberInput |
            Role::SpinButton |
            Role::ComboBox |
            Role::EditableComboBox |
            Role::ListBox |
            Role::CheckBox |
            Role::RadioButton |
            Role::Switch |
            Role::Slider |
            Role::ColorWell |
            Role::ListBoxOption |
            Role::MenuListOption |
            Role::MenuItem |
            Role::MenuItemCheckBox |
            Role::MenuItemRadio |
            Role::Tab |
            Role::TreeItem |
            Role::DisclosureTriangle
    )
}
