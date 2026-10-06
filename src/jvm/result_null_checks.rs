//! Null checks over an erased call result.
//!
//! A generic call returns its JVM erasure (`Object` for `Cell<String>.read()`), and checked IR
//! narrows that slot to the substituted type with a coercion directly over the call. kotlinc checks
//! the value in the slot the call produced and only then casts the checked value, so a `!!` or a
//! platform-type assertion over such a coercion moves beneath it: `checkNotNull` reads the `Object`,
//! and the `checkcast` follows.

use crate::ir::{ExprId, IrExpr, IrFile, IrTypeOp};
use crate::jvm::physical_type::ir_ty_to_jvm;

pub(super) fn check_before_result_coercion(ir: &mut IrFile) {
    materialize_unnamed_checks_before_result_coercion(ir);
    let placements = (0..ir.exprs.len())
        .filter_map(|assertion| {
            let assertion = assertion as ExprId;
            let IrExpr::NotNullAssert { operand, .. } = ir.exprs[assertion as usize] else {
                return None;
            };
            let root = operand;
            let mut coercion = operand;
            while let IrExpr::Block {
                value: Some(value), ..
            } = ir.exprs[coercion as usize]
            {
                coercion = value;
            }
            let IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } = ir.exprs[coercion as usize]
            else {
                return None;
            };
            let call = crate::jvm::call_result_boundaries::terminal_call(&ir.exprs, arg)?;
            let slot = ir_ty_to_jvm(&crate::jvm::call_result_boundaries::checked_result_slot(
                ir, call,
            )?);
            let target = ir_ty_to_jvm(&type_operand);
            (slot.is_reference() && target.is_reference() && slot != target)
                .then_some((assertion, root, coercion))
        })
        .collect::<Vec<_>>();
    for (assertion, root, coercion) in placements {
        let IrExpr::NotNullAssert { check, .. } = ir.exprs[assertion as usize].clone() else {
            unreachable!("a collected placement is a null assertion");
        };
        let IrExpr::TypeOp {
            arg, type_operand, ..
        } = ir.exprs[coercion as usize]
        else {
            unreachable!("a collected placement asserts over a coercion");
        };
        // The assertion keeps its identity as the consumer's operand, now as the coercion; the
        // former coercion node becomes the assertion over the call's own slot.
        ir.exprs[coercion as usize] = IrExpr::NotNullAssert {
            operand: arg,
            check,
        };
        ir.exprs[assertion as usize] = IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: root,
            type_operand,
        };
        if ir.declaration_result_coercions.remove(&coercion) {
            ir.declaration_result_coercions.insert(assertion);
        }
    }
}

/// An unnamed platform check materializes its operand so it can check and then yield the same
/// value. When that operand is a generic call result, the temporary must own the declaration's
/// erased slot, not the call-site narrowing: kotlinc stores `Object`, checks that local, and casts
/// only the final read. Common lowering keeps the whole sequence as one block, which makes this
/// JVM representation choice explicit and unambiguous here.
fn materialize_unnamed_checks_before_result_coercion(ir: &mut IrFile) {
    let placements = (0..ir.exprs.len())
        .filter_map(|block| {
            let IrExpr::Block {
                stmts,
                value: Some(result),
            } = ir.exprs.get(block)?
            else {
                return None;
            };
            let [declaration, assertion] = stmts.as_slice() else {
                return None;
            };
            let IrExpr::Variable {
                index,
                init: Some(coercion),
                named: false,
                ..
            } = ir.exprs.get(*declaration as usize)?
            else {
                return None;
            };
            let IrExpr::NotNullAssert {
                operand: checked,
                check: crate::ir::NullCheck::Unnamed,
            } = ir.exprs.get(*assertion as usize)?
            else {
                return None;
            };
            let IrExpr::GetValue(checked_value) = ir.exprs.get(*checked as usize)? else {
                return None;
            };
            let IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: produced,
                type_operand,
            } = ir.exprs.get(*coercion as usize)?
            else {
                return None;
            };
            if !ir.declaration_result_coercions.contains(coercion) {
                return None;
            }
            let (yielded_value, needs_result_coercion) = match ir.exprs.get(*result as usize)? {
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: yielded,
                    type_operand: yielded_type,
                } if yielded_type == type_operand => {
                    let IrExpr::GetValue(yielded_value) = ir.exprs.get(*yielded as usize)? else {
                        return None;
                    };
                    (*yielded_value, false)
                }
                IrExpr::GetValue(yielded_value) => (*yielded_value, true),
                _ => return None,
            };
            if checked_value != index || yielded_value != *index {
                return None;
            }
            let call = crate::jvm::call_result_boundaries::terminal_call(&ir.exprs, *produced)?;
            let slot = crate::jvm::call_result_boundaries::checked_result_slot(ir, call)?;
            let physical = ir_ty_to_jvm(&slot);
            let target = ir_ty_to_jvm(type_operand);
            (physical.is_reference() && target.is_reference() && physical != target).then_some((
                block as ExprId,
                *declaration,
                *coercion,
                *produced,
                *result,
                *type_operand,
                slot,
                needs_result_coercion,
            ))
        })
        .collect::<Vec<_>>();

    for (
        block,
        declaration,
        coercion,
        produced,
        result,
        type_operand,
        slot,
        needs_result_coercion,
    ) in placements
    {
        let IrExpr::Variable { ty, init, .. } = &mut ir.exprs[declaration as usize] else {
            unreachable!("a collected unnamed check starts with a variable declaration");
        };
        *ty = slot;
        *init = Some(produced);
        if needs_result_coercion {
            let (coerced, _) = crate::ir::clone_expression_dag(ir, result);
            ir.exprs[coerced as usize] = IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: result,
                type_operand,
            };
            let IrExpr::Block { value, .. } = &mut ir.exprs[block as usize] else {
                unreachable!("a collected unnamed check is owned by a block");
            };
            *value = Some(coerced);
        }
        ir.physical_types.insert(produced, slot);
        ir.declaration_result_coercions.remove(&coercion);
    }
}

#[cfg(test)]
mod tests {
    use super::check_before_result_coercion;
    use crate::ir::{Callee, IrExpr, IrFile, IrFunction, IrTypeOp, NullCheck};
    use crate::types::Ty;

    #[test]
    fn an_unnamed_check_casts_a_plain_materialized_result_after_the_check() {
        let mut ir = IrFile::default();
        let body = ir.add_expr(IrExpr::UnitInstance);
        let function = ir.add_fun(IrFunction {
            name: "pick".to_string(),
            params: Vec::new(),
            ret: Ty::obj("kotlin/Any"),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(function),
            dispatch_receiver: None,
            args: Vec::new(),
        });
        ir.call_declared_ret
            .insert(call, Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any"))));
        let set = Ty::obj("kotlin/collections/Set");
        let initial_coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: set,
        });
        ir.declaration_result_coercions.insert(initial_coercion);
        let declaration = ir.add_expr(IrExpr::Variable {
            index: 0,
            ty: set,
            init: Some(initial_coercion),
            named: false,
        });
        let checked = ir.add_expr(IrExpr::GetValue(0));
        let assertion = ir.add_expr(IrExpr::NotNullAssert {
            operand: checked,
            check: NullCheck::Unnamed,
        });
        let result = ir.add_expr(IrExpr::GetValue(0));
        let block = ir.add_expr(IrExpr::Block {
            stmts: vec![declaration, assertion],
            value: Some(result),
        });

        check_before_result_coercion(&mut ir);

        assert!(matches!(
            ir.expr(declaration),
            IrExpr::Variable { ty, init: Some(value), .. }
                if *ty == Ty::nullable(Ty::obj("kotlin/Any")) && *value == call
        ));
        let IrExpr::Block {
            value: Some(coerced),
            ..
        } = ir.expr(block)
        else {
            panic!("the materialization remains a value block");
        };
        assert!(matches!(
            ir.expr(*coerced),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } if *arg == result && *type_operand == set
        ));
        assert!(!ir.declaration_result_coercions.contains(&initial_coercion));
    }
}
