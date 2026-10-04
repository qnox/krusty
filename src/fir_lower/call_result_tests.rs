use crate::ir::{Callee, IrExpr, IrTypeOp};
use crate::types::Ty;

use super::tests::lower_single_source;

#[test]
fn generic_call_result_boundary_keeps_declared_and_selected_types() {
    let ir = lower_single_source(
        "interface ResultShape\n\
         class SelectedResult : ResultShape\n\
         class OtherResult : ResultShape\n\
         fun <T> produce(): T = OtherResult() as T\n\
         fun nested(flag: Boolean): ResultShape =\n\
             if (flag) (if (flag) produce() else SelectedResult()) else OtherResult()\n",
        "GenericCallResultBoundary",
    );

    let boundaries = ir
        .declaration_result_coercions
        .iter()
        .filter_map(|&coercion| {
            let IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } = ir.expr(coercion)
            else {
                return None;
            };
            let IrExpr::Call {
                callee: Callee::Local(function),
                ..
            } = ir.expr(*arg)
            else {
                return None;
            };
            (ir.functions[*function as usize].name == "produce").then_some((
                *arg,
                *type_operand,
                ir.call_declared_ret.get(arg).copied(),
            ))
        })
        .collect::<Vec<_>>();

    assert_eq!(boundaries.len(), 1, "{boundaries:?}");
    let (call, selected, declared) = boundaries[0];
    assert_eq!(selected, Ty::obj("SelectedResult"));
    assert!(matches!(declared, Some(Ty::TyParam(_, _))));
    assert_eq!(ir.logical_types.get(&call), None);
    assert_eq!(
        ir.logical_types.get(
            &ir.declaration_result_coercions
                .iter()
                .copied()
                .find(|&id| { matches!(ir.expr(id), IrExpr::TypeOp { arg, .. } if *arg == call) })
                .expect("produce result boundary")
        ),
        Some(&Ty::obj("SelectedResult"))
    );
}
