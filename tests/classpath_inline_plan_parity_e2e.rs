//! An external classpath inline call that lowers through the inline-body plan (an `@InlineOnly`
//! stdlib call like `apply`/`let`, or an iteration plan like `forEach`) carries kotlinc's inline
//! debug surface: the `$i$a$-<callee>-<lambda class>` lambda-frame marker, the callee frame's
//! `$iv` locals, the line numbers the call maps through the callee's SMAP, and the `nop`s that
//! separate the inlined frame from its caller. This suite pins the emitted class bytes against
//! the reference compiler so the plan path cannot drift from that surface.

use super::common;

/// An `@InlineOnly` extension call (`apply`): the receiver is spilled unnamed, the lambda's own
/// receiver is `$this$box_u24lambda_u240`, and the lambda frame opens with
/// `$i$a$-apply-PlanApplyKt$box$1` whose store takes the call-site line through the fake-file
/// synthetic mapping.
const APPLY: &str = r#"fun box(): String {
    val sb = StringBuilder()
    sb.apply { append("x") }
    return sb.toString()
}
"#;

#[test]
fn an_inline_only_extension_call_is_the_reference_compilers_class() {
    common::assert_classes_identical_to_kotlinc_jdk("PlanApply", APPLY, &["PlanApplyKt"]);
}

/// Two nested `@InlineOnly` calls: the inner `let` frame sits inside the outer `apply` frame, and
/// each lambda frame carries its own marker and line mapping.
const NESTED: &str = r#"fun box(): String {
    val sb = StringBuilder()
    sb.apply { append("x".let { it + "y" }) }
    return sb.toString()
}
"#;

#[test]
fn nested_inline_only_calls_are_the_reference_compilers_class() {
    common::assert_classes_identical_to_kotlinc_jdk("PlanNested", NESTED, &["PlanNestedKt"]);
}

/// An inline call with two lambda arguments marks each lambda frame separately.
const BOTH_LIB: &str = r#"inline fun both(x: Int, f: (Int) -> Int, g: (Int) -> Int): Int = f(x) + g(x)
"#;

const BOTH: &str = r#"fun box(): String {
    val v = both(20, { it + 1 }, { it * 2 })
    return if (v == 61) "OK" else "FAIL: " + v
}
"#;

#[test]
fn an_inline_call_with_two_lambdas_is_the_reference_compilers_class() {
    let lib = common::kotlinc_lib_out(&[("BothLib.kt", BOTH_LIB)])
        .expect("reference kotlinc is provisioned");
    common::assert_classes_identical_to_kotlinc_against("PlanBoth", BOTH, &["PlanBothKt"], &[lib]);
}

/// An iteration plan (`forEach` over a list): the callee is NOT inline-only, so its frame keeps
/// the `$i$f$forEach` function marker and the named `$this$forEach$iv` / `element$iv` locals, and
/// the lambda frame opens with `$i$a$-forEach-PlanForEachKt$box$1`.
const FOR_EACH: &str = r#"fun box(): String {
    val sb = StringBuilder()
    listOf(1, 2, 3).forEach { sb.append(it) }
    return sb.toString()
}
"#;

#[test]
fn an_inline_iteration_is_the_reference_compilers_class() {
    common::assert_classes_identical_to_kotlinc_jdk("PlanForEach", FOR_EACH, &["PlanForEachKt"]);
}
