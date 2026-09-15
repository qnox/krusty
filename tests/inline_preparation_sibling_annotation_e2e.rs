//! An inline member must not crash its siblings' annotation checking.
//!
//! Preparing a class's inline members re-enters the class and walks the members it did NOT select,
//! purely to rebuild their scopes. Those members' annotation ARGUMENT expressions belong to a source
//! fragment that pass no longer retains, and the checker asserted that only Pass-1 default checking
//! could ever observe released syntax:
//!
//! ```text
//! thread 'main' panicked at src/resolve.rs:
//! only Pass-1 default checking may observe released annotation syntax
//! ```
//!
//! So any argument-bearing annotation on an ordinary member — `@Suppress("UNCHECKED_CAST")` is the
//! everyday case — crashed the compiler as soon as the same class also declared an `inline` member.
//! Both passes re-enter a declaration they did not select; skipping released syntax is correct in
//! each, and the assertion now says so.

use super::common;


/// Require that BOTH compilers reject the fixture with the identical diagnostic set.
///
/// The exit code is pinned to exactly 1, not merely nonzero: a panic exits 101, and a nonzero
/// assertion would accept that as a rejection whenever the expected diagnostic happened to be
/// printed before the crash — which is precisely the failure this fixture exists to catch, since
/// the production change it guards replaced an assertion. The complete rendered contract is
/// compared too: same count, file, line, column, message and order, with nothing error-shaped on
/// stdout where the comparison would not see it.
fn expect_identical_rejection(result: &common::CompilerDiagnosticResult, tag: &str) {
    assert_eq!(
        result.krusty_code, 1,
        "{tag}: krusty exited {} rather than rejecting the fixture: {}{}",
        result.krusty_code, result.krusty_stdout, result.krusty_stderr
    );
    assert_eq!(
        result.reference_code, 1,
        "{tag}: kotlinc exited {} rather than rejecting the fixture: {}",
        result.reference_code, result.reference_stderr
    );
    assert_eq!(
        common::compiler_errors(&result.krusty_stdout),
        [],
        "{tag}: krusty wrote diagnostics to stdout, where the comparison would miss them"
    );
    let krusty = common::compiler_errors(&result.krusty_stderr);
    let reference = common::compiler_errors(&result.reference_stderr);
    assert!(
        !reference.is_empty(),
        "{tag}: kotlinc rejected with no parseable diagnostic: {}",
        result.reference_stderr
    );
    assert_eq!(
        krusty, reference,
        "{tag}: diagnostics differ.\nkrusty:  {krusty:#?}\nkotlinc: {reference:#?}"
    );
}

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

/// The crashing shape, reduced: one argument-bearing annotation, one inline sibling.
#[test]
fn an_inline_member_does_not_crash_an_annotated_sibling() {
    const MAIN: &str = "class Store {\n\
\x20   @Suppress(\"UNCHECKED_CAST\")\n\
\x20   fun tag(): String = \"t\"\n\
\n\
\x20   inline fun decorate(make: () -> String): String = make() + tag()\n\
}\n\
fun box(): String {\n\
\x20   val decorated = Store().decorate { \"made-\" }\n\
\x20   return if (decorated == \"made-t\") \"OK\" else \"FAIL: \" + decorated\n\
}\n";
    both_compilers_box(MAIN, "inline_sibling_suppress");
}

/// The annotation itself is irrelevant — any application that carries arguments releases the same
/// syntax.
#[test]
fn any_argument_bearing_annotation_on_the_sibling_behaves_the_same() {
    const MAIN: &str = "class Store {\n\
\x20   @Deprecated(\"use decorate\")\n\
\x20   fun tag(): String = \"t\"\n\
\n\
\x20   inline fun decorate(make: () -> String): String = make() + tag()\n\
}\n\
fun box(): String {\n\
\x20   val decorated = Store().decorate { \"made-\" }\n\
\x20   return if (decorated == \"made-t\") \"OK\" else \"FAIL: \" + decorated\n\
}\n";
    both_compilers_box(MAIN, "inline_sibling_deprecated");
}

/// The control that isolates the inline member as the trigger: the same annotated method in a class
/// with no inline member always compiled.
#[test]
fn a_class_with_no_inline_member_still_compiles() {
    const MAIN: &str = "class Store {\n\
\x20   @Suppress(\"UNCHECKED_CAST\")\n\
\x20   fun tag(): String = \"t\"\n\
\n\
\x20   fun decorate(make: () -> String): String = make() + tag()\n\
}\n\
fun box(): String {\n\
\x20   val decorated = Store().decorate { \"made-\" }\n\
\x20   return if (decorated == \"made-t\") \"OK\" else \"FAIL: \" + decorated\n\
}\n";
    both_compilers_box(MAIN, "no_inline_sibling");
}

/// Skipping released syntax must not skip real checking: an annotation argument of the wrong type is
/// still rejected in a class that has an inline member. Both compilers' output is asserted.
#[test]
fn a_bad_annotation_argument_is_still_rejected_beside_an_inline_member() {
    const MAIN: &str = "class Store {\n\
\x20   @Suppress(1)\n\
\x20   fun tag(): String = \"t\"\n\
\n\
\x20   inline fun decorate(make: () -> String): String = make() + tag()\n\
}\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    expect_identical_rejection(&result, "a bad annotation argument beside an inline member");
}
