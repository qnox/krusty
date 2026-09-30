use super::super::test_support::{
    checked_function_body_with_platform, jvm_stdlib_semantics, root_expression,
};
use super::super::*;

#[test]
fn omitted_source_call_vararg_with_a_declared_default_uses_that_default() {
    let (body, _) = checked_function_body_with_platform(
        "class Element\n\
         fun join(vararg values: Element = arrayOf(Element(), Element())): Int = 0\n\
         fun read(): Int = join()\n",
        "read",
        jvm_stdlib_semantics(),
    );
    let FirExprKind::Call(call) = &body
        .expr(root_expression(&body))
        .expect("defaulted vararg call")
        .kind
    else {
        panic!("omitted vararg with a declared default must become checked call FIR")
    };
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Default { parameter: 0, .. }]
    ));
}
