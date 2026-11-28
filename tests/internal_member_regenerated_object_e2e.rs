//! An `internal` member's name carries the module suffix (`merged$main`), so the name is no longer
//! the Kotlin name it was declared under. An inline call in it whose callee builds an anonymous
//! object (`sortedBy { … }` makes a `Comparator` through `compareBy`) copies that object into the
//! class under the member's own name, exactly as for a public member. krusty rejected the whole
//! file ("the inliner rejected a call its route planning gave it") because the call site was not
//! recognised as a declared function once its name was suffixed.

use super::common;

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

// PLATFORM-SPECIFIC: the defect is the JVM module suffix of an `internal` member's method name
// (`merged$main`), which no other backend writes, and the assertion is on JVM class files. The
// program itself runs on the KLIB lanes only once they inline the stdlib's crossinline `sortedBy`
// ("a dependency callable whose target code was not emitted"), the gap the corpus cases
// `basics/k42000_1.kt` and `inference/integerLiteralTypeInLamdaReturnType.kt` are listed for.
const INTERNAL_MEMBER: &str = r#"class P(val name: String, val xs: Set<Int>) {
    internal fun merged(other: P): List<Int> = (xs + other.xs).sortedBy { it }

    private fun own(): List<Int> = xs.sortedBy { -it }

    fun both(): List<Int> = own() + xs.sortedByDescending { it }
}

fun box(): String {
    val merged = P("a", setOf(2, 1)).merged(P("b", setOf(3)))
    return if (merged == listOf(1, 2, 3)) "OK" else "FAIL: $merged"
}
"#;

#[test]
fn an_internal_member_copies_the_object_its_inline_callee_builds_like_kotlinc() {
    assert_like_kotlinc(&[("InternalMember.kt", INTERNAL_MEMBER)]);
}

#[test]
fn an_internal_member_with_an_inlined_object_runs() {
    let outcome = common::compile_and_run_with_stdlib(INTERNAL_MEMBER, "InternalMember");
    assert_eq!(outcome.as_deref(), Some("OK"));
}
