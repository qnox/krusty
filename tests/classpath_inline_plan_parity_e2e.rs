//! An external classpath inline call that lowers through the inline-body plan carries kotlinc's
//! inline debug surface: the `$i$a$-<callee>-<lambda class>` lambda-frame marker, the callee frame's
//! `$iv` locals, the line numbers the call maps through the callee's SMAP, and the `nop`s that
//! separate the inlined frame from its caller. A repository-owned dependency below proves the
//! generic `InlineBodyPlan::InvokeLambda` route; the stdlib cases additionally cover inline-only and
//! iteration plans. This suite pins the emitted class bytes against the reference compiler so the
//! plan path cannot drift from that surface.

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

/// A neutral dependency function with one direct lambda invocation is the generic
/// `InlineBodyPlan::InvokeLambda` shape. Its owner and name do not belong to the stdlib, so no
/// intrinsic or name-specific plan can make this regression pass. A two-lambda body would contain
/// two invoke sites and deliberately route through `MethodInliner`, testing the wrong path here.
const STEP_LIB: &str = r#"inline fun step(x: Int, transform: (Int) -> Int): Int = transform(x)
"#;

const STEP: &str = r#"fun box(): String {
    val v = step(20) { it + 1 }
    return if (v == 21) "OK" else "FAIL: " + v
}
"#;

#[test]
fn a_neutral_invoke_lambda_plan_is_the_reference_compilers_class() {
    let lib = common::kotlinc_lib_out(&[("StepLib.kt", STEP_LIB)])
        .expect("reference kotlinc is provisioned");
    common::assert_classes_identical_to_kotlinc_against("PlanStep", STEP, &["PlanStepKt"], &[lib]);
}

/// Nested repository-owned plans leave the outer lambda receiver below the inner expansion. This
/// proves that stack preservation follows the recorded inline boundary rather than stdlib owners
/// or an `apply`/`let`-specific receiver shape.
const NESTED_STEP_LIB: &str = r#"inline fun <T> hold(value: T, action: T.() -> Unit): T {
    value.action()
    return value
}

inline fun <T, R> turn(value: T, transform: (T) -> R): R = transform(value)
"#;

const NESTED_STEP: &str = r#"fun box(): String =
    hold(StringBuilder()) { append(turn("x") { it + "y" }) }.toString()
"#;

#[test]
fn nested_neutral_inline_plans_are_the_reference_compilers_class() {
    let lib = common::kotlinc_lib_out(&[("NestedStepLib.kt", NESTED_STEP_LIB)])
        .expect("reference kotlinc is provisioned");
    common::assert_classes_identical_to_kotlinc_against_jdk(
        "PlanNestedStep",
        NESTED_STEP,
        &["PlanNestedStepKt"],
        &[lib],
    );
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

/// An inline-only call made with the caller's operands on the stack (`"O" + "x".let { … }`) saves
/// them across the lambda body. In the method's own body kotlinc's FixStack runs over the finished
/// method, so the saved words go above every local the method uses, after the lambda's `$i$a$`
/// marker, and the next such call reuses them.
const ROOT_STACK: &str = r#"fun box(): String = { "O" }.let { it() } + "x".let { it.drop(1) + "K" }
"#;

#[test]
fn a_stack_saved_in_the_methods_own_body_is_stored_above_its_locals() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PlanRootStack",
        ROOT_STACK,
        &["PlanRootStackKt"],
    );
}

/// Inside an inline lambda's body kotlinc saved the stack while it built that lambda's node, so the
/// words are claimed ahead of the inner lambda's own marker. What lies below the enclosing bracket
/// is that bracket's to save: the inner call saves only what was pushed inside the lambda, while
/// the outer call's words go above every local of the method.
const NESTED_STACK: &str = r#"fun box(): String = "O" + run { "" + "x".let { it.drop(1) + "K" } }
"#;

#[test]
fn a_stack_saved_inside_an_inline_lambda_is_stored_below_its_marker() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PlanNestedStack",
        NESTED_STACK,
        &["PlanNestedStackKt"],
    );
}
