//! Exact JVM builtin, catch-type, and modifier diagnostic parity with kotlinc.

use super::common;
use super::diagnostics_parity_support::{errors, first_error, ObservedError};

#[test]
fn jvm_builtin_errors_match_kotlinc() {
    let source = "fun f(a: Array<String>): Array<String> = a.clone(1)";
    let stdlib = common::stdlib_jar();
    let result =
        common::compiler_diagnostics(&[("CloneError.kt", source)], std::slice::from_ref(&stdlib));
    let kr_error =
        first_error(&result.krusty_stderr).or_else(|| first_error(&result.krusty_stdout));
    let kc_error = first_error(&result.reference_stderr);
    assert_ne!(result.krusty_code, 0, "krusty unexpectedly accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    assert_eq!(kr_error, kc_error, "diagnostic mismatch for {source:?}");
}

#[test]
fn kotlin_internal_exact_requires_an_exact_argument_type() {
    let ordinary_source =
        "fun <T> ordinary(value: T) {}\nfun use() = ordinary<CharSequence>(\"x\")";
    let (ordinary_code, ordinary_stderr) =
        common::kotlinc_source_result("Ordinary", ordinary_source);
    assert_eq!(ordinary_code, 0, "{ordinary_stderr}");

    let exact_source = concat!(
        "@Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n",
        "fun <T> exact(value: @kotlin.internal.Exact T) {}\n",
        "fun use() = exact<CharSequence>(\"x\")",
    );
    let result = common::compiler_diagnostics(&[("Exact.kt", exact_source)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "@Exact unexpectedly accepted the widened type"
    );
    assert_ne!(result.krusty_code, 0, "krusty unexpectedly accepted @Exact");
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(krusty_errors.len(), 1);
}

#[test]
fn kotlin_internal_exact_can_require_two_arguments_to_have_the_same_type() {
    let source = concat!(
        "@Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n",
        "fun <T> same(first: @kotlin.internal.Exact T, second: @kotlin.internal.Exact T) {}\n",
        "fun use(first: String, second: CharSequence) = same(first, second)",
    );
    let result = common::compiler_diagnostics(&[("ExactPair.kt", source)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "two @Exact parameters accepted different types"
    );
    assert_eq!(
        errors(&result.reference_stderr),
        vec![ObservedError {
            file: "ExactPair.kt".to_string(),
            line: 3,
            column: 60,
            message:
                "argument type mismatch: actual type is 'CharSequence', but 'String' was expected."
                    .to_string(),
        }]
    );
}

#[test]
fn cross_file_generic_diagnostic_matches_kotlinc() {
    let result = common::compiler_diagnostics(
        &[
            ("declaration.kt", "fun <T> id(x: T): T = x"),
            ("use.kt", "fun use(): Int = id(1, 2)"),
        ],
        &[],
    );
    let krusty_error =
        first_error(&result.krusty_stderr).or_else(|| first_error(&result.krusty_stdout));
    let kotlinc_error = first_error(&result.reference_stderr);

    assert_ne!(result.krusty_code, 0, "krusty unexpectedly accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    assert_eq!(krusty_error, kotlinc_error);
}

#[test]
fn unresolved_catch_type_reports_only_the_unresolved_reference() {
    let source = "fun f() { try {} catch (e: DefinitelyMissingException) {} }\n";
    let result = common::compiler_diagnostics(&[("MissingCatch.kt", source)], &[]);
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![ObservedError {
        file: "MissingCatch.kt".to_string(),
        line: 1,
        column: 28,
        message: "unresolved reference 'DefinitelyMissingException'.".to_string(),
    }];

    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(result.reference_code, 0, "kotlinc silently accepted source");
    assert_eq!(krusty_errors.len(), 1);
    assert_eq!(kotlinc_errors.len(), 1);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn non_throwable_catch_type_matches_kotlinc_throwable_mismatch() {
    let stdlib = common::stdlib_jar();
    for (source, line, message) in [
        (
            "fun f() { try {} catch (e: Int) {} }\n",
            1,
            "throwable type mismatch: actual type is 'Int'.",
        ),
        (
            "fun f() { try {} catch (e: () -> Unit) {} }\n",
            1,
            "throwable type mismatch: actual type is '() -> Unit'.",
        ),
        (
            "fun f() { try {} catch (e: RuntimeException?) {} }\n",
            1,
            "throwable type mismatch: actual type is 'RuntimeException?'.",
        ),
        (
            "fun f() { try {} catch (e: String) {} }\n",
            1,
            "throwable type mismatch: actual type is 'String'.",
        ),
        (
            "class Plain\nfun f() { try {} catch (e: Plain) {} }\n",
            2,
            "throwable type mismatch: actual type is 'Plain'.",
        ),
    ] {
        let result = common::compiler_diagnostics(
            &[("CatchMismatch.kt", source)],
            std::slice::from_ref(&stdlib),
        );
        let mut krusty_errors = errors(&result.krusty_stderr);
        krusty_errors.extend(errors(&result.krusty_stdout));
        let kotlinc_errors = errors(&result.reference_stderr);
        let expected = vec![ObservedError {
            file: "CatchMismatch.kt".to_string(),
            line,
            column: 25,
            message: message.to_string(),
        }];

        assert_ne!(result.krusty_code, 0, "krusty accepted {source:?}");
        assert_ne!(result.reference_code, 0, "kotlinc accepted {source:?}");
        assert_eq!(krusty_errors.len(), 1, "source: {source:?}");
        assert_eq!(kotlinc_errors.len(), 1, "source: {source:?}");
        assert_eq!(krusty_errors, expected, "source: {source:?}");
        assert_eq!(kotlinc_errors, expected, "source: {source:?}");
    }
}

#[test]
fn throwable_catch_types_are_accepted() {
    let source = "class LocalProblem : RuntimeException()\n\
        fun a() { try {} catch (e: Throwable) {} }\n\
        fun b() { try {} catch (e: Exception) {} }\n\
        fun c() { try {} catch (e: RuntimeException) {} }\n\
        fun d() { try {} catch (e: NotImplementedError) {} }\n\
        fun e() { try {} catch (e: LocalProblem) {} }\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("ThrowableCatch.kt", source)],
        std::slice::from_ref(&stdlib),
    );

    assert_eq!(result.krusty_code, 0, "{}", result.krusty_stderr);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(errors(&result.krusty_stderr), []);
    assert_eq!(errors(&result.krusty_stdout), []);
    assert_eq!(errors(&result.reference_stderr), []);
}

#[test]
fn classpath_catch_types_follow_the_declared_hierarchy() {
    let library = common::compile_libs_ref(
        "catch_type_hierarchy",
        &[(
            "Library.kt",
            "package lib\nclass ExternalProblem : RuntimeException()\nclass ExternalPlain",
        )],
    )
    .expect("reference compiler unavailable");
    let source = "fun accepted() { try {} catch (e: lib.ExternalProblem) {} }\n\
        fun rejected() { try {} catch (e: lib.ExternalPlain) {} }\n";
    let result = common::compiler_diagnostics(
        &[("ClasspathCatch.kt", source)],
        &[library, common::stdlib_jar()],
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![ObservedError {
        file: "ClasspathCatch.kt".to_string(),
        line: 2,
        column: 32,
        message: "throwable type mismatch: actual type is 'ExternalPlain'.".to_string(),
    }];

    assert_ne!(result.krusty_code, 0, "krusty accepted ExternalPlain");
    assert_ne!(result.reference_code, 0, "kotlinc accepted ExternalPlain");
    assert_eq!(krusty_errors.len(), 1);
    assert_eq!(kotlinc_errors.len(), 1);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

/// `tailrec` on an OVERRIDABLE member, rejected identically by both compilers.
///
/// The modifier promises a loop, and a loop is sound only when the self-call cannot dispatch
/// elsewhere. In an open member `f(n - 1)` on `this` is a virtual call a subclass may answer, so
/// rewriting it would devirtualize into the base frame and silently run the wrong body. kotlinc
/// rejects the declaration rather than quietly skipping the rewrite; so does krusty, at the same
/// line and column and with the same text — the modifier's own span, not the function's name.
#[test]
fn tailrec_on_an_open_member_is_rejected_by_both_frontends() {
    let source = "open class C {\n\
                  \x20   tailrec open fun f(n: Int, acc: Int): Int =\n\
                  \x20       if (n == 0) acc else f(n - 1, acc + 1)\n\
                  }\n";
    let result = common::compiler_diagnostics(&[("Open.kt", source)], &[common::stdlib_jar()]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));

    let expected = vec![ObservedError {
        file: "Open.kt".to_string(),
        line: 2,
        column: 5,
        message: "tailrec is prohibited on open members.".to_string(),
    }];
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}

/// The other side of the same rule, so the check above cannot be "reject every `tailrec` member".
///
/// Overridability is a property of the member AND its owner. A bare `override` stays open, but only
/// while something can subclass the class holding it — so kotlinc ACCEPTS this exact member in a
/// final class and rejects it once the class is `open`. Both compilers take the same program.
#[test]
fn tailrec_overriding_in_a_final_class_is_accepted_by_both_frontends() {
    let source = "open class B { open fun f(n: Int, acc: Int): Int = acc }\n\
                  class D : B() {\n\
                  \x20   tailrec override fun f(n: Int, acc: Int): Int =\n\
                  \x20       if (n == 0) acc else f(n - 1, acc + 1)\n\
                  }\n";
    let (code, stderr) = common::kotlinc_source_result("FinalOverride", source);
    assert_eq!(
        code, 0,
        "kotlinc rejected a final overriding tailrec: {stderr}"
    );
    common::expect_front_end_ok_files_with_stdlib(&[source], "final overriding tailrec parity");
}
