//! Checked operand types on value nodes synthesized after FIR.

use super::*;

fn primitive_operands(ir: &IrFile, op: IrBinOp) -> Vec<(Option<Ty>, Option<Ty>)> {
    ir.exprs
        .iter()
        .filter_map(|expression| {
            let IrExpr::PrimitiveBinOp {
                op: found,
                lhs,
                rhs,
            } = expression
            else {
                return None;
            };
            (*found == op).then(|| (ir.checked_type(*lhs), ir.checked_type(*rhs)))
        })
        .collect()
}

#[test]
fn a_comparison_call_records_the_nested_compare_result() {
    let ir = lower_single_source(
        "fun less(a: Comparable<Int>, b: Int): Boolean = a < b\n",
        "CheckedComparisonOperands",
    );

    assert_eq!(
        primitive_operands(&ir, IrBinOp::Lt),
        [(Some(Ty::Int), Some(Ty::Int))]
    );
}

#[test]
fn generated_data_class_equality_records_declared_field_types() {
    let ir = lower_single_source(
        "data class Count(val value: Int)\n",
        "CheckedDataClassOperands",
    );

    assert_eq!(
        primitive_operands(&ir, IrBinOp::Eq),
        [(Some(Ty::Int), Some(Ty::Int))]
    );
}

#[test]
fn generated_range_membership_records_the_comparison_type() {
    let ir = lower_single_source(
        "fun contains(value: Int): Boolean = value in 1..3\n",
        "CheckedRangeMembershipOperands",
    );

    assert_eq!(
        primitive_operands(&ir, IrBinOp::Le),
        [
            (Some(Ty::Int), Some(Ty::Int)),
            (Some(Ty::Int), Some(Ty::Int)),
        ]
    );
}
