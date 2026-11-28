//! A positional argument of a Java annotation written as a constructor call (an annotation nested
//! in another annotation's argument) is accepted by kotlinc 2.4, warned about by 2.5, and an error
//! once `EnforceMissingNamedArgumentsOnJavaAnnotation` is on. The same argument on a top-level
//! annotation use is an error regardless.

use super::common;

const NESTED: &str = "import jl.Inner
import jl.Outer

@Outer(inner = Inner(1), many = [Inner(2), Inner(b = \"x\")]) class Nested
";

const TOP_LEVEL: &str = "import jl.Inner

@Inner(3) class TopLevel
";

fn library() -> std::path::PathBuf {
    let java = [
        (
            "Inner.java".to_string(),
            "package jl;\npublic @interface Inner { int a() default 1; String b() default \"\"; }\n"
                .to_string(),
        ),
        (
            "Outer.java".to_string(),
            "package jl;\npublic @interface Outer { Inner inner(); Inner[] many() default {}; }\n"
                .to_string(),
        ),
    ];
    common::javac_compile(&java, &[])
        .map(|(dir, _)| dir)
        .expect("javac must build the Java annotation fixture")
}

/// Krusty's and kotlinc's complete `file:line:column: message` reports: errors, then warnings.
fn reports(source: &str, args: &[&str]) -> [(Vec<String>, Vec<String>); 2] {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result =
        common::compiler_diagnostics_with_shared_args(&[("Use.kt", source)], &[library()], &args);
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let render = |diagnostics: Vec<common::CompilerError>| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}:{}: {}",
                    diagnostic.file, diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    [
        (
            render(common::compiler_errors(&krusty_output)),
            render(common::compiler_errors(&result.reference_stderr)),
        ),
        (
            render(common::compiler_warnings(&krusty_output)),
            render(common::compiler_warnings(&result.reference_stderr)),
        ),
    ]
}

fn released() -> bool {
    krusty::kotlin_version::target() >= krusty::kotlin_version::KotlinVersion::V2_5_0_RC
}

#[test]
fn a_positional_argument_of_a_nested_java_annotation_warns_before_the_feature() {
    let [(krusty_errors, kotlinc_errors), (krusty_warnings, kotlinc_warnings)] =
        reports(NESTED, &["-no-reflect"]);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(krusty_warnings, kotlinc_warnings);
    assert!(kotlinc_errors.is_empty());
    if released() {
        assert_eq!(
            kotlinc_warnings,
            [
                "Use.kt:4:22: only named arguments are available for Java annotations. This will become \
                 an error in language version 2.6. See: \
                 https://youtrack.jetbrains.com/issue/KTLC-418",
                "Use.kt:4:40: only named arguments are available for Java annotations. This will become \
                 an error in language version 2.6. See: \
                 https://youtrack.jetbrains.com/issue/KTLC-418",
            ]
        );
    } else {
        assert!(kotlinc_warnings.is_empty());
    }
}

#[test]
fn the_enforcing_feature_rejects_a_positional_argument_of_a_nested_java_annotation() {
    if !released() {
        return;
    }
    let [(krusty_errors, kotlinc_errors), (krusty_warnings, kotlinc_warnings)] = reports(
        NESTED,
        &[
            "-no-reflect",
            "-XXLanguage:+EnforceMissingNamedArgumentsOnJavaAnnotation",
        ],
    );
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(krusty_warnings, kotlinc_warnings);
    assert!(kotlinc_warnings.is_empty());
    assert_eq!(
        kotlinc_errors,
        [
            "Use.kt:4:22: only named arguments are available for Java annotations.",
            "Use.kt:4:40: only named arguments are available for Java annotations.",
        ]
    );
}

#[test]
fn a_positional_argument_of_a_top_level_java_annotation_is_an_error_without_the_feature() {
    let [(krusty_errors, kotlinc_errors), _] = reports(TOP_LEVEL, &["-no-reflect"]);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        kotlinc_errors,
        ["Use.kt:3:8: only named arguments are available for Java annotations."]
    );
}
