use super::test_support::{checked_function_body, root_expression};
use super::*;
use crate::fir::{FirBody, FirConstant};

/// The only built-in binary operation in `body`, as `(operation, lhs, rhs)`.
fn binary(body: &FirBody, operation: FirBinaryOperation) -> (FirExprId, FirExprId) {
    let operands = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match expression.kind {
            FirExprKind::Binary {
                operation: found,
                lhs,
                rhs,
            } if found == operation => Some((lhs, rhs)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [operands] = operands.as_slice() else {
        panic!("expected exactly one {operation:?}, found {operands:?}")
    };
    *operands
}

fn kind(body: &FirBody, expression: FirExprId) -> &FirExprKind {
    &body.expr(expression).expect("checked operand").kind
}

fn ty(body: &FirBody, expression: FirExprId) -> Ty {
    body.expr(expression).expect("checked operand").ty.get()
}

/// Two different smart-cast primitives compare at their common numeric type: `Int == Long` widens
/// the `Int` side to `Long`, the comparison kotlinc emits as `i2l; lcmp`.
#[test]
fn mixed_primitive_equality_compares_at_the_promoted_type() {
    let (body, _) = checked_function_body(
        "fun equal(a: Any, b: Any): Boolean = a is Int && b is Long && a == b\n",
        "equal",
    );
    let (lhs, rhs) = binary(&body, FirBinaryOperation::Equal);
    assert!(matches!(
        kind(&body, lhs),
        FirExprKind::ImplicitConversion {
            conversion: FirConversion {
                kind: FirConversionKind::NumericWidening { to },
                ..
            },
            ..
        } if to.get() == Ty::Long
    ));
    assert_eq!((ty(&body, lhs), ty(&body, rhs)), (Ty::Long, Ty::Long));
}

/// A comparison widens a constant operand at compile time: `d < 1.0F` compares against the
/// `Double` constant `1.0`, and `a == 3` on a smart-cast `Long` against the `Long` constant `3`.
#[test]
fn a_comparison_widens_a_constant_operand_to_a_constant() {
    for (source, function, operation, expected) in [
        (
            "fun less(d: Double): Boolean = d < 1.0F\n",
            "less",
            FirBinaryOperation::Less,
            FirConstant::Double(1.0),
        ),
        (
            "fun equal(a: Any): Boolean = a is Long && a == 3\n",
            "equal",
            FirBinaryOperation::Equal,
            FirConstant::Long(3),
        ),
        (
            "fun differ(a: Any): Boolean = a is Double && a != 0.5F\n",
            "differ",
            FirBinaryOperation::NotEqual,
            FirConstant::Double(0.5),
        ),
    ] {
        let (body, _) = checked_function_body(source, function);
        let (_, rhs) = binary(&body, operation);
        assert_eq!(
            kind(&body, rhs),
            &FirExprKind::Constant(expected),
            "{source}"
        );
    }
}

/// Arithmetic selects the overload taking the operand's own type, so its widening stays a runtime
/// conversion: `d + 1.0F` is kotlinc's `fconst_1; f2d; dadd`.
#[test]
fn arithmetic_keeps_a_constant_operands_runtime_widening() {
    let (body, _) = checked_function_body("fun add(d: Double): Double = d + 1.0F\n", "add");
    let root = body.expr(root_expression(&body)).expect("checked body");
    assert_eq!(root.ty.get(), Ty::Double);
    let (_, rhs) = binary(&body, FirBinaryOperation::Add);
    let FirExprKind::ImplicitConversion { value, conversion } = kind(&body, rhs) else {
        panic!("the Float operand must keep its conversion")
    };
    assert!(matches!(
        conversion.kind,
        FirConversionKind::NumericWidening { to } if to.get() == Ty::Double
    ));
    assert_eq!(
        kind(&body, *value),
        &FirExprKind::Constant(FirConstant::Float(1.0))
    );
}
