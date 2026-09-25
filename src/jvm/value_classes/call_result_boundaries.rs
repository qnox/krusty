//! Physical results at checked generic-call boundaries involving a value class.
//!
//! Common lowering retains the selected declaration result, the substituted semantic result, and
//! their coercion. JVM generic erasure runs before this module, but value-class carriers are known
//! only inside value-class lowering. Joining both facts here avoids two incorrect approximations:
//! treating a declared `Tagged<A>` result as its box before its carrier is known, and attaching a
//! value-class representation fact to unrelated generic scalar/reference coercions.

use crate::ir::{IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

use super::{erase, Under};

fn value_class(ty: Ty, underlying: &Under) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(|classifier| underlying.contains_key(&classifier))
}

fn physical_boundary(
    declared: Ty,
    erased_result: Option<Ty>,
    target: Ty,
    underlying: &Under,
) -> Option<Option<Ty>> {
    let declared_value_class = value_class(declared, underlying);
    let target_value_class = value_class(target, underlying);
    if !declared_value_class && !target_value_class {
        return None;
    }

    // A declaration that names a value class returns its carrier. A bare type parameter
    // instantiated with that class instead returns through its already-erased reference slot.
    let physical = if declared_value_class {
        erase(&declared, underlying)
    } else {
        erased_result?
    };
    let target = if target_value_class {
        erase(&target, underlying)
    } else {
        target
    };
    let different_slots =
        crate::jvm::ir_emit::ir_ty_to_jvm(&physical) != crate::jvm::ir_emit::ir_ty_to_jvm(&target);
    // A bare type parameter crosses the erased generic slot as a box even when that slot and the
    // value class's carrier share the same descriptor (for example, `T` and `Slot(Any?)` are both
    // `Object`). Preserve the physical slot as a representation fact; descriptor equality alone
    // cannot express the box/carrier distinction.
    Some((declared.is_ty_param() || different_slots).then_some(physical))
}

pub(super) fn realize(ir: &mut IrFile, underlying: &Under) {
    let mut coercions = ir
        .declaration_result_coercions
        .iter()
        .copied()
        .collect::<Vec<_>>();
    coercions.sort_unstable();
    let boundaries = coercions
        .into_iter()
        .filter_map(|coercion| match ir.exprs.get(coercion as usize)? {
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } => Some((*arg, *type_operand)),
            _ => None,
        })
        .filter_map(|(expression, target)| {
            // A retained inline body has already crossed and removed its declaration ABI; its
            // result slot is specialized to the call-site type.
            if ir.inline_regions.contains(&expression) {
                return None;
            }
            let call = crate::jvm::call_result_boundaries::terminal_call(&ir.exprs, expression)?;
            let declared = *ir.call_declared_ret.get(&call)?;
            let erased = ir.physical_types.get(&expression).copied().or_else(|| {
                crate::jvm::call_result_boundaries::erased_result_slot(ir, call, declared)
            });
            physical_boundary(declared, erased, target, underlying)
                .map(|physical| (expression, physical))
        })
        .collect::<Vec<_>>();

    for (expression, physical) in boundaries {
        if let Some(physical) = physical {
            ir.physical_types.insert(expression, physical);
        } else {
            ir.physical_types.remove(&expression);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    fn carriers() -> Under {
        [
            (type_name("test/Tagged"), Ty::String),
            (type_name("test/Token"), Ty::String),
            (type_name("test/Slot"), Ty::nullable(Ty::obj("kotlin/Any"))),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn a_declared_generic_value_class_result_is_already_its_carrier() {
        let underlying = carriers();
        let parameter = Ty::ty_param("A", Ty::nullable(Ty::obj("kotlin/Any")));
        let declared = Ty::obj_args("test/Tagged", &[parameter]);
        let target = Ty::obj_args("test/Tagged", &[Ty::Int]);

        assert_eq!(
            physical_boundary(declared, None, target, &underlying),
            Some(None)
        );
    }

    #[test]
    fn a_bare_type_parameter_result_keeps_its_erased_box_boundary() {
        let underlying = carriers();
        let declared = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let erased = Ty::nullable(Ty::obj("kotlin/Any"));
        let target = Ty::obj("test/Token");

        assert_eq!(
            physical_boundary(declared, Some(erased), target, &underlying),
            Some(Some(erased))
        );
    }

    #[test]
    fn a_bare_type_parameter_keeps_a_box_when_its_carrier_has_the_same_descriptor() {
        let underlying = carriers();
        let erased = Ty::nullable(Ty::obj("kotlin/Any"));
        let declared = Ty::ty_param("T", erased);

        assert_eq!(
            physical_boundary(declared, Some(erased), Ty::obj("test/Slot"), &underlying),
            Some(Some(erased))
        );
    }

    #[test]
    fn a_declared_value_class_is_boxed_only_after_its_carrier_result() {
        let underlying = carriers();
        let declared = Ty::obj("test/Token");

        assert_eq!(
            physical_boundary(declared, Some(declared), Ty::obj("kotlin/Any"), &underlying),
            Some(Some(Ty::String))
        );
    }

    #[test]
    fn an_unrelated_nullable_generic_scalar_is_not_a_value_class_boundary() {
        let underlying = carriers();
        let declared = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));

        assert_eq!(
            physical_boundary(
                declared,
                Some(Ty::nullable(Ty::obj("kotlin/Any"))),
                Ty::nullable(Ty::Int),
                &underlying,
            ),
            None
        );
    }
}
