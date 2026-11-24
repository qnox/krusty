//! `x::javaClass` selects the stdlib `javaClass` extension property: a reference to a property of
//! the receiver, not the receiver's Java class. kotlinc 2.5 warns about it, and reports an error
//! once `ForbidJavaClassPropertyReferences` is on (`-progressive`); earlier releases accept it
//! silently. A member property of the same name is another declaration and is left alone.

use super::common;

const SOURCE: &str = "class Holder { val javaClass: String = \"\" }

fun test(x: Any, h: Holder) {
    val a = x::javaClass
    val b = Any()::javaClass
    val c = 2::javaClass
    val d = x::javaClass.get()
    val e = Any::javaClass
    val member = h::javaClass

    val direct = x.javaClass
    val literal = Any::class.java
}
";

/// Krusty's and kotlinc's complete `(line, column, message)` reports of one severity.
fn reports(args: &[&str]) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(&[("Refs.kt", SOURCE)], &[], &args);
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let render = |diagnostics: Vec<common::CompilerError>| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}: {}",
                    diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    (
        render(common::compiler_errors(&krusty_output)),
        render(common::compiler_errors(&result.reference_stderr)),
        render(common::compiler_warnings(&krusty_output)),
        render(common::compiler_warnings(&result.reference_stderr)),
    )
}

#[test]
fn a_javaclass_property_reference_is_reported_like_kotlinc() {
    let (krusty_errors, kotlinc_errors, krusty_warnings, kotlinc_warnings) =
        reports(&["-no-reflect"]);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(krusty_warnings, kotlinc_warnings);
    assert!(kotlinc_errors.is_empty());
    let released =
        krusty::kotlin_version::target() >= krusty::kotlin_version::KotlinVersion::V2_5_0_RC;
    // Five stdlib references; the member property `h::javaClass` is not one.
    assert_eq!(kotlinc_warnings.len(), if released { 5 } else { 0 });
}

#[test]
fn the_forbidding_feature_turns_the_reference_into_an_error() {
    let (krusty_errors, kotlinc_errors, krusty_warnings, kotlinc_warnings) = reports(&[
        "-no-reflect",
        "-XXLanguage:+ForbidJavaClassPropertyReferences",
    ]);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(krusty_warnings, kotlinc_warnings);
    assert!(kotlinc_warnings.is_empty());
    let released =
        krusty::kotlin_version::target() >= krusty::kotlin_version::KotlinVersion::V2_5_0_RC;
    assert_eq!(kotlinc_errors.len(), if released { 5 } else { 0 });
}
