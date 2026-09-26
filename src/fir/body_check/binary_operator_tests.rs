use super::test_support::checked_function_body;
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

/// A constant operand of a mixed-width operation keeps its checked `NumericWidening` to the promoted
/// type, for comparisons and arithmetic alike. Whether a backend widens it at compile time or at
/// runtime is that backend's representation choice.
#[test]
fn a_constant_operand_keeps_its_checked_widening() {
    for (source, function, operation, constant, target) in [
        (
            "fun less(d: Double): Boolean = d < 1.0F\n",
            "less",
            FirBinaryOperation::Less,
            FirConstant::Float(1.0),
            Ty::Double,
        ),
        (
            "fun equal(a: Any): Boolean = a is Long && a == 3\n",
            "equal",
            FirBinaryOperation::Equal,
            FirConstant::Int(3),
            Ty::Long,
        ),
        (
            "fun differ(a: Any): Boolean = a is Double && a != 0.5F\n",
            "differ",
            FirBinaryOperation::NotEqual,
            FirConstant::Float(0.5),
            Ty::Double,
        ),
        (
            "fun add(d: Double): Double = d + 1.0F\n",
            "add",
            FirBinaryOperation::Add,
            FirConstant::Float(1.0),
            Ty::Double,
        ),
    ] {
        let (body, _) = checked_function_body(source, function);
        let (_, rhs) = binary(&body, operation);
        let FirExprKind::ImplicitConversion { value, conversion } = kind(&body, rhs) else {
            panic!("the constant must keep its conversion: {source}")
        };
        assert!(
            matches!(
                conversion.kind,
                FirConversionKind::NumericWidening { to } if to.get() == target
            ),
            "{source}"
        );
        assert_eq!(
            kind(&body, *value),
            &FirExprKind::Constant(constant),
            "{source}"
        );
    }
}
