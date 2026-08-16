//! `val x: T?` with no initializer, assigned once in a later branch. Kotlin's definite-assignment
//! rules make this a normal declaration — intellij-community's `MinusculeMatcherImpl` writes
//! `val ranges: List<MatchedFragment>?` and assigns it in both arms of an `if`. krusty parsed the
//! nullable case as a syntax error ("expected '='") while accepting the identical `var`, so the
//! same shape was legal or not depending on a keyword that does not affect it: a deferred `val` is
//! lowered as internally mutable either way.

use super::common;

fn run(source: &str) -> Option<String> {
    common::compile_and_run_box(
        source,
        "Main",
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    )
}

fn assert_kotlinc_accepts(tag: &str, source: &str) {
    let (code, diagnostics) = common::kotlinc_source_result(tag, source);
    assert_eq!(code, 0, "kotlinc rejected {tag}: {diagnostics}");
}

#[test]
fn a_deferred_nullable_val_assigned_in_branches_runs() {
    let source = r#"
        fun pick(flag: Boolean): String {
            val chosen: String?
            if (flag) {
                chosen = "OK"
            } else {
                chosen = null
            }
            return chosen ?: "null"
        }

        fun box(): String = pick(true) + pick(false)
    "#;

    assert_kotlinc_accepts("DeferredNullableValBranches", source);
    assert_eq!(run(source).as_deref(), Some("OKnull"));
}

#[test]
fn a_deferred_nullable_val_of_a_generic_type_parses() {
    let source = r#"
        fun collect(flag: Boolean): Int {
            val values: List<String>?
            values = if (flag) listOf("a", "b") else null
            return values?.size ?: 0
        }

        fun box(): String = "${collect(true)}${collect(false)}"
    "#;

    assert_kotlinc_accepts("DeferredNullableValGeneric", source);
    assert_eq!(run(source).as_deref(), Some("20"));
}

#[test]
fn a_deferred_nullable_val_reads_as_non_null_after_assignment() {
    let source = r#"
        fun length(): Int {
            val text: String?
            text = "four"
            return text.length
        }

        fun box(): String = "${length()}"
    "#;

    assert_kotlinc_accepts("DeferredNullableValNarrowing", source);
    assert_eq!(run(source).as_deref(), Some("4"));
}
