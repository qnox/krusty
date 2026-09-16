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

/// A spread does not make an adjacent ordinary element safe to emit over the builder stack.
#[test]
fn a_branching_ordinary_element_may_follow_a_spread() {
    let main = format!(
        "{DECLARATIONS}\
fun probe(xs: List<String>): String =\n\
\x20   sink(*arrayOf(\"S\"), \"A\".takeUnless {{ xs.isEmpty() }})\n\
fun box(): String {{\n\
\x20   val empty = probe(emptyList())\n\
\x20   val filled = probe(listOf(\"x\"))\n\
\x20   if (empty != \"S-\") return \"FAIL: empty \" + empty\n\
\x20   return if (filled == \"SA\") \"OK\" else \"FAIL: filled \" + filled\n\
}}\n"
    );
    both_compilers_box(&main, "vararg_branch_after_spread");
}

/// The spread-producing expression itself may branch before it yields its array.
#[test]
fn a_branching_expression_may_supply_the_spread_array() {
    let main = format!(
        "{DECLARATIONS}\
fun probe(drop: Boolean): String =\n\
\x20   sink(*(arrayOf(\"S\").takeUnless {{ drop }} ?: emptyArray()), \"A\")\n\
fun box(): String {{\n\
\x20   val kept = probe(false)\n\
\x20   val dropped = probe(true)\n\
\x20   if (kept != \"SA\") return \"FAIL: kept \" + kept\n\
\x20   return if (dropped == \"A\") \"OK\" else \"FAIL: dropped \" + dropped\n\
}}\n"
    );
    both_compilers_box(&main, "vararg_branching_spread");
}

/// Spilling is an evaluation strategy only: it must not duplicate or reorder source effects.
#[test]
fn branching_spread_elements_are_evaluated_once_from_left_to_right() {
    let main = format!(
        "{DECLARATIONS}\
var trace = \"\"\n\
fun mark(value: String): String {{ trace += value; return value }}\n\
fun box(): String {{\n\
\x20   val out = sink(\n\
\x20       *(arrayOf(mark(\"S\")).takeUnless {{ trace += \"B\"; false }} ?: emptyArray()),\n\
\x20       mark(\"A\").takeUnless {{ trace += \"C\"; false }},\n\
\x20   )\n\
\x20   if (trace != \"SBAC\") return \"FAIL: trace \" + trace\n\
\x20   return if (out == \"SA\") \"OK\" else \"FAIL: out \" + out\n\
}}\n"
    );
    both_compilers_box(&main, "vararg_spread_effect_order");
}

/// Primitive spread builders have a separate JVM realization and must obey the same empty-stack
/// rule for both the spread array and an adjacent scalar element.
#[test]
fn primitive_branching_spread_and_element_use_the_same_frame_safe_path() {
    const MAIN: &str = "fun sum(vararg items: Int): Int {\n\
\x20   var out = 0\n\
\x20   for (item in items) out += item\n\
\x20   return out\n\
}\n\
fun probe(drop: Boolean): Int =\n\
\x20   sum(*(intArrayOf(1, 2).takeUnless { drop } ?: intArrayOf()), 4.takeUnless { drop } ?: 8)\n\
fun box(): String {\n\
\x20   val kept = probe(false)\n\
\x20   val dropped = probe(true)\n\
\x20   if (kept != 7) return \"FAIL: kept \" + kept\n\
\x20   return if (dropped == 8) \"OK\" else \"FAIL: dropped \" + dropped\n\
}\n";
    both_compilers_box(MAIN, "primitive_vararg_branching_spread");
}

/// The non-frame path is intentionally untouched; pin its complete classfile, not only behavior.
#[test]
fn ordinary_packed_vararg_stays_byte_identical_to_kotlinc() {
    const SOURCE: &str =
        "fun ordinary(first: Int, second: Int): IntArray = intArrayOf(first, second)\n";
    let Some(result) = common::byte_diff_against_kotlinc_cp(
        "OrdinaryVarargBytes",
        SOURCE,
        "OrdinaryVarargBytesKt",
        &[common::stdlib_jar()],
    ) else {
        eprintln!("skip: provisioned kotlinc unavailable");
        return;
    };
    assert_eq!(result, Ok(()), "ordinary packed vararg bytes changed");
}
