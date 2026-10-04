//! A classpath inline body with a loop or a try/catch is entered with an empty operand stack:
//! kotlinc brackets the inlined body with `InlineMarker.beforeInlineCall`/`afterInlineCall`
//! (`requiresEmptyStackOnEntry`), and its mandatory FixStack pass stores whatever the caller
//! already has on the stack into fresh locals, then reloads it under the call's result.
//!
//! krusty's inliner declined such a body whenever an operand was already pushed, so a reified
//! inline call (which has no callable fallback) in an argument position after another argument,
//! such as `pair(id, xs.filterIsInstance<String>())`, failed the whole compile with "inline
//! splice failed". This shape came from a private-corpus module.
//!
//! The run tests keep the private corpus's `(v as? List<*>)?.filterIsInstance<String>() ?: …`
//! shape. The byte comparisons drop the safe call and the elvis: krusty marks the caller's line
//! after an inline call inside an elvis operand at the wrong offset with or without a saved stack,
//! a separate gap.

use super::common;

const ONE_OPERAND: &str = "fun pair(id: String, labels: List<String>): String = id + labels\n\
    \n\
    fun labels(id: String, v: Any?): String =\n\
    \x20   pair(id, (v as? List<*>)?.filterIsInstance<String>() ?: emptyList())\n\
    \n\
    fun box(): String {\n\
    \x20   val r = labels(\"x\", listOf(\"a\", 1, \"b\"))\n\
    \x20   return if (r == \"x[a, b]\" && labels(\"y\", null) == \"y[]\") \"OK\" else \"fail $r\"\n\
    }\n";

const WIDE_OPERANDS: &str =
    "fun triple(n: Long, id: String, labels: List<String>): String = \"$n$id$labels\"\n\
    \n\
    fun labels(n: Long, id: String, v: List<*>): String =\n\
    \x20   triple(n, id, v.filterIsInstance<String>())\n\
    \n\
    fun box(): String {\n\
    \x20   val r = labels(7L, \"x\", listOf(\"a\", 1, \"b\"))\n\
    \x20   return if (r == \"7x[a, b]\") \"OK\" else \"fail $r\"\n\
    }\n";

const CONSTRUCTOR_ARGUMENT: &str = "class W(val id: String, val labels: List<String>)\n\
    \n\
    fun build(id: String, v: Any?): W =\n\
    \x20   W(id = id, labels = (v as? List<*>)?.filterIsInstance<String>() ?: emptyList())\n\
    \n\
    fun box(): String {\n\
    \x20   val w = build(\"x\", listOf(\"a\", 1, \"b\"))\n\
    \x20   return if (w.labels == listOf(\"a\", \"b\")) \"OK\" else \"fail ${w.labels}\"\n\
    }\n";

#[test]
fn a_looping_reified_inline_body_after_a_pushed_argument_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(ONE_OPERAND, "inline_spill_one_operand"),
        "OK"
    );
}

#[test]
fn a_looping_reified_inline_body_after_wide_pushed_arguments_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(WIDE_OPERANDS, "inline_spill_wide_operands"),
        "OK"
    );
}

#[test]
fn a_looping_reified_inline_body_under_a_constructed_object_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(CONSTRUCTOR_ARGUMENT, "inline_spill_constructor"),
        "OK"
    );
}

const ONE_OPERAND_BYTES: &str =
    "fun pair(id: String, labels: List<String>): String = id + labels\n\
    \n\
    fun labels(id: String, v: List<*>): String = pair(id, v.filterIsInstance<String>())\n";

const CONSTRUCTOR_ARGUMENT_BYTES: &str = "class W(val id: String, val labels: List<String>)\n\
    \n\
    fun build(id: String, v: List<*>): W = W(id = id, labels = v.filterIsInstance<String>())\n";

#[test]
fn the_saved_operand_stack_matches_kotlinc() {
    common::assert_class_code_matches_kotlinc(
        "InlineSpillOneOperand",
        ONE_OPERAND_BYTES,
        "InlineSpillOneOperandKt",
    );
}

#[test]
fn saved_wide_operands_match_kotlinc() {
    common::assert_class_code_matches_kotlinc(
        "InlineSpillWideOperands",
        WIDE_OPERANDS,
        "InlineSpillWideOperandsKt",
    );
}

#[test]
fn an_operand_saved_under_a_new_object_matches_kotlinc() {
    common::assert_class_code_matches_kotlinc(
        "InlineSpillConstructor",
        CONSTRUCTOR_ARGUMENT_BYTES,
        "InlineSpillConstructorKt",
    );
}

/// A reified inline body with a try/catch and no lambda, compiled by kotlinc as a dependency.
const CATCHING_LIB: &str = "package lib\n\
    \n\
    inline fun <reified T> castOr(value: Any?, fallback: T): T =\n\
    \x20   try { value as T } catch (e: ClassCastException) { fallback }\n";

const CATCHING_MAIN: &str = "import lib.castOr\n\
    \n\
    class Box(val id: String, val size: Int)\n\
    \n\
    fun consume(n: Int, v: Int): Int = n + v\n\
    \n\
    fun pick(v: Any?): Int = consume(1, castOr(v, -1))\n\
    \n\
    fun boxed(id: String, v: Any?): Box = Box(id, castOr(v, 0))\n\
    \n\
    fun box(): String {\n\
    \x20   val ok = pick(41) == 42 && pick(\"x\") == 0 &&\n\
    \x20       boxed(\"a\", 7).size == 7 && boxed(\"b\", \"y\").size == 0\n\
    \x20   return if (ok) \"OK\" else \"fail\"\n\
    }\n";

/// A handler body is never entered over the caller's operands as emitted: the handler would start
/// without them. The call runs with the operands saved, whether by the operand sequencing ahead of
/// the call or by FixStack around it.
#[test]
fn a_catching_reified_inline_body_after_a_pushed_argument_runs() {
    assert_eq!(
        common::expect_box_run_against_ref(
            "inline_spill_catching_callee",
            CATCHING_LIB,
            CATCHING_MAIN
        )
        .as_deref(),
        Some("OK")
    );
}
