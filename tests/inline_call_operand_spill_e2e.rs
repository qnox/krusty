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
