//! `null` passed straight to a Java parameter annotated `@NotNull` is an argument mismatch on the
//! resolved call, exactly as kotlinc reports it: `null cannot be a value of a non-null type`, and
//! nothing else. The call is resolved to the one candidate the arguments map onto, so neither an
//! unresolved-static message nor a "none of the following candidates is applicable" list precedes
//! the mismatch: not for a static, an instance member, an overloaded member, or a constructor, and
//! not when the call is the body of a declaration whose result type is inferred.

use super::common;

const NOT_NULL_DECLARATION: &str = "package org.jetbrains.annotations;\n\
    import java.lang.annotation.*;\n\
    @Retention(RetentionPolicy.CLASS)\n\
    @Target({ElementType.METHOD, ElementType.FIELD, ElementType.PARAMETER, \
    ElementType.LOCAL_VARIABLE})\n\
    public @interface NotNull { String value() default \"\"; }\n";

/// The same annotation as the current JetBrains artifact declares it: also a type-use annotation,
/// so javac records it both on the parameter and on the parameter's type.
const NOT_NULL_TYPE_USE: &str = "package org.jetbrains.annotations;\n\
    import java.lang.annotation.*;\n\
    @Retention(RetentionPolicy.CLASS)\n\
    @Target({ElementType.METHOD, ElementType.FIELD, ElementType.PARAMETER, \
    ElementType.LOCAL_VARIABLE, ElementType.TYPE_USE})\n\
    public @interface NotNull { String value() default \"\"; }\n";

const API: &str = "package p;\n\
    import org.jetbrains.annotations.NotNull;\n\
    public class Api {\n\
        public Api() {}\n\
        public Api(@NotNull String name) {}\n\
        public static String stat(@NotNull String value) { return value; }\n\
        public String one(@NotNull String value, int count) { return value; }\n\
        public String over(@NotNull String value) { return value; }\n\
        public String over(int first, int second) { return \"\"; }\n\
    }\n";

const SOURCE: &str = r#"import p.Api

fun statics() {
    Api.stat(null)
}

fun members(api: Api) {
    api.one(null, 1)
    api.over(null)
}

fun constructed() = Api(null)

fun inferredStatic() = Api.stat(null)

fun inferredMember(api: Api) = api.one(null, 1)

fun inferredOverload(api: Api) = api.over(null)
"#;

fn assert_null_argument_diagnostics(annotation: &str) {
    let Some((java_dir, _)) = common::javac_compile(
        &[
            (
                "org/jetbrains/annotations/NotNull.java".to_string(),
                annotation.to_string(),
            ),
            ("p/Api.java".to_string(), API.to_string()),
        ],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let result = common::compiler_diagnostics(
        &[("NullArguments.kt", SOURCE)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }

    let message = "null cannot be a value of a non-null type 'String'.";
    let expected = [
        (4, 14),
        (8, 13),
        (9, 14),
        (12, 25),
        (14, 33),
        (16, 40),
        (18, 43),
    ]
    .into_iter()
    .map(|(line, column)| common::CompilerError {
        file: "NullArguments.kt".to_string(),
        line,
        column,
        message: message.to_string(),
    })
    .collect::<Vec<_>>();
    assert_ne!(result.reference_code, 0, "kotlinc accepted null arguments");
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        expected,
        "kotlinc's diagnostics changed",
    );
    assert_ne!(result.krusty_code, 0, "krusty accepted null arguments");
    let mut krusty = common::compiler_errors(&result.krusty_stderr);
    krusty.extend(common::compiler_errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
}

#[test]
fn null_for_a_declaration_annotated_not_null_parameter_matches_kotlinc() {
    assert_null_argument_diagnostics(NOT_NULL_DECLARATION);
}

#[test]
fn null_for_a_type_use_annotated_not_null_parameter_matches_kotlinc() {
    assert_null_argument_diagnostics(NOT_NULL_TYPE_USE);
}
