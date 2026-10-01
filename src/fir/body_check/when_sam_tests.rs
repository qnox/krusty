//! A fun-interface expectation converts the lambda a `when` arm actually yields.

use super::test_support::{checked_function_body, root_expression};
use super::*;

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
