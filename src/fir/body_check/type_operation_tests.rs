use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_stdlib_semantics,
    root_expression,
};
use super::*;
use crate::fir::FirTypeTestPlan;

#[test]
fn scalar_and_unit_type_tests_retain_checked_fir_operations() {
    for (source, function, target) in [
        (
            "fun testLong(value: Long): Boolean = value is Long\n",
            "testLong",
            Ty::Long,
        ),
        (
            "fun testUnit(value: Unit): Boolean = value is Unit\n",
            "testUnit",
            Ty::Unit,
        ),
    ] {
        let (body, _) = checked_function_body(source, function);
        let expression = body.expr(root_expression(&body)).expect("type test");
        assert!(matches!(
            &expression.kind,
            FirExprKind::TypeOperation {
                operation: FirTypeOperation::Is(_),
                target: actual,
                ..
            } if actual.get() == target
        ));
        assert_eq!(expression.ty.get(), Ty::Boolean);
    }
}

#[test]
fn function_type_tests_publish_provider_normalized_erased_bound_matches() {
    for (source, expected) in [
        (
            "fun test(value: (() -> Any?)?) = value is Function0<*>\n",
            true,
        ),
        (
            "fun test(value: (suspend () -> Any?)?) = value is kotlin.coroutines.SuspendFunction0<*>\n",
            true,
        ),
        (
            "fun test(value: (() -> Any?)?) = value is kotlin.coroutines.SuspendFunction0<*>\n",
            false,
        ),
        (
            "fun test(value: (() -> Any?)?) = value is Function1<*, *>\n",
            false,
        ),
        (
            "fun test(value: Function0<*>?) = value is Function0<*>\n",
            true,
        ),
        (
            "fun test(value: Function0<*>?) = value is Function1<*, *>\n",
            false,
        ),
    ] {
        let (body, _) = checked_function_body_with_platform(source, "test", jvm_stdlib_semantics());
        let expression = body.expr(root_expression(&body)).expect("type test");
        assert!(matches!(
            &expression.kind,
            FirExprKind::TypeOperation {
                operation: FirTypeOperation::Is(FirTypeTestPlan { same_erased_upper_bound }),
                ..
            } if *same_erased_upper_bound == expected
        ), "source: {source}");
    }
}

#[test]
fn nullable_mixed_numeric_equality_publishes_exact_comparison_types() {
    let (body, _) = checked_function_body(
        "fun equal(left: Int?, right: Double?): Boolean = left == right\n",
        "equal",
    );
    let FirExprKind::NullableNumericComparison {
        operation,
        lhs_primitive,
        rhs_primitive,
        comparison,
        ..
    } = body
        .expr(root_expression(&body))
        .expect("checked nullable numeric equality")
        .kind
    else {
        panic!("mixed nullable numbers must use their checked equality operation")
    };
    assert_eq!(operation, FirBinaryOperation::Equal);
    assert_eq!(lhs_primitive.get(), Ty::Int);
    assert_eq!(rhs_primitive.get(), Ty::Double);
    assert_eq!(comparison.get(), Ty::Double);
}

#[test]
fn bare_generic_typealias_runtime_operands_are_checked_as_star_expansions() {
    let (body, _) = checked_function_body_with_platform(
        "// WITH_STDLIB\n\
         typealias L<T> = List<T>\n\
         fun test(value: Collection<Int>): List<*> {\n\
             if (value !is L) return emptyList()\n\
             return value as L\n\
         }\n",
        "test",
        jvm_stdlib_semantics(),
    );
    let target = Ty::obj_args(
        "kotlin/collections/List",
        &[Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))],
    );
    let operations = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .filter_map(|expression| match &expression.kind {
            FirExprKind::TypeOperation {
                operation, target, ..
            } => Some((*operation, target.get(), expression.ty.get())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        operations.as_slice(),
        [
            (FirTypeOperation::NotIs(_), first_target, Ty::Boolean),
            (FirTypeOperation::Cast, second_target, result),
        ] if *first_target == target && *second_target == target && *result == target
    ));
}

#[test]
fn bare_generic_typealias_type_test_recovers_arguments_from_the_subject_supertype() {
    let (body, _) = checked_function_body(
        "sealed class C<out T, out U>\n\
         class B<out U>(val value: U) : C<Nothing, U>()\n\
         typealias Z<U> = B<U>\n\
         fun source(): C<Int, String> = B(\"OK\")\n\
         fun test(): String {\n\
             val value = source()\n\
             if (value is Z) return value.value\n\
             return \"fail\"\n\
         }\n",
        "test",
    );

    assert_eq!(
        body.expr(root_expression(&body))
            .expect("checked function body")
            .ty
            .get(),
        Ty::Nothing,
    );
}
