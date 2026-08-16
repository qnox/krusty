//! Two diagnostics whose wording drifted from the Kotlin frontend's templates: a context argument is
//! reported for a CALLABLE, never "for property", and the `hasNext()` return-type error names the
//! loop range's `iterator().hasNext()` rather than a bare `hasNext()`.

use super::common;

fn diagnostics(source: &str) -> Vec<String> {
    common::front_end_diagnostics(
        source,
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    )
}

#[test]
fn a_missing_context_argument_for_a_property_names_the_callable() {
    const SOURCE: &str = r#"
        // LANGUAGE: +ContextParameters
        class C
        context(c: C) val label: String get() = "OK"
        fun box(): String = label
    "#;

    let reported = diagnostics(SOURCE);
    assert!(
        reported
            .iter()
            .any(|diagnostic| diagnostic.contains("No context argument for")),
        "expected a missing-context diagnostic, got: {reported:?}"
    );
    assert!(
        !reported
            .iter()
            .any(|diagnostic| diagnostic.contains("for property")),
        "kotlinc's template has no 'property' in it: {reported:?}"
    );
}

#[test]
fn a_loop_range_whose_has_next_is_not_boolean_names_the_iterator() {
    const SOURCE: &str = r#"
        class Cursor {
            operator fun hasNext(): Int = 0
            operator fun next(): String = "x"
        }

        class Range {
            operator fun iterator(): Cursor = Cursor()
        }

        fun box() {
            for (item in Range()) {
            }
        }
    "#;

    let reported = diagnostics(SOURCE);
    assert!(
        reported.iter().any(|diagnostic| diagnostic
            == "the 'iterator().hasNext()' function of the loop range must return 'Boolean', \
                but returns 'Int'."),
        "expected the frontend's loop-range wording, got: {reported:?}"
    );
}
