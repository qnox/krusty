//! kotlinc's value-class primary-constructor parameter checks (`FirValueClassDeclarationChecker`):
//! a parameter that is not a final read-only property, an underlying type expanding to `Unit` or
//! `Nothing`, and an underlying type that reaches its own value class again. Recursion through a
//! type parameter's bound is a deprecation warning until
//! `ForbidValueClassRecursionViaTypeParameters` makes it an error.

use super::common::{self, CompilerOutcome};
use krusty::compilation_target::CompilationTarget;
use krusty::kotlin_version::KotlinVersion;

const FORBID: &str = "-XXLanguage:+ForbidValueClassRecursionViaTypeParameters";

/// Compile `source` with both compilers, passing `args` to each, and require the same complete
/// errors and warnings, which must also be `errors` and `warnings`.
fn assert_matches_kotlinc(source: &str, args: &[&str], errors: &[&str], warnings: &[&str]) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(
        &[("ValueClasses.kt", source)],
        &[common::stdlib_jar()],
        &args,
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty_errors = common::compiler_errors(&krusty_output);
    let kotlinc_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    let krusty_warnings = common::compiler_warnings(&krusty_output);
    let kotlinc_warnings = common::compiler_warnings(&result.reference_stderr);
    assert_eq!(
        krusty_warnings, kotlinc_warnings,
        "warnings\n{krusty_output}"
    );
    let render = |diagnostics: &[common::CompilerError]| {
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
    assert_eq!(render(&kotlinc_errors), errors);
    assert_eq!(render(&kotlinc_warnings), warnings);
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

const SHAPES: &str = "\
typealias U = Unit
@JvmInline value class A(var x: Int)
@JvmInline value class B(x: Int)
@JvmInline value class C(val x: Unit)
@JvmInline value class D(vararg val x: Int)
@JvmInline value class E(val x: Nothing)
@JvmInline value class F(val x: U)
@JvmInline value class G(val x: Unit?)
@JvmInline value class R1(val x: R1)
@JvmInline value class R2(val x: R2?)
@JvmInline value class M1(val x: M2)
@JvmInline value class M2(val x: M1)
@JvmInline value class W<T>(val x: T)
@JvmInline value class R3(val x: W<R3>)
@JvmInline value class Ok1(val x: Int)
@JvmInline value class Ok2(private val x: String)
";

#[test]
fn value_class_parameter_shapes_match_kotlinc() {
    assert_matches_kotlinc(
        SHAPES,
        &[],
        &[
            "2:26: value class primary constructor must only have final read-only ('val') property parameters.",
            "3:26: value class primary constructor must only have final read-only ('val') property parameters.",
            "4:33: value class cannot have value parameter of type 'kotlin/Unit'.",
            "5:26: value class primary constructor must only have final read-only ('val') property parameters.",
            "6:33: value class cannot have value parameter of type 'kotlin/Nothing'.",
            "7:33: value class cannot have value parameter of type '{U=} kotlin/Unit'.",
            "9:34: value class cannot be recursive.",
            "10:34: value class cannot be recursive.",
            "11:34: value class cannot be recursive.",
            "12:34: value class cannot be recursive.",
        ],
        &[],
    );
}

#[test]
fn source_typealias_recursion_is_checked_without_substituting_type_arguments() {
    assert_matches_kotlinc(
        "typealias A = R\n@JvmInline value class R(val x: A)\n",
        &[],
        &["2:33: value class cannot be recursive."],
        &[],
    );
}

#[test]
fn explicit_open_override_property_is_not_final_read_only() {
    assert_matches_kotlinc(
        "interface I { val x: Int }\n\
         @JvmInline value class V(open override val x: Int) : I\n",
        &[],
        &[
            "2:26: value class primary constructor must only have final read-only ('val') property \
             parameters.",
        ],
        &[],
    );
}

const VIA_TYPE_PARAMETERS: &str = "\
@JvmInline value class T1<T : T1<T>>(val x: T)
@JvmInline value class T2<T : T2<T>>(val x: T?)
@JvmInline value class T3<T : S, S : R, R : T3<T, S, R>>(val x: R)
@JvmInline value class T4<T>(val x: T) where T : T4<T>, T : CharSequence
@JvmInline value class T5<T : T5<T>?>(val x: T)
";

/// kotlinc compiles these declarations with only the warning, but its JVM carrier for such a class
/// is the boxed class itself, which krusty's value-class representation does not model: krusty's
/// front end is compared here, and the forbidden form below compares the complete compilation.
#[test]
fn recursion_through_type_parameters_matches_the_selected_release() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        assert_matches_kotlinc(
            VIA_TYPE_PARAMETERS,
            &[],
            &[
                "1:45: value class cannot be recursive.",
                "2:45: value class cannot be recursive.",
                "3:65: value class cannot be recursive.",
                "5:46: value class cannot be recursive.",
            ],
            &[],
        );
        return;
    }
    let warning = "value class cannot be recursive. This will become an error in language \
                   version 2.5. See https://youtrack.jetbrains.com/issue/KT-85848.";
    let expected = [
        format!("1:45: warning: {warning}"),
        format!("2:45: warning: {warning}"),
        format!("3:65: warning: {warning}"),
        format!("5:46: warning: {warning}"),
    ];
    let result = common::compiler_diagnostics_with_shared_args(
        &[("ValueClasses.kt", VIA_TYPE_PARAMETERS)],
        &[common::stdlib_jar()],
        &[],
    );
    assert!(common::compiler_errors(&result.reference_stderr).is_empty());
    let kotlinc = common::compiler_warnings(&result.reference_stderr)
        .into_iter()
        .map(|warning| {
            format!(
                "{}:{}: warning: {}",
                warning.line, warning.column, warning.message
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(kotlinc, expected);
    let krusty = common::front_end_diagnostics_located(
        VIA_TYPE_PARAMETERS,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(krusty, expected);
}

#[test]
fn recursion_warning_keeps_its_typed_policy_identity() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    const SOURCE: &str = "@JvmInline value class W<T : W<T>>(val x: T)\n";
    let diagnostics = common::front_end_diagnostic_records_for(
        CompilationTarget::Jvm,
        SOURCE,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.span, krusty::diag::Span::new(42, 43));
    assert_eq!(diagnostic.editor_span, None);
    assert_eq!(diagnostic.severity, krusty::diag::Severity::Warning);
    assert_eq!(
        diagnostic.name,
        Some(krusty::diag::names::VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING,)
    );
    assert_eq!(diagnostic.kind, krusty::diag::DiagnosticKind::Compiler);
    assert_eq!(
        diagnostic.message(krusty::diag::DiagnosticNames::Hidden),
        "value class cannot be recursive. This will become an error in language version 2.5. \
         See https://youtrack.jetbrains.com/issue/KT-85848."
    );
    assert_eq!(diagnostic.identity, None);
    assert_eq!(diagnostic.file, 0);
}

#[test]
fn forbidden_recursion_through_type_parameters_is_an_error() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    assert_matches_kotlinc(
        VIA_TYPE_PARAMETERS,
        &[FORBID],
        &[
            "1:45: value class cannot be recursive.",
            "2:45: value class cannot be recursive.",
            "3:65: value class cannot be recursive.",
            "5:46: value class cannot be recursive.",
        ],
        &[],
    );
}

#[test]
fn suppression_does_not_hide_forbidden_recursion() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    assert_matches_kotlinc(
        "@Suppress(\"VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING\")\n\
         @JvmInline value class T3<T : T3<T>>(val x: T)\n",
        &[FORBID],
        &["2:45: value class cannot be recursive."],
        &[],
    );
}

const SUPPRESSED: &str = "\
@JvmInline value class T1<T : T1<T>>(@Suppress(\"VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING\") val x: T)
@Suppress(\"VALUE_CLASS_CANNOT_BE_RECURSIVE_VIA_TYPE_PARAMETERS_WARNING\")
@JvmInline value class T2<T : T2<T>>(val x: T)
@JvmInline value class T3<T : T3<T>>(val x: T)
";

/// The warning's name suppresses it on the parameter that holds it as well as on the class.
#[test]
fn recursion_warning_is_suppressed_on_the_parameter_and_the_class() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    let expected = [
        "4:45: warning: value class cannot be recursive. This will become an error \
                     in language version 2.5. See https://youtrack.jetbrains.com/issue/KT-85848.",
    ];
    let result = common::compiler_diagnostics_with_shared_args(
        &[("ValueClasses.kt", SUPPRESSED)],
        &[common::stdlib_jar()],
        &[],
    );
    assert!(common::compiler_errors(&result.reference_stderr).is_empty());
    let kotlinc = common::compiler_warnings(&result.reference_stderr)
        .into_iter()
        .map(|warning| {
            format!(
                "{}:{}: warning: {}",
                warning.line, warning.column, warning.message
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(kotlinc, expected);
    let krusty = common::front_end_diagnostics_located(
        SUPPRESSED,
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(krusty, expected);
}

/// Full value classes have one semantic field per constructor property. Recursion checks traverse
/// that selected multi-field representation instead of treating the absence of an inline
/// underlying property as absence of value-class structure.
#[test]
fn native_full_value_representation_is_checked_for_recursion() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    let source = "// LANGUAGE: +FullValueClasses\n\
value class V(val n: Int, val self: V)\n";
    let expected = CompilerOutcome {
        accepted: false,
        errors: vec!["V.kt:2:37: value class cannot be recursive.".to_string()],
    };
    let actual = common::front_end_diagnostics_located_for(
        CompilationTarget::Native,
        source,
        &[common::stdlib_jar()],
        None,
    )
    .into_iter()
    .map(|located| {
        let (position, message) = located
            .split_once(": error: ")
            .unwrap_or_else(|| panic!("unexpected located diagnostic: {located}"));
        format!("V.kt:{position}: {message}")
    })
    .collect::<Vec<_>>();
    assert_eq!(
        CompilerOutcome {
            accepted: actual.is_empty(),
            errors: actual,
        },
        expected
    );

    let args = vec!["-XXLanguage:+FullValueClasses".to_string()];
    if let Some(reference) =
        common::reference_outcome(CompilationTarget::Native, &[("V.kt", source)], &args)
    {
        assert_eq!(reference, expected);
    }
}

/// Full-value recursion follows every non-null field, also follows a nullable one-field
/// representation, and stops at a nullable multi-field representation.
#[test]
fn native_full_value_recursion_respects_field_count_and_nullability() {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    let source = "// LANGUAGE: +FullValueClasses\n\
value class A(val n: Int, val b: B)\n\
value class B(val n: Int, val a: A)\n\
value class S(val next: S?)\n\
value class M(val n: Int, val next: M?)\n";
    let expected = CompilerOutcome {
        accepted: false,
        errors: vec![
            "V.kt:2:34: value class cannot be recursive.".to_string(),
            "V.kt:3:34: value class cannot be recursive.".to_string(),
            "V.kt:4:25: value class cannot be recursive.".to_string(),
        ],
    };
    let actual = common::front_end_diagnostics_located_for(
        CompilationTarget::Native,
        source,
        &[common::stdlib_jar()],
        None,
    )
    .into_iter()
    .map(|located| {
        let (position, message) = located
            .split_once(": error: ")
            .unwrap_or_else(|| panic!("unexpected located diagnostic: {located}"));
        format!("V.kt:{position}: {message}")
    })
    .collect::<Vec<_>>();
    assert_eq!(
        CompilerOutcome {
            accepted: actual.is_empty(),
            errors: actual,
        },
        expected
    );

    let args = vec!["-XXLanguage:+FullValueClasses".to_string()];
    if let Some(reference) =
        common::reference_outcome(CompilationTarget::Native, &[("V.kt", source)], &args)
    {
        assert_eq!(reference, expected);
    }
}
