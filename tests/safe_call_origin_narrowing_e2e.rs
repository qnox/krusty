//! Proving a safe-call RESULT non-null proves its receiver non-null.
//!
//! `x?.let { … }` evaluates to null whenever `x` is null, so a later `y != null` on
//! `val y = x?.let { … }` entails `x != null`. kotlinc's data flow draws that implication; krusty
//! did not, so the second use of `x` inside `y?.let { … }` still read as nullable and the file was
//! rejected. The direct spelling (`if (x?.foo != null) x.bar`) already worked — only the version
//! that stores the intermediate result in a `val` was lost, which is how the shape is actually
//! written when the result is needed twice.

use super::common;

fn assert_both_accept(source: &str, what: &str) {
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected the {what} fixture"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty must accept the exact source kotlinc accepts for {what}"
    );
}

const PRELUDE: &str = "class Item(val name: String)\n";
const CHANGING_DELEGATE_PRELUDE: &str = "import kotlin.reflect.KProperty\n\
class Item(val name: String)\n\
class Changing(private var next: Item?) {\n\
    operator fun getValue(thisRef: Any?, property: KProperty<*>): Item? {\n\
        val result = next\n\
        next = null\n\
        return result\n\
    }\n\
}\n";

fn assert_both_reject_unsafe_receiver(
    source: &str,
    line: usize,
    reference_column: usize,
    krusty_column: usize,
    source_line: &str,
    reference_marker_width: usize,
    reference_message: &str,
    krusty_message: &str,
) {
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    let reference_path = result
        .reference_stderr
        .split(':')
        .next()
        .expect("kotlinc diagnostic path");
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (
            1,
            format!(
                "{reference_path}:{line}:{reference_column}: error: {reference_message}\n{source_line}\n{}{}\n",
                " ".repeat(reference_column - 1),
                "^".repeat(reference_marker_width)
            )
            .as_str()
        )
    );
    let krusty_path = result
        .krusty_stderr
        .split(':')
        .next()
        .expect("krusty diagnostic path");
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (
            1,
            format!(
                "{krusty_path}:{line}:{krusty_column}: error: {krusty_message}\nkrusty: 1 error(s)\n"
            )
            .as_str()
        )
    );
}

#[test]
fn a_branch_local_result_cannot_narrow_an_outer_same_named_binding() {
    assert_both_reject_unsafe_receiver(
        "class Item(val name: String)\n\
         fun use(a: Item?, n: String?, flag: Boolean): String? {\n\
         \x20 if (flag) {\n\
         \x20\x20 val n = a?.name\n\
         \x20\x20 n?.length\n\
         \x20 }\n\
         \x20 return n?.let { a.name }\n\
         }\n",
        7,
        20,
        20,
        "  return n?.let { a.name }",
        1,
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
    );
}

#[test]
fn a_loop_local_result_cannot_escape_the_loop_body() {
    assert_both_reject_unsafe_receiver(
        "class Item(val name: String)\n\
         fun use(a: Item?, n: String?, flag: Boolean): String? {\n\
         \x20 while (flag) {\n\
         \x20\x20 val n = a?.name\n\
         \x20\x20 if (n != null) break\n\
         \x20 }\n\
         \x20 return n?.let { a.name }\n\
         }\n",
        7,
        20,
        20,
        "  return n?.let { a.name }",
        1,
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
    );
}

#[test]
fn an_origin_shadow_is_not_the_binding_the_result_proves() {
    assert_both_reject_unsafe_receiver(
        "class Item(val name: String)\n\
         fun use(a: Item?, flag: Boolean): String? {\n\
         \x20 val n = a?.name\n\
         \x20 if (flag) {\n\
         \x20\x20 val a: Item? = null\n\
         \x20\x20 return n?.let { a.name }\n\
         \x20 }\n\
         \x20 return null\n\
         }\n",
        6,
        21,
        21,
        "   return n?.let { a.name }",
        1,
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
    );
}

#[test]
fn a_mutable_origin_is_not_recorded() {
    assert_both_reject_unsafe_receiver(
        "class Item(val name: String)\n\
         fun use(seed: Item?): String? {\n\
         \x20 var a = seed\n\
         \x20 val n = a?.name\n\
         \x20 a = null\n\
         \x20 return n?.let { a.name }\n\
         }\n",
        6,
        20,
        20,
        "  return n?.let { a.name }",
        1,
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Nothing?'.",
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Nothing?'.",
    );
}

#[test]
fn a_delegated_origin_is_not_recorded() {
    assert_both_reject_unsafe_receiver(
        &format!(
            "{CHANGING_DELEGATE_PRELUDE}fun use(seed: Item?): String? {{\n\
             \x20   val origin: Item? by Changing(seed)\n\
             \x20   val result = origin?.name\n\
             \x20   return result?.let {{ origin.name }}\n\
             }}\n"
        ),
        13,
        26,
        32,
        "    return result?.let { origin.name }",
        6,
        "smart cast to 'Item' is impossible, because 'origin' is a delegated property.",
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
    );
}

#[test]
fn a_delegated_result_is_not_stable_inside_its_own_safe_call() {
    assert_both_reject_unsafe_receiver(
        &format!(
            "{CHANGING_DELEGATE_PRELUDE}fun use(seed: Item?): String? {{\n\
             \x20   val result: Item? by Changing(seed)\n\
             \x20   return result?.let {{ result.name }}\n\
             }}\n"
        ),
        12,
        26,
        32,
        "    return result?.let { result.name }",
        6,
        "smart cast to 'Item' is impossible, because 'result' is a delegated property.",
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
    );
}

#[test]
fn an_outer_relation_survives_an_inner_non_safe_shadow() {
    assert_both_accept(
        "class Item(val name: String)\n\
         fun use(a: Item?, flag: Boolean): String? {\n\
         \x20 val n = a?.name\n\
         \x20 if (flag) {\n\
         \x20\x20 val n: String? = null\n\
         \x20\x20 n?.length\n\
         \x20 }\n\
         \x20 return n?.let { a.name }\n\
         }\n",
        "an outer relation after an inner shadow",
    );
}

#[test]
fn shadowed_spellings_do_not_form_a_provenance_cycle() {
    assert_both_accept(
        "class Item(val name: String)\n\
         fun use(seed: Item?): Int? {\n\
         \x20 val a = seed?.name\n\
         \x20 return run {\n\
         \x20\x20 val seed = a?.length\n\
         \x20\x20 if (seed != null) a.length else null\n\
         \x20 }\n\
         }\n",
        "a binding-identified chain with repeated spellings",
    );
}

#[test]
fn a_valid_origin_chain_has_no_arbitrary_depth_limit() {
    let mut source = String::from(
        "class Item(val name: String)\n\
         fun use(seed: Item?): Int? {\n\
         \x20 val n0 = seed?.name\n",
    );
    for index in 1..=24 {
        source.push_str(&format!("  val n{index} = n{}?.let {{ it }}\n", index - 1));
    }
    source.push_str("  return n24?.let { seed.name.length }\n}\n");
    assert_both_accept(&source, "an origin chain longer than an implementation cap");
}

/// The corpus shape: two locals chained through `?.let`, the first used inside the second's branch.
#[test]
fn a_safe_call_result_proves_its_receiver_non_null() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun parentOf(item: Item): String? = item.name.ifEmpty {{ null }}\n\
             fun use(map: Map<String, Item>, key: String?): String? {{\n\
             \x20 val explicit = key?.let {{ map[it] }}\n\
             \x20 val parent = explicit?.let {{ parentOf(it) }}\n\
             \x20 return parent?.let {{ p -> \"${{explicit.name}} under $p\" }}\n\
             }}\n"
        ),
        "a chained `?.let`",
    );
}

/// The implication is transitive: three links, with the first read inside the last branch.
#[test]
fn the_implication_is_transitive_across_a_chain() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun use(a: Item?): String? {{\n\
             \x20 val b = a?.let {{ it.name }}\n\
             \x20 val c = b?.let {{ it.length }}\n\
             \x20 return c?.let {{ \"${{a.name}} $it\" }}\n\
             }}\n"
        ),
        "a transitive chain",
    );
}

/// A safe MEMBER access, not just `let`, carries the same implication.
#[test]
fn a_safe_member_access_carries_the_implication() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun use(a: Item?): String? {{\n\
             \x20 val n = a?.name\n\
             \x20 return n?.let {{ \"${{a.name}} $it\" }}\n\
             }}\n"
        ),
        "a safe member access",
    );
}

/// Control: the direct spelling already worked and must keep working.
#[test]
fn the_direct_spelling_still_narrows() {
    assert_both_accept(
        &format!(
            "{PRELUDE}fun use(a: Item?): String {{\n\
             \x20 if (a?.name != null) return a.name\n\
             \x20 return \"\"\n\
             }}\n"
        ),
        "the direct spelling",
    );
}

/// Control: a `var` receiver is not stable, so nothing may be proven through it. Both compilers
/// reject; kotlinc pins the verdict and krusty pins its complete output.
#[test]
fn a_mutable_receiver_still_proves_nothing() {
    assert_both_reject_unsafe_receiver(
        &format!(
            "{PRELUDE}class Holder(var a: Item?) {{\n\
         \x20 fun use(): String? {{\n\
         \x20\x20 val n = a?.name\n\
         \x20\x20 return n?.let {{ a.name }}\n\
         \x20 }}\n\
         }}\n"
        ),
        5,
        20,
        21,
        "   return n?.let { a.name }",
        1,
        "smart cast to 'Item' is impossible, because 'a' is a mutable property that could be mutated concurrently.",
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
    );
}

/// Control: a local REASSIGNED after the safe call is no longer the same value, so the recorded
/// implication must not survive it.
#[test]
fn a_reassigned_local_drops_the_implication() {
    assert_both_reject_unsafe_receiver(
        &format!(
            "{PRELUDE}fun use(a: Item?, b: Item?): String? {{\n\
         \x20 var n = a?.name\n\
         \x20 n = b?.name\n\
         \x20 return n?.let {{ a.name }}\n\
         }}\n"
        ),
        5,
        20,
        20,
        "  return n?.let { a.name }",
        1,
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
        "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Item?'.",
    );
}

/// The runtime contract: the narrowed receiver really holds the value the chain proved present.
#[test]
fn the_narrowed_receiver_holds_its_value_at_runtime() {
    common::expect_box_ok_files_with_stdlib(
        &[(
            "Use.kt",
            "class Item(val name: String)\n\
             fun pick(a: Item?): String {\n\
             \x20 val n = a?.name\n\
             \x20 return n?.let { \"${a.name}=$it\" } ?: \"none\"\n\
             }\n\
             fun box(): String {\n\
             \x20 val present = pick(Item(\"x\"))\n\
             \x20 val absent = pick(null)\n\
             \x20 return if (present == \"x=x\" && absent == \"none\") \"OK\" else \"$present/$absent\"\n\
             }\n",
        )],
        "SafeCallOriginNarrowing",
    );
}
