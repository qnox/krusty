//! A bare call `name(args)` whose nearest `name` is a lexical value with no applicable `invoke`
//! calls the function of that name instead, as kotlinc's call tower does. An inferred result is
//! computed before the body check, so the signature phase must take the same step.

use super::common;

#[test]
fn an_inferred_result_calls_past_a_lexical_value_without_an_applicable_invoke() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/inferredResultCallsPastLexicalValue.kt"),
        "InferredResultCallsPastLexicalValue",
    );
}
