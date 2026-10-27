//! `@OverloadResolutionByLambdaReturnType` families such as `flatMap` over `(T) -> Iterable<R>`
//! and `(T) -> Sequence<R>` take the same lambda inputs and differ only in the lambda's result.
//! The lambda body chooses the overload, so it is not checked against the first candidate's
//! result type, with or without an expected call result, and inside a SAM-converted lambda.

use super::common;

const SOURCE: &str = "fun interface Languages { fun of(x: Int): Sequence<String> }\n\
fun sequenceBody(s: Sequence<Int>): Sequence<String> = s.flatMap { sequenceOf(\"s$it\") }\n\
fun iterableBody(s: Sequence<Int>): Sequence<String> = s.flatMap { listOf(\"i$it\") }\n\
fun inferred(s: Sequence<Int>) = s.flatMap { p -> sequenceOf(\"n$p\") }\n\
fun listOfSequences(s: List<Int>): List<String> = s.flatMap { sequenceOf(\"l$it\") }\n\
fun combined(providers: Sequence<Languages>): Languages = Languages { x ->\n\
    providers.flatMap { provider ->\n\
        provider.of(x)\n\
    }\n\
}\n\
fun box(): String {\n\
    val s = sequenceOf(1, 2)\n\
    val all = sequenceBody(s).toList() + iterableBody(s).toList() + inferred(s).toList() +\n\
        listOfSequences(listOf(3)) +\n\
        combined(sequenceOf(Languages { sequenceOf(\"a$it\") })).of(4).toList()\n\
    return if (all.joinToString() == \"s1, s2, i1, i2, n1, n2, l3, a4\") \"OK\" else all.joinToString()\n\
}\n";

#[test]
fn a_lambda_body_selects_its_result_overload_like_kotlinc() {
    common::expect_box_ok_with_stdlib(SOURCE, "LambdaReturnOverload");
    let classes = common::classes_against_kotlinc_module(&[("LambdaReturnOverload.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}
