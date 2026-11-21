use crate::{
    intrinsic_methods,
    runtime::{
        BigIntValue, Context, Handle, Value,
        abstract_operations::construct,
        alloc_error::AllocResult,
        error::{range_error, type_error},
        eval_result::EvalResult,
        intrinsic_builder::IntrinsicBuilder,
        intrinsics::{
            bigint_object::BigIntObject, intl::number_format_object::NumberFormatObject,
            intrinsics::Intrinsic,
        },
        object_value::ObjectValue,
        realm::Realm,
        type_utilities::to_integer_or_infinity,
    },
    runtime_fn,
};

pub struct BigIntPrototype;

impl BigIntPrototype {
    /// Properties of the BigInt Prototype Object (https://tc39.es/ecma262/#sec-properties-of-the-bigint-prototype-object)
    pub fn new(cx: Context, realm: Handle<Realm>) -> AllocResult<Handle<ObjectValue>> {
        let mut builder = IntrinsicBuilder::new_object(cx, realm, Intrinsic::ObjectPrototype)?;

        // Constructor property is added once BigIntConstructor has been created
        intrinsic_methods!(cx, builder, {
            to_locale_string BigIntPrototype_to_locale_string (0),
            to_string        BigIntPrototype_to_string        (0),
            value_of         BigIntPrototype_value_of         (0),
        });

        // BigInt.prototype [ @@toStringTag ] (https://tc39.es/ecma262/#sec-bigint.prototype-%symbol.tostringtag%)
        builder.to_string_tag(cx.names.bigint())?;

        builder.build()
    }

    runtime_fn! {
    /// BigInt.prototype.toLocaleString ( https://tc39.es/ecma402/#sup-bigint.prototype.tolocalestring ),
    /// 20.3.1, which supersedes the ECMA-262 definition. ECMA-262 has no `toLocaleString` on
    /// %BigInt.prototype% at all, so this is the method being added rather than a body being replaced.
    fn to_locale_string(cx, this_value, arguments) {
        // Step 1: ? ThisBigIntValue(this value).
        let bigint = this_bigint_value(cx, this_value, "toLocaleString")?;

        // Step 2: ? Construct(%Intl.NumberFormat%, « locales, options »), the intrinsic so that a
        // replaced `Intl.NumberFormat` is the one that gets asked.
        let number_format = construct(
            cx,
            cx.get_intrinsic(Intrinsic::NumberFormatConstructor),
            &[arguments.get(cx, 0), arguments.get(cx, 1)],
            None,
        )?;

        let Some(number_format) = number_format.as_opt::<NumberFormatObject>() else {
            // Step 3 reaches FormatNumeric, which reads the slots a NumberFormat has.
            return type_error(
                cx,
                "BigInt.prototype.toLocaleString needs Intl.NumberFormat to build a NumberFormat",
            );
        };

        // Step 3: FormatNumeric (16.5.5) with ℝ(x). The BigInt branch of ToIntlMathematicalValue
        // (16.5.15 step 2) is that ℝ, read from the number's own digits rather than through a double.
        let x = NumberFormatObject::to_intl_mathematical_value(cx, bigint.into())?;
        let text = number_format.format(cx, &x)?;

        Ok(cx.alloc_string(&text)?.as_value())
    }}

    runtime_fn! {
    /// BigInt.prototype.toString (https://tc39.es/ecma262/#sec-bigint.prototype.tostring)
    fn to_string(cx, this_value, arguments) {
        let bigint_value = this_bigint_value(cx, this_value, "toString")?;

        let radix = arguments.get(cx, 0);
        let radix = if radix.is_undefined() {
            10
        } else {
            let radix_int = to_integer_or_infinity(cx, radix)?;
            if !(2.0..=36.0).contains(&radix_int) {
                return range_error(
                    cx,
                    "BigInt.prototype.toString radix must be an integer between 2 and 36",
                );
            }

            radix_int as u32
        };

        Ok(cx
            .alloc_string(&bigint_value.bigint().to_str_radix(radix))?
            .as_value())
    }}

    runtime_fn! {
    /// BigInt.prototype.valueOf (https://tc39.es/ecma262/#sec-bigint.prototype.valueof)
    fn value_of(cx, this_value, _) {
        Ok(this_bigint_value(cx, this_value, "valueOf")?.into())
    }}
}

fn this_bigint_value(
    cx: Context,
    value: Handle<Value>,
    method_name: &str,
) -> EvalResult<Handle<BigIntValue>> {
    if value.is_bigint() {
        return Ok(value.as_bigint());
    }

    if let Some(bigint_object) = value.as_opt::<BigIntObject>() {
        return Ok(bigint_object.bigint_data());
    }

    type_error(cx, &format!("BigInt.prototype.{method_name} must be called on a BigInt"))
}
