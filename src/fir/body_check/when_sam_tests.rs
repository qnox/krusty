//! A fun-interface expectation converts the lambda a `when` arm actually yields.

use super::test_support::{
    checked_function_body, root_expression, try_checked_function_body_rewriting,
};
use super::*;
use crate::libraries::EmptySymbolSource;

fn sam_method(body: &FirBody, value: FirExprId) -> String {
    let expression = body.expr(value).expect("checked branch value");
    match &expression.kind {
        FirExprKind::ImplicitConversion { conversion, .. } => {
            let FirConversionKind::Sam(sam) = conversion.kind else {
                panic!("branch value must be a SAM conversion, not {conversion:?}")
            };
            let sam = body.sam_conversion(sam).expect("recorded SAM conversion");
            sam.method.as_ref().to_owned()
        }
        FirExprKind::Block {
            result: Some(result),
            ..
        } => sam_method(body, *result),
        other => panic!("branch value must convert to the fun interface, got {other:?}"),
    }
}

fn when_branch_methods(source: &str) -> Vec<String> {
    let (body, _) = checked_function_body(source, "select");
    let FirExprKind::When { branches, .. } = &body.expr(root_expression(&body)).expect("when").kind
    else {
        panic!("select must be a when")
    };
    branches
        .iter()
        .map(|branch| sam_method(&body, branch.result))
        .collect()
}

#[test]
fn a_when_arm_lambda_converts_to_the_expected_fun_interface() {
    let methods = when_branch_methods(
        "class Text(val n: Int)\n\
         fun interface Read { fun read(text: Text): Int }\n\
         fun select(flag: Boolean): Read = when {\n\
             flag -> { text -> text.n }\n\
             else -> { text -> text.n }\n\
         }\n",
    );
    assert_eq!(methods, ["read", "read"].map(str::to_owned));
}

#[test]
fn a_when_arm_block_converts_its_trailing_lambda() {
    let methods = when_branch_methods(
        "class Text(val n: Int)\n\
         fun interface Read { fun read(text: Text): Int }\n\
         fun select(flag: Boolean): Read = when {\n\
             flag -> {\n\
                 { it.n }\n\
             }\n\
             else -> {\n\
                 { it.n }\n\
             }\n\
         }\n",
    );
    assert_eq!(methods, ["read", "read"].map(str::to_owned));
}

const SELECT: &str = "class Text(val n: Int)\n\
     fun interface Read { fun read(text: Text): Int }\n\
     fun select(flag: Boolean): Read = when {\n\
         flag -> { text -> text.n }\n\
         else -> { text -> text.n }\n\
     }\n";

fn first_lambda_span(file: &crate::ast::File) -> Option<crate::diag::Span> {
    (0..file.expr_arena.len()).find_map(|raw| {
        let id = crate::ast::ExprId(raw as u32);
        matches!(file.expr(id), Expr::Lambda { .. }).then(|| file.expr_span(id))?
    })
}

fn assert_unpublished(failure: BodyCheckFailure, span: Option<crate::diag::Span>) {
    assert_eq!(
        (failure.span, failure.kind),
        (span, BodyCheckFailureKind::UnpublishedRecordedSamConversion)
    );
}

/// A recorded conversion whose target is still a function type must not publish the lambda.
#[test]
fn a_recorded_sam_conversion_with_a_function_target_is_a_frontend_error() {
    let mut lambda_span = None;
    let failure = try_checked_function_body_rewriting(
        SELECT,
        "select",
        Box::new(EmptySymbolSource),
        &crate::features::LangFeatures::new(),
        |file, info| {
            lambda_span = first_lambda_span(file);
            let when_index = (0..file.expr_arena.len())
                .find(|&raw| matches!(file.expr(crate::ast::ExprId(raw as u32)), Expr::When { .. }))
                .expect("select is a when");
            info.expr_types[when_index] = Ty::fun(vec![Ty::Int], Ty::Int);
        },
    )
    .expect_err("a function-typed target cannot publish the recorded conversion");
    assert_unpublished(failure, lambda_span);
}

/// A recorded conversion that does not materialize must not publish the lambda either.
#[test]
fn a_recorded_sam_conversion_that_does_not_materialize_is_a_frontend_error() {
    let _omit = super::arguments::OmitRecordedSamPublication::arm();
    let mut lambda_span = None;
    let failure = try_checked_function_body_rewriting(
        SELECT,
        "select",
        Box::new(EmptySymbolSource),
        &crate::features::LangFeatures::new(),
        |file, _info| {
            lambda_span = first_lambda_span(file);
        },
    )
    .expect_err("a missing publication cannot leave the original function value");
    assert_unpublished(failure, lambda_span);
}
