//! An inline call whose body BRANCHES may be a vararg element.
//!
//! `sink("A".takeUnless { xs.isEmpty() })` — one stdlib inline call, passed as a vararg element —
//! declined its splice, and because a required inline body that cannot be spliced bails the whole
//! file, one such argument cost a module every class in it:
//!
//! ```text
//! error: krusty: JVM backend inline error: inline splice failed
//! [emit] inline splice failed for kotlin/StandardKt.takeUnless(…)
//! ```
//!
//! The same calls in a FIXED-ARITY parameter list always worked, and a straight-line inline body
//! (`let { it.size }`) worked as a vararg element too. It is the combination — a branching inline
//! body packed into the vararg array — that failed.

use super::common;

/// Run one fixture under BOTH compilers and require the same `box()` value.
///
/// `expect_box_ok_files_with_stdlib` alone only proves krusty agrees with itself. These shapes are
/// about matching the reference compiler, so it must run the identical source.
fn both_compilers_box(main: &str, stem: &str) {
    let reference = common::kotlinc_box_result(main);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", main)], stem);
}

const DECLARATIONS: &str = "fun sink(vararg items: String?): String {\n\
\x20   var out = \"\"\n\
\x20   for (item in items) {\n\
\x20       out += item ?: \"-\"\n\
\x20   }\n\
\x20   return out\n\
}\n\
\n";

/// The reduced shape: a single branching inline call as the only vararg element.
#[test]
fn a_branching_inline_call_may_be_the_only_vararg_element() {
    let main = format!(
        "{DECLARATIONS}\
fun probe(xs: List<String>): String = sink(\"A\".takeUnless {{ xs.isEmpty() }})\n\
fun box(): String {{\n\
\x20   val empty = probe(emptyList())\n\
\x20   val filled = probe(listOf(\"x\"))\n\
\x20   if (empty != \"-\") return \"FAIL: empty \" + empty\n\
\x20   return if (filled == \"A\") \"OK\" else \"FAIL: filled \" + filled\n\
}}\n"
    );
    both_compilers_box(&main, "vararg_inline_single");
}

/// The corpus spelling: several such elements in one vararg call.
#[test]
fn several_branching_inline_calls_may_share_one_vararg_call() {
    let main = format!(
        "{DECLARATIONS}\
fun probe(xs: List<String>): String =\n\
\x20   sink(\n\
\x20       \"A\".takeUnless {{ xs.isEmpty() }},\n\
\x20       \"B\".takeUnless {{ xs.isNotEmpty() }},\n\
\x20   )\n\
fun box(): String {{\n\
\x20   val empty = probe(emptyList())\n\
\x20   val filled = probe(listOf(\"x\"))\n\
\x20   if (empty != \"-B\") return \"FAIL: empty \" + empty\n\
\x20   return if (filled == \"A-\") \"OK\" else \"FAIL: filled \" + filled\n\
}}\n"
    );
    both_compilers_box(&main, "vararg_inline_several");
}

/// The control that isolates the vararg: the identical calls in a FIXED-ARITY parameter list always
/// spliced.
#[test]
fn the_same_calls_in_a_fixed_arity_call_still_work() {
    let main = format!(
        "{DECLARATIONS}\
fun pair(first: String?, second: String?): String = (first ?: \"-\") + (second ?: \"-\")\n\
fun probe(xs: List<String>): String =\n\
\x20   pair(\n\
\x20       \"A\".takeUnless {{ xs.isEmpty() }},\n\
\x20       \"B\".takeUnless {{ xs.isNotEmpty() }},\n\
\x20   )\n\
fun box(): String {{\n\
\x20   val empty = probe(emptyList())\n\
\x20   val filled = probe(listOf(\"x\"))\n\
\x20   if (empty != \"-B\") return \"FAIL: empty \" + empty\n\
\x20   return if (filled == \"A-\") \"OK\" else \"FAIL: filled \" + filled\n\
}}\n"
    );
    both_compilers_box(&main, "fixed_arity_inline");
}

/// The control that isolates the BRANCH: a straight-line inline body was always accepted as a vararg
/// element.
#[test]
fn a_straight_line_inline_body_still_works_as_a_vararg_element() {
    const MAIN: &str = "fun count(vararg items: Int): Int {\n\
\x20   var total = 0\n\
\x20   for (item in items) {\n\
\x20       total += item\n\
\x20   }\n\
\x20   return total\n\
}\n\
fun probe(xs: List<String>): Int =\n\
\x20   count(\n\
\x20       xs.let { it.size },\n\
\x20       xs.let { it.size + 1 },\n\
\x20   )\n\
fun box(): String {\n\
\x20   val total = probe(listOf(\"x\"))\n\
\x20   return if (total == 3) \"OK\" else \"FAIL: \" + total\n\
}\n";
    both_compilers_box(MAIN, "straight_line_vararg");
}
