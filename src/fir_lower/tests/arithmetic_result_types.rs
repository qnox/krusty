//! The result type common lowering records on every arithmetic `PrimitiveBinOp`.
//!
//! Kotlin's mixed numeric promotion is a property of the operator the checker selected:
//! `Int.plus(Long)` is declared to return `Long`, `Char.plus(Int)` to return `Char` and
//! `Char.minus(Char)` to return `Int`. Common lowering records that declared result on the node, so
//! no backend re-derives it from the operands' types.

use super::*;

/// The recorded result of each `op` node in `source`, in the order lowering produced them.
fn recorded_results(source: &str, stem: &str, op: IrBinOp) -> Vec<Option<Ty>> {
    let ir = lower_single_source(source, stem);
    assert_eq!(ir.unrecorded_arithmetic_result(), None, "{stem}");
    ir.exprs
        .iter()
        .enumerate()
        .filter(
            |(_, node)| matches!(node, IrExpr::PrimitiveBinOp { op: found, .. } if *found == op),
        )
        .map(|(id, _)| ir.logical_types.get(&(id as u32)).copied())
        .collect()
}

#[test]
fn a_mixed_binary_operator_records_the_selected_operators_result() {
    for (source, op, expected) in [
        ("fun f(a: Int, b: Long) = a + b\n", IrBinOp::Add, Ty::Long),
        ("fun f(a: Long, b: Int) = a * b\n", IrBinOp::Mul, Ty::Long),
        (
            "fun f(a: Int, b: Double) = a / b\n",
            IrBinOp::Div,
            Ty::Double,
        ),
        (
            "fun f(a: Float, b: Long) = a - b\n",
            IrBinOp::Sub,
            Ty::Float,
        ),
        ("fun f(a: Char, b: Int) = a + b\n", IrBinOp::Add, Ty::Char),
        ("fun f(a: Char, b: Int) = a - b\n", IrBinOp::Sub, Ty::Char),
        ("fun f(a: Char, b: Char) = a - b\n", IrBinOp::Sub, Ty::Int),
        ("fun f(a: Byte, b: Byte) = a + b\n", IrBinOp::Add, Ty::Int),
        ("fun f(a: Short, b: Byte) = a * b\n", IrBinOp::Mul, Ty::Int),
        ("fun f(a: Byte, b: Long) = a % b\n", IrBinOp::Rem, Ty::Long),
        ("fun f(a: Long) = a shl 3\n", IrBinOp::Shl, Ty::Long),
        (
            "fun f(a: Int, b: Int) = a xor b\n",
            IrBinOp::BitXor,
            Ty::Int,
        ),
        (
            "fun f(a: Boolean, b: Boolean) = a and b\n",
            IrBinOp::BitAnd,
            Ty::Boolean,
        ),
    ] {
        assert_eq!(
            recorded_results(source, "MixedBinary", op),
            [Some(expected)],
            "{source}"
        );
    }
}

#[test]
fn a_compound_assignment_records_the_selected_operators_result() {
    for (source, op, expected) in [
        (
            "fun f(a: Long): Long { var x = a; x += 1; return x }\n",
            IrBinOp::Add,
            Ty::Long,
        ),
        (
            "fun f(c: Char): Char { var x = c; x += 2; return x }\n",
            IrBinOp::Add,
            Ty::Char,
        ),
        (
            "fun f(c: Char): Char { var x = c; x -= 2; return x }\n",
            IrBinOp::Sub,
            Ty::Char,
        ),
        (
            "fun f(d: Double): Double { var x = d; x *= 3; return x }\n",
            IrBinOp::Mul,
            Ty::Double,
        ),
    ] {
        assert_eq!(
            recorded_results(source, "CompoundAssignment", op),
            [Some(expected)],
            "{source}"
        );
    }
}

/// `x++` is lowered to an addition of a delta in the type Kotlin promotes `x` to, then a coercion
/// back to `inc`'s own result: the addition's type is the producer's, and it is recorded too.
#[test]
fn an_increment_records_the_promoted_addition() {
    for (source, expected) in [
        (
            "fun f(c: Char): Char { var x = c; x++; return x }\n",
            Ty::Int,
        ),
        (
            "fun f(b: Byte): Byte { var x = b; x++; return x }\n",
            Ty::Int,
        ),
        (
            "fun f(s: Short): Short { var x = s; --x; return x }\n",
            Ty::Int,
        ),
        (
            "fun f(l: Long): Long { var x = l; x++; return x }\n",
            Ty::Long,
        ),
        (
            "fun f(d: Double): Double { var x = d; x--; return x }\n",
            Ty::Double,
        ),
    ] {
        assert_eq!(
            recorded_results(source, "Increment", IrBinOp::Add),
            [Some(expected)],
            "{source}"
        );
    }
}

#[test]
fn an_arithmetic_node_without_a_recorded_result_is_an_incomplete_fact() {
    let mut ir = IrFile::default();
    let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
    let two = ir.add_expr(IrExpr::Const(IrConst::Long(2)));
    ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Lt,
        lhs: one,
        rhs: two,
    });
    ir.add_arithmetic(IrBinOp::Add, one, two, Ty::Long);
    assert_eq!(ir.unrecorded_arithmetic_result(), None);
    let unrecorded = ir.add_expr(IrExpr::PrimitiveBinOp {
        op: IrBinOp::Sub,
        lhs: one,
        rhs: two,
    });
    assert_eq!(ir.unrecorded_arithmetic_result(), Some(unrecorded));
    assert_eq!(
        ir.validate_complete_facts(crate::fir::SourceFileId::from_raw(0)),
        Err(crate::ir::IncompleteIrFact::ArithmeticResult(unrecorded))
    );
}
