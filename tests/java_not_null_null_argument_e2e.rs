//! `null` passed straight to a Java parameter annotated `@NotNull` is an argument mismatch on the
//! resolved call, exactly as kotlinc reports it: `null cannot be a value of a non-null type`, and
//! nothing else. The call is resolved to the one candidate the arguments map onto, so neither an
//! unresolved-static message nor a "none of the following candidates is applicable" list precedes
//! the mismatch: not for a static, an instance member, an overloaded member, or a constructor, and
//! not when the call is the body of a declaration whose result type is inferred. A generic call keeps
//! its result whether a written type argument or another mapped argument fixes it, and a
//! package-private static the arguments do not fit reports only the argument, as kotlinc does.

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

const NULLABLE_DECLARATION: &str = "package org.jetbrains.annotations;\n\
    import java.lang.annotation.*;\n\
    @Retention(RetentionPolicy.CLASS)\n\
    @Target({ElementType.METHOD, ElementType.FIELD, ElementType.PARAMETER, \
    ElementType.LOCAL_VARIABLE})\n\
    public @interface Nullable { String value() default \"\"; }\n";

const API: &str = "package p;\n\
    import org.jetbrains.annotations.NotNull;\n\
    import org.jetbrains.annotations.Nullable;\n\
    public class Api {\n\
        public Api() {}\n\
        public Api(@NotNull String name) {}\n\
        public static String stat(@NotNull String value) { return value; }\n\
        public String one(@NotNull String value, int count) { return value; }\n\
        public String over(@NotNull String value) { return value; }\n\
        public String over(int first, int second) { return \"\"; }\n\
        static String hidden(@NotNull String value) { return value; }\n\
        public <T> T parse(@NotNull String value) { return null; }\n\
        public String parse(@NotNull CharSequence value) { return value.toString(); }\n\
        public <T> T parse(int first, int second) { return null; }\n\
        public static <T> T parseStatic(@NotNull String value) { return null; }\n\
        public <T> T take(T value, @NotNull String required) { return value; }\n\
        @Nullable public <T> T nullable(@NotNull String value) { return null; }\n\
        public static <T extends Number> T bounded(String value) { return null; }\n\
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

fun packagePrivateMismatch() {
    Api.hidden(null)
}

fun packagePrivateAccess() {
    Api.hidden("x")
}

fun inferredGeneric(api: Api) = api.parse<String>(null).length

fun inferredStaticGeneric() = Api.parseStatic<String>(null).length

fun inferredArgumentGeneric(api: Api) = api.take(1, null).inc()

fun nullableGeneric(api: Api) = api.nullable<String>(null).length

fun invalidStaticBound() = Api.bounded<String>("x").length
"#;

const BOUNDED_MEMBER: &str = r#"class BoundedApi {
    fun <T : Number> bounded(value: String): T = TODO()
}

fun inferredBound(api: BoundedApi) = api.bounded<String>(null).length
"#;

fn assert_null_argument_diagnostics(annotation: &str) {
    let Some((java_dir, _)) = common::javac_compile(
        &[
            (
                "org/jetbrains/annotations/NotNull.java".to_string(),
                annotation.to_string(),
            ),
            (
                "org/jetbrains/annotations/Nullable.java".to_string(),
                NULLABLE_DECLARATION.to_string(),
            ),
            ("p/Api.java".to_string(), API.to_string()),
        ],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let result = common::compiler_diagnostics(
        &[
            ("NullArguments.kt", SOURCE),
            ("ZBoundedCall.kt", BOUNDED_MEMBER),
        ],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }

    let null_argument = "null cannot be a value of a non-null type 'String'.";
    let package_private =
        "cannot access 'static fun hidden(p0: String): String!': it is package-private in 'p.Api'.";
    let expected = [
        (4, 14, null_argument),
        (8, 13, null_argument),
        (9, 14, null_argument),
        (12, 25, null_argument),
        (14, 33, null_argument),
        (16, 40, null_argument),
        (18, 43, null_argument),
        // An inaccessible candidate the arguments do not fit reports only the argument; the access
        // error belongs to the call that would otherwise apply.
        (21, 16, null_argument),
        (25, 9, package_private),
        // The rejected generic call keeps its explicit `T`: `.length` resolves on `String`.
        (28, 51, null_argument),
        (30, 55, null_argument),
        // A mapped valid argument infers `T = Int` before the rejected argument; `.inc()` resolves.
        (32, 53, null_argument),
        // The declaration's nullable return policy applies after `T = String`; the rejected
        // argument and the unsafe dereference remain independent diagnostics.
        (34, 54, null_argument),
        (
            34,
            59,
            "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
        ),
        (
            36,
            32,
            "inapplicable candidate(s): static fun <T : Number!> bounded(p0: String!): T!",
        ),
        (
            36,
            40,
            "type argument is not within its bounds: type parameter 'T (of fun <T : Number!> bounded)' must be subtype of 'Number!', but actual: 'String!'.",
        ),
    ]
    .into_iter()
    .map(|(line, column, message)| common::CompilerError {
        file: "NullArguments.kt".to_string(),
        line,
        column,
        message: message.to_string(),
    })
    .chain([
        common::CompilerError {
            file: "ZBoundedCall.kt".to_string(),
            line: 5,
            column: 50,
            message: "type argument is not within its bounds: type parameter 'T (of fun <T : Number> bounded)' must be subtype of 'Number', but actual: 'String'.".to_string(),
        },
        common::CompilerError {
            file: "ZBoundedCall.kt".to_string(),
            line: 5,
            column: 58,
            message: null_argument.to_string(),
        },
    ])
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
