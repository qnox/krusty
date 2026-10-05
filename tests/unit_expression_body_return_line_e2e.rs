//! fir2ir's `ExpressionBodyTransformer` builds an expression body's return at the expression's end
//! offset, so a `Unit` function written `= expression` marks the expression's END line on its
//! `return` once the expression has run — a declared, member or local function alike. A body on
//! one line already has that line in effect and gets no new entry.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      fun sink(x: Int) {}\n\
                      \n\
                      fun deliver(block: () -> Unit) {\n\
                      \x20   block()\n\
                      }\n\
                      \n\
                      fun lambdaBody(n: Int) = deliver {\n\
                      \x20   sink(n)\n\
                      }\n\
                      \n\
                      fun callBody(n: Int) = sink(\n\
                      \x20   n\n\
                      )\n\
                      \n\
                      fun branchBody(n: Int) = if (n > 0)\n\
                      \x20   sink(n)\n\
                      else\n\
                      \x20   sink(-n)\n\
                      \n\
                      fun singleLine(n: Int) = sink(n)\n\
                      \n\
                      fun localBody(n: Int) {\n\
                      \x20   fun inner(m: Int) = sink(\n\
                      \x20       m\n\
                      \x20   )\n\
                      \x20   inner(n)\n\
                      }\n\
                      \n\
                      class Holder(val n: Int) {\n\
                      \x20   fun member() = sink(\n\
                      \x20       n\n\
                      \x20   )\n\
                      }\n";

#[test]
fn unit_expression_body_returns_are_byte_identical_to_kotlinc() {
    for class in ["store/UnitExpressionBodyKt", "store/Holder"] {
        common::byte_diff_against_kotlinc_cp(
            "UnitExpressionBody",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
