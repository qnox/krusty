//! Checked-FIR publication of selected generic substitutions.

use super::test_support::{checked_function_body_rewriting, root_expression};
use super::*;

#[test]
fn sparse_call_substitution_keeps_the_later_parameters_stable_identity() {
    let (body, index) = checked_function_body_rewriting(
        "fun <A, B> choose(value: B): B = value\n\
         fun make(): String = choose<Int, String>(\"OK\")\n",
        "make",
        Box::new(crate::libraries::EmptySymbolSource),
        &crate::features::LangFeatures::new(),
        |file, info| {
            let call = file
                .expr_arena
                .iter()
                .enumerate()
                .find_map(|(raw, expression)| {
                    let crate::ast::Expr::Call { callee, .. } = expression else {
                        return None;
                    };
                    matches!(file.expr(*callee), crate::ast::Expr::Name(name) if name == "choose")
                        .then_some(ExprId(raw as u32))
                })
                .expect("generic call");
            info.replace_selected_call_type_arguments_for_test(call, vec![None, Some(Ty::String)]);
        },
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("generic call")
        .kind
    else {
        panic!("selected generic function must become checked call FIR")
    };
    let declaration = index
        .callable(call.target.module().expect("stable source target"))
        .expect("source callable")
        .declaration;
    let [substitution] = call.substitutions.as_ref() else {
        panic!(
            "only the later materialized substitution must cross checked FIR: {:?}",
            call.substitutions
        )
    };
    assert_eq!(
        Some(substitution.parameter),
        index.type_parameter(declaration, 1).map(Into::into),
    );
    assert_eq!(substitution.value.get(), Ty::String);
}
