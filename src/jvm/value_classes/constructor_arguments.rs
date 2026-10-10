//! Constructor parameter traversal after default-argument mapping.

use crate::types::Ty;

/// The parameters that have supplied arguments. Prefix parameters are never defaultable; defaults
/// index only the declaration-owned suffix.
pub(super) fn supplied_parameters<'a>(
    parameters: &'a [Ty],
    defaults: &'a [u32],
    prefix_count: u32,
) -> impl Iterator<Item = &'a Ty> {
    let prefix_count = prefix_count as usize;
    parameters
        .iter()
        .enumerate()
        .filter_map(move |(parameter, ty)| {
            (parameter < prefix_count
                || !defaults.contains(&u32::try_from(parameter - prefix_count).ok()?))
            .then_some(ty)
        })
}

/// The boxes a generated deserialization constructor call needs for its value-class `arguments`.
/// That constructor takes every value-class element as its box, while each decoded local holds the
/// carrier the plugin recorded as the argument's physical type. A nullable element's null is the
/// absent value and stays null; a non-null element's carrier is always a value (`S(null)` for a
/// nullable carrier) and is boxed as it is.
pub(super) fn deserialization_argument_boxes(
    repr_ctx: &super::ReprCtx<'_>,
    arguments: &[crate::ir::ExprId],
    parameters: &[Ty],
    under: &super::Under,
) -> Vec<(crate::ir::ExprId, super::BoxOp)> {
    arguments
        .iter()
        .zip(parameters)
        .filter_map(|(&argument, &parameter)| {
            let value_class = parameter
                .non_null()
                .obj_internal()
                .filter(|classifier| under.contains_key(classifier))?;
            let carried = repr_ctx.unboxed_value_class(argument, under) == Some(value_class)
                || recorded_carrier(
                    repr_ctx.physical.get(&argument).copied(),
                    parameter,
                    value_class,
                    under,
                );
            let operation = if parameter.is_nullable() {
                super::BoxOp::BoxNull(value_class)
            } else {
                super::BoxOp::Box(value_class)
            };
            carried.then_some((argument, operation))
        })
        .collect()
}

/// Whether the plugin explicitly recorded exactly the carrier this parameter normalizes to. A
/// missing/Error/unrelated physical type is never evidence that an invalid intermediate state is a
/// carrier merely because it is not the value-class box.
fn recorded_carrier(
    physical: Option<Ty>,
    parameter: Ty,
    value_class: crate::types::TypeName,
    under: &super::Under,
) -> bool {
    let Some(physical) = physical.filter(|physical| *physical != Ty::Error) else {
        return false;
    };
    let mut carrier = super::erase(&under[&value_class], under);
    if parameter.is_nullable() {
        carrier = Ty::nullable(carrier);
    }
    physical.canonical_semantic() == carrier.canonical_semantic()
}

#[cfg(test)]
mod tests {
    use super::recorded_carrier;
    use crate::types::{type_name, Ty};

    #[test]
    fn only_the_exact_normalized_carrier_proves_a_generated_argument_is_unboxed() {
        let value_class = type_name("sample/Label");
        let declarations = super::super::Under::from([(value_class, Ty::String)]);
        assert!(recorded_carrier(
            Some(Ty::String),
            Ty::obj_name(value_class),
            value_class,
            &declarations,
        ));
        assert!(recorded_carrier(
            Some(Ty::nullable(Ty::String)),
            Ty::nullable(Ty::obj_name(value_class)),
            value_class,
            &declarations,
        ));
        assert!(!recorded_carrier(
            Some(Ty::obj_name(value_class)),
            Ty::obj_name(value_class),
            value_class,
            &declarations,
        ));
        assert!(!recorded_carrier(
            Some(Ty::Int),
            Ty::obj_name(value_class),
            value_class,
            &declarations,
        ));
        assert!(!recorded_carrier(
            Some(Ty::Error),
            Ty::obj_name(value_class),
            value_class,
            &declarations,
        ));
        assert!(!recorded_carrier(
            None,
            Ty::obj_name(value_class),
            value_class,
            &declarations,
        ));

        let nullable_carrier_class = type_name("sample/Stamp");
        let declarations =
            super::super::Under::from([(nullable_carrier_class, Ty::nullable(Ty::String))]);
        assert!(recorded_carrier(
            Some(Ty::nullable(Ty::String)),
            Ty::obj_name(nullable_carrier_class),
            nullable_carrier_class,
            &declarations,
        ));
    }
}
