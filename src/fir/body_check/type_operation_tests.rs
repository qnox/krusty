use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_stdlib_semantics,
    root_expression,
};
use super::*;

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
                operation: FirTypeOperation::Is,
                target: actual,
                ..
            } if actual.get() == target
        ));
        assert_eq!(expression.ty.get(), Ty::Boolean);
    }
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
    assert_eq!(
        operations,
        vec![
            (FirTypeOperation::NotIs, target, Ty::Boolean),
            (FirTypeOperation::Cast, target, target),
        ]
    );
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
