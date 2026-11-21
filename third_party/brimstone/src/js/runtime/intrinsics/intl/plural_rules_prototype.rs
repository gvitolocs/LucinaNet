//! The `Intl.PluralRules.prototype` object (https://tc39.es/ecma402/#sec-properties-of-the-intl-pluralrules-prototype-object),
//! 17.3: `select`, `selectRange` and `resolvedOptions`, and the tag that makes an instance report
//! "[object Intl.PluralRules]".
//!
//! The instance, the reading of its options and the plural rules themselves are
//! [`super::plural_rules_object`]; this file is only the methods a program calls.

use super::number_format_object::NumberFormatObject;
use super::number_format_prototype::{report_number, report_str, report_value};
use super::plural_rules_object::PluralRulesObject;
use crate::runtime::{
    Context, Handle, Realm, abstract_operations::create_data_property_or_throw,
    alloc_error::AllocResult, array_object::create_array_from_list, error::type_error,
    intrinsic_builder::IntrinsicBuilder, intrinsics::intrinsics::Intrinsic,
    object_value::ObjectValue, ordinary_object::ordinary_object_create,
};
use crate::{intrinsic_methods, runtime_fn};

/// %Intl.PluralRules.prototype% (https://tc39.es/ecma402/#sec-properties-of-the-intl-pluralrules-prototype-object),
/// 17.3.
pub struct PluralRulesPrototype;

impl PluralRulesPrototype {
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        // 17.3: an ordinary object that is not an instance, so it has no [[InitializedPluralRules]]
        // and every method below refuses it.
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        intrinsic_methods!(cx, builder, {
            resolved_options PluralRulesPrototype_resolved_options (0),
            select PluralRulesPrototype_select (1),
            select_range PluralRulesPrototype_select_range (2),
        });

        // 17.3.5: @@toStringTag, the String "Intl.PluralRules", non-writable and configurable,
        // which is what `builder.to_string_tag` installs.
        builder.to_string_tag(cx.names.intl_plural_rules())?;

        builder.build()
    }

    runtime_fn! {
    /// Intl.PluralRules.prototype.select ( https://tc39.es/ecma402/#sec-intl.pluralrules.prototype.select ), 17.3.3.
    fn select(cx, this_value, arguments) {
        // Steps 1 and 2: `RequireInternalSlot(pr, [[InitializedPluralRules]])`.
        let plural_rules = PluralRulesObject::require_initialized(cx, this_value)?;

        // Step 3: ? ToIntlMathematicalValue(value), 16.5.15, the same operation NumberFormat's
        // methods use - which is why it lives there.
        let n = NumberFormatObject::to_intl_mathematical_value(cx, arguments.get(cx, 0))?;

        // Step 4: ResolvePlural (17.5.2), for its [[PluralCategory]].
        let category = plural_rules.resolve_plural(cx, &n)?;

        Ok(cx.alloc_string(category)?.as_value())
    }}

    runtime_fn! {
    /// Intl.PluralRules.prototype.selectRange ( https://tc39.es/ecma402/#sec-intl.pluralrules.prototype.selectrange ), 17.3.4.
    fn select_range(cx, this_value, arguments) {
        // Steps 1 and 2.
        let plural_rules = PluralRulesObject::require_initialized(cx, this_value)?;

        let start = arguments.get(cx, 0);
        let end = arguments.get(cx, 1);

        // Step 3: an omitted endpoint is the error, before either value is converted - a getter on
        // the second argument therefore does not run for a call with one argument.
        if start.is_undefined() || end.is_undefined() {
            return type_error(cx, "Intl.PluralRules.selectRange needs a start and an end");
        }

        // Steps 4 and 5.
        let x = NumberFormatObject::to_intl_mathematical_value(cx, start)?;
        let y = NumberFormatObject::to_intl_mathematical_value(cx, end)?;

        // Step 6: ResolvePluralRange (17.5.4).
        let category = plural_rules.resolve_plural_range(cx, &x, &y)?;

        Ok(cx.alloc_string(category)?.as_value())
    }}

    runtime_fn! {
    /// Intl.PluralRules.prototype.resolvedOptions ( https://tc39.es/ecma402/#sec-intl.pluralrules.prototype.resolvedoptions ), 17.3.2.
    fn resolved_options(cx, this_value, _) {
        // Steps 1 and 2.
        let plural_rules = PluralRulesObject::require_initialized(cx, this_value)?;

        let Some(resolved) = plural_rules.resolved() else {
            // The question `require_initialized` has already answered.
            return type_error(cx, "Intl.PluralRules method called on an uninitialized instance");
        };

        // [[Locale]] is a raw pointer inside the instance's own data, so it goes into a handle here,
        // before anything else allocates: a collection during step 3 would move the string and leave
        // the copy above pointing at the block it was copied out of.
        let locale = resolved.locale.to_handle(cx);

        // Step 3: `OrdinaryObjectCreate(%Object.prototype%)`.
        let options = ordinary_object_create(cx)?;

        // Steps 4 and 5, in the order Table 32 lists the slots. A slot this build does not have is
        // left off rather than reported as `undefined`, which is what step 5.4's "if value is not
        // undefined" means: the two digit pairs the rounding type did not choose, and
        // `compactDisplay`, which 17.1.1 step 12 stores only for the notation this build refuses.
        report_value(cx, options, cx.names.locale(), locale)?;
        report_str(cx, options, cx.names.type_(), resolved.settings.kind.as_str())?;
        report_str(cx, options, cx.names.notation(), "standard")?;
        report_number(
            cx,
            options,
            cx.names.minimum_integer_digits(),
            resolved.slots.minimum_integer_digits,
        )?;
        if resolved.slots.reports_fraction_digits {
            report_number(
                cx,
                options,
                cx.names.minimum_fraction_digits(),
                resolved.slots.minimum_fraction_digits,
            )?;
            report_number(
                cx,
                options,
                cx.names.maximum_fraction_digits(),
                resolved.slots.maximum_fraction_digits,
            )?;
        }
        if resolved.slots.reports_significant_digits {
            report_number(
                cx,
                options,
                cx.names.minimum_significant_digits(),
                resolved.slots.minimum_significant_digits,
            )?;
            report_number(
                cx,
                options,
                cx.names.maximum_significant_digits(),
                resolved.slots.maximum_significant_digits,
            )?;
        }

        // Step 5.2.1, for the row whose Property is "pluralCategories": the list of step 4, which is
        // the locale's rules read in the order UTS 35 fixes.
        let categories = plural_rules.plural_categories(cx)?;
        let mut elements = Vec::with_capacity(categories.len());
        for category in &categories {
            elements.push(cx.alloc_string(category)?.as_value());
        }
        create_data_property_or_throw(
            cx,
            options,
            cx.names.plural_categories(),
            create_array_from_list(cx, &elements)?.into(),
        )?;

        report_number(
            cx,
            options,
            cx.names.rounding_increment(),
            resolved.slots.rounding_increment.as_u32(),
        )?;
        report_str(cx, options, cx.names.rounding_mode(), resolved.slots.rounding_mode.as_str())?;
        // [[ComputedRoundingPriority]], which is the "roundingPriority" of the option a caller would
        // have to pass to get the same rounding again.
        report_str(
            cx,
            options,
            cx.names.rounding_priority(),
            resolved.slots.rounding_priority.as_str(),
        )?;
        report_str(
            cx,
            options,
            cx.names.trailing_zero_display(),
            resolved.slots.trailing_zero_display.as_str(),
        )?;

        // Step 6.
        Ok(options.as_value())
    }}
}
