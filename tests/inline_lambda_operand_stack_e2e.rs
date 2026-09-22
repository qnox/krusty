//! An inline call taking a lambda splices under a non-empty operand stack.
//!
//! `Box(buildMap { … })` reaches the splice with `new Box; dup` already on the stack. The relocated
//! StackMapTable frames carry no operand prefix, so a splice that records frames has to refuse a
//! non-empty baseline — but the test for "records frames" asked whether the lambda had a BODY, not
//! whether any body produced a frame. Every inline call with a lambda argument answered yes, so a
//! branchless lambda in an argument position declined, and because `buildMap` has no callable
//! fallback the declined splice dropped the whole file.
use super::common;

const BUILD_MAP_IN_AN_ARGUMENT: &str = "\
class Box(val values: Map<String, String>)

fun make(tag: String): Box =
    Box(
        buildMap {
            put(\"a\", tag)
            put(\"b\", tag + tag)
        },
    )

fun box(): String {
    val values = make(\"x\").values
    return if (values[\"a\"] == \"x\" && values[\"b\"] == \"xx\") \"OK\" else values.toString()
}
";

/// It compiles and runs: the lambda body has no frame of its own, so the splice is free to land on
/// the constructor's uninitialized prefix.
#[test]
fn a_branchless_inline_lambda_splices_under_a_constructor_prefix() {
    let jdk = common::jdk_modules();
    let cp = [common::stdlib_jar(), jdk.clone()];
    assert_eq!(
        common::expect_box_run(BUILD_MAP_IN_AN_ARGUMENT, "Main", &cp, Some(jdk.as_path())),
        "OK"
    );
}

const BRANCHY_LAMBDA_IN_AN_ARGUMENT: &str = "\
class Box(val values: Map<String, String>)

fun make(tag: String, flag: Boolean): Box =
    Box(
        buildMap {
            put(\"a\", tag)
            if (flag) put(\"b\", tag + tag)
        },
    )

fun box(): String {
    val on = make(\"x\", true).values
    val off = make(\"x\", false).values
    return if (on.size == 2 && off.size == 1) \"OK\" else \"$on/$off\"
}
";

/// The neighbour that always worked, kept as the control: a lambda body that DOES record a frame is
/// spilled to an empty baseline by the operand sequence before the splice is reached.
#[test]
fn a_branchy_inline_lambda_still_reaches_an_empty_baseline() {
    let jdk = common::jdk_modules();
    let cp = [common::stdlib_jar(), jdk.clone()];
    assert_eq!(
        common::expect_box_run(
            BRANCHY_LAMBDA_IN_AN_ARGUMENT,
            "Main",
            &cp,
            Some(jdk.as_path())
        ),
        "OK"
    );
}
