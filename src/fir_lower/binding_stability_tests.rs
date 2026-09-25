use crate::fir::FirRangeOperation;
use crate::ir::{IrBindingStability, IrCheckedOperation, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

#[test]
fn immutable_binding_read_survives_clone_and_inline_rehome() {
    let mut ir = IrFile::default();
    let read = ir.add_expr(IrExpr::GetValue(0));
    ir.binding_read_stability
        .insert(read, IrBindingStability::Stable);
    let cast = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::CastNonNull,
        arg: read,
        type_operand: Ty::String,
    });

    let (copy, identities) = crate::ir::clone_expression_dag(&mut ir, cast);
    let copied_read = identities[&read];
    assert_eq!(
        ir.binding_read_stability.get(&copied_read),
        Some(&IrBindingStability::Stable)
    );

    assert_eq!(
        super::source_calls::rehome_inline_body_values(&mut ir, copy, &[37], 100),
        Some(0)
    );
    assert!(matches!(ir.expr(copied_read), IrExpr::GetValue(37)));
    assert_eq!(
        ir.binding_read_stability.get(&copied_read),
        Some(&IrBindingStability::Stable)
    );
}

#[test]
fn inline_rehome_moves_a_checked_range_loops_declared_value() {
    let mut ir = IrFile::default();
    let start = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
    let end = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(3)));
    let body = ir.add_expr(IrExpr::UnitInstance);
    let range = ir.add_expr(IrExpr::Checked(IrCheckedOperation::RangeLoop {
        variable: 2,
        variable_name: Some("element".into()),
        counter: Ty::Int,
        operation: FirRangeOperation::Until,
        start,
        end,
        body,
        label: "loop".to_string(),
    }));

    assert_eq!(
        super::source_calls::rehome_inline_body_values(&mut ir, range, &[37], 100),
        Some(2)
    );
    assert!(matches!(
        ir.expr(range),
        IrExpr::Checked(IrCheckedOperation::RangeLoop { variable: 101, .. })
    ));
}
