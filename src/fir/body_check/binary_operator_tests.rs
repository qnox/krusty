use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_semantics, root_expression,
};
use super::*;
use crate::fir::{FirBody, FirConstant};
use crate::types::EqualityMode;

/// The only built-in binary operation in `body`, as `(operation, lhs, rhs)`.
fn binary(body: &FirBody, operation: FirBinaryOperation) -> (FirExprId, FirExprId) {
    let operands = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match expression.kind {
            FirExprKind::Binary {
                operation: found,
                lhs,
                rhs,
            }
            | FirExprKind::Equality {
                operation: found,
                lhs,
                rhs,
                ..
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

fn assert_numeric_conversion_to(body: &FirBody, expression: FirExprId, target: Ty) {
    assert!(
        matches!(
            kind(body, expression),
            FirExprKind::ImplicitConversion {
                conversion: FirConversion {
                    kind: FirConversionKind::NumericConversion { to },
                    ..
                },
                ..
            } if to.get() == target
        ),
        "expected a checked numeric conversion to {target:?}, found {:?}",
        kind(body, expression)
    );
}

#[test]
fn a_selected_numeric_conditional_converts_each_result_edge_in_checked_fir() {
    let (body, _) = checked_function_body(
        "fun bits(v: Long, flag: Boolean): Long = v or if (flag) 128 else 0\n",
        "bits",
    );
    let (_, rhs) = binary(&body, FirBinaryOperation::BitwiseOr);
    let FirExprKind::Conditional {
        then_branch,
        then_conversion: None,
        else_branch,
        else_conversion: None,
        ..
    } = kind(&body, rhs)
    else {
        panic!("the selected Long operand must remain a checked conditional")
    };
    assert_eq!(ty(&body, rhs), Ty::Long);
    assert_numeric_conversion_to(&body, *then_branch, Ty::Long);
    assert_numeric_conversion_to(&body, *else_branch, Ty::Long);

    let (body, _) = checked_function_body(
        "fun bits(v: Long, n: Int): Long = v or when (n) { 0 -> 128; else -> 0 }\n",
        "bits",
    );
    let (_, rhs) = binary(&body, FirBinaryOperation::BitwiseOr);
    let FirExprKind::When { branches, .. } = kind(&body, rhs) else {
        panic!("the selected Long operand must remain a checked when")
    };
    assert_eq!(ty(&body, rhs), Ty::Long);
    assert_eq!(branches.len(), 2);
    for branch in branches {
        assert_numeric_conversion_to(&body, branch.result, Ty::Long);
    }
}

/// `UInt? == UInt` stays a source-order equality. The non-null operand is a carrier in a
/// parameter and a box in a function value, so the checker must not unbox it up front.
#[test]
fn unsigned_nullable_equality_keeps_both_operands() {
    let (body, _) =
        checked_function_body("fun equal(a: UInt?, b: UInt): Boolean = a == b\n", "equal");
    let kind = &body
        .expr(root_expression(&body))
        .expect("checked equality")
        .kind;
    assert!(
        matches!(
            kind,
            FirExprKind::Equality {
                operation: FirBinaryOperation::Equal,
                mode: EqualityMode::Structural,
                ..
            }
        ),
        "unsigned equality must keep its operands, found {kind:?}"
    );
}

/// `Int? == Int` still unboxes the nullable operand before the primitive comparison.
#[test]
fn signed_nullable_equality_unboxes_the_primitive() {
    let (body, _) =
        checked_function_body("fun equal(a: Int?, b: Int): Boolean = a == b\n", "equal");
    let kind = &body
        .expr(root_expression(&body))
        .expect("checked equality")
        .kind;
    assert!(
        matches!(
            kind,
            FirExprKind::NullablePrimitiveComparison {
                nullable_first: true,
                ..
            }
        ),
        "a nullable Int still compares as a primitive, found {kind:?}"
    );
}

/// `<A : Double, B : Double?>` compares as a primitive `Double` against a nullable `Double`.
#[test]
fn double_type_parameter_equality_uses_the_floating_bound() {
    let (body, _) = checked_function_body(
        "fun <A : Double, B : Double?> equal(a: A, b: B): Boolean = a == b\n",
        "equal",
    );
    let kind = &body
        .expr(root_expression(&body))
        .expect("checked equality")
        .kind;
    let FirExprKind::NullablePrimitiveComparison {
        operation,
        primitive_ty,
        nullable_first,
        ..
    } = kind
    else {
        panic!("a non-null Double bound against Double? must unbox, found {kind:?}")
    };
    assert_eq!(*operation, FirBinaryOperation::Equal);
    assert_eq!(primitive_ty.get(), Ty::Double);
    assert!(!nullable_first);
}

/// Two `Double?` type parameters compare as nullable doubles, including null.
#[test]
fn nullable_double_type_parameters_compare_as_nullable_doubles() {
    let (body, _) = checked_function_body(
        "fun <A : Double?, B : Double?> equal(a: A, b: B): Boolean = a == b\n",
        "equal",
    );
    let kind = &body
        .expr(root_expression(&body))
        .expect("checked equality")
        .kind;
    let FirExprKind::NullableNumericComparison {
        lhs_primitive,
        rhs_primitive,
        comparison,
        ..
    } = kind
    else {
        panic!("two Double? bounds must compare as nullable doubles, found {kind:?}")
    };
    assert_eq!(lhs_primitive.get(), Ty::Double);
    assert_eq!(rhs_primitive.get(), Ty::Double);
    assert_eq!(comparison.get(), Ty::Double);
}

/// `<B : Any>` is not a floating bound, so equality stays structural `equals`.
#[test]
fn double_type_parameter_against_any_stays_structural_equality() {
    let (body, _) = checked_function_body(
        "fun <A : Double, B : Any> equal(a: A, b: B): Boolean = a == b\n",
        "equal",
    );
    let kind = &body
        .expr(root_expression(&body))
        .expect("checked equality")
        .kind;
    assert!(
        matches!(
            kind,
            FirExprKind::Equality {
                operation: FirBinaryOperation::Equal,
                mode: EqualityMode::Structural,
                ..
            }
        ),
        "Any must select structural equality, found {kind:?}"
    );
}

/// The checker records IEEE equality for a static `Double` or `Float`, including a parameter
/// bounded by one, and structural equality for `Comparable<Double>`.
#[test]
fn source_equality_mode_follows_the_static_operand_types() {
    let cases = [
        (
            "fun equal(a: Double, b: Double): Boolean = a == b\n",
            EqualityMode::Ieee754,
            false,
        ),
        (
            "fun equal(a: Float, b: Float): Boolean = a == b\n",
            EqualityMode::Ieee754,
            false,
        ),
        (
            "fun <T : Double> equal(a: T, b: T): Boolean = a == b\n",
            EqualityMode::Ieee754,
            false,
        ),
        (
            "fun equal(a: Int, b: Int): Boolean = a == b\n",
            EqualityMode::Primitive,
            false,
        ),
        (
            "fun <T : Comparable<Double>> equal(a: T, b: T): Boolean = a == b\n",
            EqualityMode::Structural,
            true,
        ),
        (
            "fun equal(a: Comparable<Double>, b: Comparable<Double>): Boolean = a == b\n",
            EqualityMode::Structural,
            true,
        ),
    ];
    for (source, mode, platform) in cases {
        let (body, _) = if platform {
            checked_function_body_with_platform(source, "equal", jvm_semantics())
        } else {
            checked_function_body(source, "equal")
        };
        let kind = &body
            .expr(root_expression(&body))
            .expect("checked equality")
            .kind;
        assert!(
            matches!(
                kind,
                FirExprKind::Equality {
                    operation: FirBinaryOperation::Equal,
                    mode: found,
                    ..
                } if *found == mode
            ),
            "{source} selected {kind:?}, expected {mode:?}"
        );
    }
}
