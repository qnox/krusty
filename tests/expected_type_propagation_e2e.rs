//! A declared function type is propagated into RESULT positions of the initializer — an `if`/`when`
//! branch or a block's trailing value — so a bare lambda literal there takes its parameter types
//! from the expectation instead of erasing to `Any` (which typed as a wrong-arity `Function`).

use crate::common;

#[test]
fn function_literal_as_last_expression_in_block() {
    match common::run_box_corpus_case("regressions/functionLiteralAsLastExpressionInBlock.kt") {
        Some(s) => assert_eq!(s, "OK"),
        None => panic!("unexpectedly skipped"),
    }
}

#[test]
fn function_type_from_if_branches_used() {
    let src = r#"
fun box(): String {
    val p: (String) -> Int = if (true) {
        { s -> s.length }
    } else {
        { s -> s.length + 1 }
    }
    return if (p("abc") == 3) "OK" else "FAIL"
}
"#;
    common::expect_box_ok_with_stdlib(src, "FnTypeFromIf");
}

#[test]
fn function_type_from_when_branch_used() {
    let src = r#"
fun box(): String {
    val f: (Int) -> Int = when (1) {
        1 -> { x -> x * 2 }
        else -> { x -> x }
    }
    return if (f(5) == 10) "OK" else "FAIL"
}
"#;
    common::expect_box_ok_with_stdlib(src, "FnTypeFromWhen");
}
