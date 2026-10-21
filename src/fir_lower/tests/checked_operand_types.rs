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
    let ir = lower_single_source_with_jvm_stdlib(
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
        [
            (Some(Ty::Int), Some(Ty::Int)),
            (Some(Ty::Boolean), Some(Ty::Boolean)),
        ]
    );
    assert_eq!(
        ir.exprs
            .iter()
            .filter(|expression| matches!(expression, IrExpr::Equality { .. }))
            .count(),
        0,
        "generated comparisons must keep their primitive contract instead of acquiring source equality modes"
    );
}

/// Membership is realized after FIR lowering, as kotlinc's `RangeContainsLowering` runs after
/// checking: `1 <= value && value < 4` on the JVM, whose constant inclusive bound becomes exclusive.
#[test]
fn realized_range_membership_records_the_comparison_type() {
    let mut ir = lower_single_source_with_jvm_stdlib(
        "fun contains(value: Int): Boolean = value in 1..3\n",
        "CheckedRangeMembershipOperands",
    );
    use crate::backend::counted_loops::{
        realize, CountedLoopPolicy, CounterLoopStyle, HeaderInlining, UntilSteps,
    };
    realize(
        &mut ir,
        CountedLoopPolicy {
            style: CounterLoopStyle::JavaLike,
            inlining: HeaderInlining::Kotlinc,
            until_steps: UntilSteps::Built,
        },
    );

    assert_eq!(
        primitive_operands(&ir, IrBinOp::Le),
        [(Some(Ty::Int), Some(Ty::Int))]
    );
    assert_eq!(
        primitive_operands(&ir, IrBinOp::Lt),
        [(Some(Ty::Int), Some(Ty::Int))]
    );
}

#[test]
fn a_negated_containment_call_records_its_boolean_operand() {
    let ir = lower_single_source_with_jvm_stdlib(
        "fun outside(value: Int, range: IntRange): Boolean = value !in range\n",
        "CheckedNegatedContainmentOperand",
    );
    let negations = ir.negations.iter().copied().collect::<Vec<_>>();
    assert_eq!(negations.len(), 1);
    let IrExpr::PrimitiveBinOp { lhs, rhs, .. } = ir.expr(negations[0]) else {
        panic!("a recorded negation must remain a primitive comparison");
    };

    assert_eq!(
        (ir.checked_type(*lhs), ir.checked_type(*rhs)),
        (Some(Ty::Boolean), Some(Ty::Boolean))
    );
}
