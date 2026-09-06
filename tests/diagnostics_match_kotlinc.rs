//! Exact diagnostic comparisons between krusty and kotlinc.

use std::path::Path;

use super::common;

#[derive(Debug, PartialEq, Eq)]
struct ObservedError {
    file: String,
    line: usize,
    column: usize,
    message: String,
}

const RECURSIVE_INFERENCE_MESSAGE: &str = "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.";

/// Extract every `path:line:column: error: message` record. Compare the basename because both
/// compilers receive the same path, while their renderers may independently canonicalize its prefix.
fn errors(output: &str) -> Vec<ObservedError> {
    output
        .lines()
        .filter_map(|rendered| {
            let (location, message) = rendered.split_once("error:")?;
            let location = location.trim().trim_end_matches(':');
            let mut fields = location.rsplitn(3, ':');
            let column = fields.next()?.trim().parse().ok()?;
            let line = fields.next()?.trim().parse().ok()?;
            let path = fields.next()?.trim();
            let file = Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(path)
                .to_string();
            Some(ObservedError {
                file,
                line,
                column,
                message: message.trim().to_string(),
            })
        })
        .collect()
}

fn first_error(output: &str) -> Option<ObservedError> {
    errors(output).into_iter().next()
}

#[test]
fn generic_cast_is_accepted_by_both_frontends() {
    let source = "fun <T> materialize(): T = 42 as T";
    let (code, stderr) = common::kotlinc_source_result("GenericCast", source);
    assert_eq!(code, 0, "kotlinc rejected generic cast: {stderr}");
    common::expect_front_end_ok_files_with_stdlib(&[source], "generic cast parity");
}

#[test]
fn anonymous_objects_do_not_restore_cut_outer_type_parameters() {
    let companion = "class CompanionOuter<T> {\n\
                     \x20   companion object {\n\
                     \x20       val marker = object {\n\
                     \x20           fun value(): T = error(\"unreachable\")\n\
                     \x20       }\n\
                     \x20   }\n\
                     }\n";
    let nested = "class NestedOuter<T> {\n\
                  \x20   class Nested {\n\
                  \x20       val marker = object {\n\
                  \x20           fun value(): T = error(\"unreachable\")\n\
                  \x20       }\n\
                  \x20   }\n\
                  }\n";
    let result = common::compiler_diagnostics(
        &[
            ("AnonymousObjectCompanionCut.kt", companion),
            ("AnonymousObjectNestedCut.kt", nested),
        ],
        &[common::stdlib_jar()],
    );
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));

    let expected = vec![
        ObservedError {
            file: "AnonymousObjectCompanionCut.kt".to_string(),
            line: 4,
            column: 26,
            message: "unresolved reference 'T'.".to_string(),
        },
        ObservedError {
            file: "AnonymousObjectNestedCut.kt".to_string(),
            line: 4,
            column: 26,
            message: "unresolved reference 'T'.".to_string(),
        },
    ];
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}

#[test]
fn mutable_local_smart_cast_diagnostics_match_kotlinc() {
    let result = common::compiler_diagnostics(
        &[
            (
                "ActiveClosure.kt",
                "fun active(): Int {\n\
                 var text: String? = \"abc\"\n\
                 val mutate = { text = null }\n\
                 if (text != null) {\n\
                     return text.length\n\
                 }\n\
                 return -1\n\
                 }\n",
            ),
            (
                "ClosureCreatedInsideProof.kt",
                "fun createdInsideProof(): Int {\n\
                 var text: String? = \"abc\"\n\
                 if (text != null) {\n\
                     val mutate = { text = null }\n\
                     return text.length\n\
                 }\n\
                 return -1\n\
                 }\n",
            ),
            (
                "ElvisArgument.kt",
                "fun take(value: Int) {}\n\
                 fun elvisArgument() {\n\
                 var value: Int? = 5\n\
                 val reset = { value = null }\n\
                 value ?: return\n\
                 reset()\n\
                 take(value)\n\
                 }\n",
            ),
            (
                "FutureClosure.kt",
                "fun futureClosure(): Int {\n\
                 var text: String? = \"abc\"\n\
                 if (text != null) {\n\
                     val length = text.length\n\
                     val mutate = { text = null }\n\
                     return length\n\
                 }\n\
                 return -1\n\
                 }\n",
            ),
        ],
        &[],
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let message = "smart cast to 'String' is impossible, because 'text' is a local variable that is mutated in a capturing closure.".to_string();
    let expected = vec![
        ObservedError {
            file: "ActiveClosure.kt".to_string(),
            line: 5,
            column: 8,
            message: message.clone(),
        },
        ObservedError {
            file: "ClosureCreatedInsideProof.kt".to_string(),
            line: 5,
            column: 8,
            message,
        },
        ObservedError {
            file: "ElvisArgument.kt".to_string(),
            line: 7,
            column: 6,
            message: "smart cast to 'Int' is impossible, because 'value' is a local variable that is mutated in a capturing closure.".to_string(),
        },
    ];
    assert_eq!(krusty_errors.len(), 3);
    assert_eq!(kotlinc_errors.len(), 3);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn colliding_classifier_names_are_qualified_in_type_mismatches() {
    let result = common::compiler_diagnostics(
        &[
            ("a/Foo.kt", "package a\nclass Foo"),
            ("b/Foo.kt", "package b\nclass Foo"),
            (
                "Main.kt",
                "fun take(value: a.Foo) {}\n\nfun use() {\n    take(b.Foo())\n}",
            ),
        ],
        &[],
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![ObservedError {
        file: "Main.kt".to_string(),
        line: 4,
        column: 10,
        message: "argument type mismatch: actual type is 'b.Foo', but 'a.Foo' was expected."
            .to_string(),
    }];

    assert_eq!(krusty_errors.len(), 1);
    assert_eq!(kotlinc_errors.len(), 1);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn explicit_empty_array_type_is_not_replaced_by_a_projected_expectation() {
    let result = common::compiler_diagnostics(
        &[(
            "ExplicitEmptyArray.kt",
            "fun take(values: Array<out String>) {}\n\nfun use() {\n    take(emptyArray<Any>())\n}",
        )],
        &[common::stdlib_jar()],
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![ObservedError {
        file: "ExplicitEmptyArray.kt".to_string(),
        line: 4,
        column: 10,
        message: "argument type mismatch: actual type is 'Array<Any>', but 'Array<out String>' was expected."
            .to_string(),
    }];

    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    assert_eq!(krusty_errors.len(), 1);
    assert_eq!(kotlinc_errors.len(), 1);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn empty_reference_array_does_not_match_a_primitive_array_parameter() {
    let result = common::compiler_diagnostics(
        &[(
            "EmptyReferenceForPrimitive.kt",
            "fun take(values: IntArray) {}\n\nfun use() {\n    take(emptyArray<Int>())\n}",
        )],
        &[common::stdlib_jar()],
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![ObservedError {
        file: "EmptyReferenceForPrimitive.kt".to_string(),
        line: 4,
        column: 10,
        message:
            "argument type mismatch: actual type is 'Array<Int>', but 'IntArray' was expected."
                .to_string(),
    }];

    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    assert_eq!(krusty_errors.len(), 1);
    assert_eq!(kotlinc_errors.len(), 1);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn subjectless_when_fallthrough_diagnostics_match_kotlinc() {
    let result = common::compiler_diagnostics(
        &[(
            "WhenFallthrough.kt",
            "fun noNarrowing(text: String?, choose: Boolean): Int = when {\n\
             choose -> text.length\n\
             else -> -1\n\
             }\n\
             fun capturedMutation(): Int {\n\
             var text: String? = \"abc\"\n\
             val mutate = { text = null }\n\
             return when {\n\
             text == null -> -1\n\
             else -> text.length\n\
             }\n\
             }\n",
        )],
        &[],
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![
        ObservedError {
            file: "WhenFallthrough.kt".to_string(),
            line: 2,
            column: 15,
            message: "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.".to_string(),
        },
        ObservedError {
            file: "WhenFallthrough.kt".to_string(),
            line: 10,
            column: 9,
            message: "smart cast to 'String' is impossible, because 'text' is a local variable that is mutated in a capturing closure.".to_string(),
        },
    ];
    assert_eq!(krusty_errors.len(), 2);
    assert_eq!(kotlinc_errors.len(), 2);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn declaration_type_parameter_annotations_are_accepted_by_both_frontends() {
    let source = r#"
@Target(AnnotationTarget.TYPE_PARAMETER)
annotation class Marker(val value: String)

class Box<@Marker("class") T>(val value: T)

class Lines<
    @Marker("line") T
>

class Bound<@Marker("bound") T:
Any>

typealias Boxed<@Marker("alias") T> = List<T>

class Host {
    @Target(AnnotationTarget.TYPE_PARAMETER)
    annotation class Marker

    fun <@Marker T> keep(value: T): T = value

    fun outer(): String {
        fun <@Marker T> local(value: T): T = value
        return local("OK")
    }
}

interface Contract {
    @Target(AnnotationTarget.TYPE_PARAMETER)
    annotation class Marker

    fun <@Marker T> member(value: T): T
}

inline fun <
    reified @Marker("function") T
> choose(value: T): T = value

fun box(): Boxed<String> = listOf(
    Box(choose("OK")).value,
    Host().keep("OK"),
    Host().outer(),
)
"#;
    let (code, stderr) = common::kotlinc_source_result("AnnotatedTypeParameters", source);
    assert_eq!(
        code, 0,
        "kotlinc rejected annotated type parameters: {stderr}"
    );
    common::expect_front_end_ok_files_with_stdlib(&[source], "annotated type parameters");
}

#[test]
fn unresolved_declaration_type_parameter_annotation_matches_kotlinc() {
    let source = "class Box<@DefinitelyAbsentAnnotation T>";
    let result = common::compiler_diagnostics(&[("MissingAnnotation.kt", source)], &[]);
    let krusty_error =
        first_error(&result.krusty_stderr).or_else(|| first_error(&result.krusty_stdout));
    let kotlinc_error = first_error(&result.reference_stderr);

    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    assert_eq!(krusty_error, kotlinc_error);
}

#[test]
fn constructor_infers_nested_function_type() {
    let diagnostics = common::front_end_diagnostics(
        "class C<T>(val consume: ((T) -> Unit)?)\n\
         fun bad() { val f: (String) -> Unit = { }; val c = C(f); c.consume!!(1) }",
        &[],
        None,
    );
    assert_eq!(
        diagnostics,
        ["argument type mismatch: actual type is 'Int', but 'String' was expected."]
    );
}

#[test]
fn failed_property_inference_diagnostics_match_kotlinc_exactly() {
    let result = common::compiler_diagnostics(
        &[
            (
                "Blocks.kt",
                "package blocks\nval topBlock get() { return missingTopBlock() }\nclass C {\n    val memberBlock get() { return missingMemberBlock() }\n}\n",
            ),
            (
                "Cycle.kt",
                "package cycle\nval x get() = y\nval y get() = x\n",
            ),
            (
                "Expressions.kt",
                "package expressions\nval topExpression get() = missingTopExpression()\nval String.topExtension get() = missingTopExtension()\nclass C {\n    val memberExpression get() = missingMemberExpression()\n    val String.memberExtension get() = missingMemberExtension()\n}\n",
            ),
            (
                "Forward.kt",
                "package forward\nval eager = later\nval later = 1\n",
            ),
            (
                "Multiple.kt",
                "package multiple\nval eager = later + after\nval later = 1\nval after = 2\n",
            ),
        ],
        &[],
    );
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    let reference = errors(&result.reference_stderr);
    let recursive = RECURSIVE_INFERENCE_MESSAGE.to_string();
    let expected = vec![
        ObservedError {
            file: "Blocks.kt".to_string(),
            line: 2,
            column: 1,
            message: "this property must have an explicit type, be initialized, or be delegated."
                .to_string(),
        },
        ObservedError {
            file: "Blocks.kt".to_string(),
            line: 2,
            column: 29,
            message: "unresolved reference 'missingTopBlock'.".to_string(),
        },
        ObservedError {
            file: "Blocks.kt".to_string(),
            line: 4,
            column: 5,
            message: "this property must have an explicit type, be initialized, or be delegated."
                .to_string(),
        },
        ObservedError {
            file: "Blocks.kt".to_string(),
            line: 4,
            column: 36,
            message: "unresolved reference 'missingMemberBlock'.".to_string(),
        },
        ObservedError {
            file: "Cycle.kt".to_string(),
            line: 2,
            column: 15,
            message: recursive.clone(),
        },
        ObservedError {
            file: "Cycle.kt".to_string(),
            line: 3,
            column: 15,
            message: recursive,
        },
        ObservedError {
            file: "Expressions.kt".to_string(),
            line: 2,
            column: 27,
            message: "unresolved reference 'missingTopExpression'.".to_string(),
        },
        ObservedError {
            file: "Expressions.kt".to_string(),
            line: 3,
            column: 33,
            message: "unresolved reference 'missingTopExtension'.".to_string(),
        },
        ObservedError {
            file: "Expressions.kt".to_string(),
            line: 5,
            column: 34,
            message: "unresolved reference 'missingMemberExpression'.".to_string(),
        },
        ObservedError {
            file: "Expressions.kt".to_string(),
            line: 6,
            column: 40,
            message: "unresolved reference 'missingMemberExtension'.".to_string(),
        },
        ObservedError {
            file: "Forward.kt".to_string(),
            line: 2,
            column: 13,
            message: "variable 'later' must be initialized.".to_string(),
        },
        ObservedError {
            file: "Multiple.kt".to_string(),
            line: 2,
            column: 13,
            message: "variable 'later' must be initialized.".to_string(),
        },
        ObservedError {
            file: "Multiple.kt".to_string(),
            line: 2,
            column: 21,
            message: "variable 'after' must be initialized.".to_string(),
        },
    ];
    assert_eq!(krusty.len(), 13);
    assert_eq!(reference.len(), 13);
    assert_eq!(krusty, expected);
    assert_eq!(reference, expected);
}

#[test]
fn error_receiver_diagnostics_match_kotlinc_exactly() {
    let result = common::compiler_diagnostics(
        &[(
            "ErrorReceivers.kt",
            "fun declared(u: Missing) {\n    u.method()\n    println(u.name)\n}\n\
             fun failed() {\n    val value = missingFn()\n    value.method()\n}\n\
             fun argument(builder: StringBuilder) {\n    builder.append(missingFn())\n}\n",
        )],
        &[],
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![
        ObservedError {
            file: "ErrorReceivers.kt".to_string(),
            line: 1,
            column: 17,
            message: "unresolved reference 'Missing'.".to_string(),
        },
        ObservedError {
            file: "ErrorReceivers.kt".to_string(),
            line: 2,
            column: 7,
            message: "unresolved reference 'method'.".to_string(),
        },
        ObservedError {
            file: "ErrorReceivers.kt".to_string(),
            line: 3,
            column: 15,
            message: "unresolved reference 'name'.".to_string(),
        },
        ObservedError {
            file: "ErrorReceivers.kt".to_string(),
            line: 6,
            column: 17,
            message: "unresolved reference 'missingFn'.".to_string(),
        },
        ObservedError {
            file: "ErrorReceivers.kt".to_string(),
            line: 10,
            column: 20,
            message: "unresolved reference 'missingFn'.".to_string(),
        },
    ];
    assert_eq!(krusty_errors.len(), expected.len());
    assert_eq!(kotlinc_errors.len(), expected.len());
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn generic_method_result_binds_outer_type_parameter() {
    common::expect_front_end_ok_files_with_stdlib(
        &["class C { fun <T> get(value: Any): T = value as T }\n\
           fun <T> outer(c: C, value: Any): T = c.get(value)"],
        "generic method result binds outer type parameter",
    );
}

#[test]
fn dependent_function_bound_keeps_the_enclosing_formal_identity() {
    let source = "fun <T : CharSequence, U : T> keep(value: U): T = value\n\
                  fun box(): String = keep<String, String>(\"OK\")";
    let (code, stderr) = common::kotlinc_source_result("DependentFunctionBound", source);
    assert_eq!(code, 0, "kotlinc rejected dependent bound: {stderr}");
    common::expect_front_end_ok_files_with_stdlib(&[source], "dependent function bound");
}

#[test]
fn member_type_parameter_bound_keeps_the_class_formal_identity() {
    let source = "class Outer<T : CharSequence> {\n\
                      fun <U : T> keep(value: U): T = value\n\
                  }\n\
                  fun box(): String = Outer<String>().keep(\"OK\")";
    let (code, stderr) = common::kotlinc_source_result("MemberDependentBound", source);
    assert_eq!(code, 0, "kotlinc rejected dependent member bound: {stderr}");
    common::expect_front_end_ok_files_with_stdlib(&[source], "dependent member bound");
}

#[test]
fn where_constraint_subject_diagnostic_matches_kotlinc() {
    let source = "class C<T> where U : Any";
    let (code, stderr) = common::kotlinc_source_result("InvalidWhereSubject", source);
    assert_ne!(code, 0, "kotlinc unexpectedly accepted invalid constraint");
    assert_eq!(
        errors(&stderr)
            .into_iter()
            .map(|error| error.message)
            .collect::<Vec<_>>(),
        vec!["'U' does not refer to a type parameter of 'C'."]
    );
    assert_eq!(
        common::front_end_diagnostics(source, &[], None),
        ["'U' does not refer to a type parameter of 'C'."]
    );
}

#[test]
fn callable_reference_bound_failure_is_inference_error() {
    let diagnostics = common::front_end_diagnostics(
        "interface Bound\n\
         fun foo(x: Int, y: Char = 'K'): String = \"\"\n\
         fun <T : Bound, U> hold(f: (T) -> U): U = hold(f)\n\
         fun bad(): String = hold(::foo)",
        &[],
        None,
    );
    assert_eq!(
        diagnostics,
        ["cannot infer type for type parameter 'T'. Specify it explicitly."]
    );
}

#[test]
fn result_failure_type_argument_arity_matches_kotlinc_exactly() {
    let source = "fun box() { Result.failure<Int, String>(RuntimeException(\"x\")) }";
    let result = common::compiler_diagnostics(
        &[("ResultFailureArity.kt", source)],
        &[common::stdlib_jar()],
    );
    let mut krusty = errors(&result.krusty_stdout);
    krusty.extend(errors(&result.krusty_stderr));
    let reference = errors(&result.reference_stderr);
    assert_eq!(krusty, reference);
    assert_eq!(krusty.len(), 1);
}

#[test]
fn result_failure_unbound_type_parameter_matches_kotlinc_exactly() {
    let source = "fun box() { val result = Result.failure(RuntimeException(\"x\")) }";
    let result = common::compiler_diagnostics(
        &[("ResultFailureInference.kt", source)],
        &[common::stdlib_jar()],
    );
    let mut krusty = errors(&result.krusty_stdout);
    krusty.extend(errors(&result.krusty_stderr));
    let reference = errors(&result.reference_stderr);
    assert_eq!(krusty, reference);
    assert_eq!(krusty.len(), 1);
}

#[test]
fn conditional_inference_diagnostics_match_kotlinc_exactly() {
    let result = common::compiler_diagnostics(
        &[
            (
                "ElvisLeftBound.kt",
                "fun <T> id(value: T): T = value\n\
                 fun <T> nullable(): T? = null\n\
                 fun elvisCase() {\n\
                 val result: Int = id(nullable() ?: \"a\")\n\
                 }\n",
            ),
            (
                "IfBound.kt",
                "fun boundCase(n: Result<Int>?) {\n\
                 val result: String = if (true) Result.failure(RuntimeException(\"a\")) else n\n\
                 }\n",
            ),
            (
                "IfInvalidArgument.kt",
                "fun <T> invalidFrom(value: String): T = throw RuntimeException()\n\
                 fun ifInvalidArgument() {\n\
                 val result = if (true) invalidFrom(1) else 0\n\
                 }\n",
            ),
            (
                "IfNarrowed.kt",
                "fun <T> from(value: String): T = throw RuntimeException()\n\
                 fun ifNarrowed(text: String?) {\n\
                 val result: String = if (text != null) from(text) else 0\n\
                 }\n",
            ),
            (
                "IfUnbound.kt",
                "fun ifCase() {\n\
                 val result = if (true) Result.failure(RuntimeException(\"a\")) else Result.failure(RuntimeException(\"b\"))\n\
                 }\n",
            ),
            (
                "WhenInvalidArgument.kt",
                "fun <T> invalidFromWhen(value: String): T = throw RuntimeException()\n\
                 fun whenInvalidArgument() {\n\
                 val result = when {\n\
                 true -> invalidFromWhen(1)\n\
                 else -> 0\n\
                 }\n\
                 }\n",
            ),
            (
                "WhenNarrowed.kt",
                "fun <T> fromWhen(value: String): T = throw RuntimeException()\n\
                 fun whenNarrowed(text: String?) {\n\
                 val result: String = when {\n\
                 text == null -> 0\n\
                 else -> fromWhen(text)\n\
                 }\n\
                 }\n",
            ),
            (
                "WhenUnbound.kt",
                "fun whenCase() {\n\
                 val result = when {\n\
                 true -> Result.failure(RuntimeException(\"a\"))\n\
                 false -> Result.failure(RuntimeException(\"b\"))\n\
                 else -> Result.failure(RuntimeException(\"c\"))\n\
                 }\n\
                 }\n",
            ),
        ],
        &[common::stdlib_jar()],
    );
    let mut krusty = errors(&result.krusty_stdout);
    krusty.extend(errors(&result.krusty_stderr));
    let reference = errors(&result.reference_stderr);
    let cannot_infer =
        "cannot infer type for type parameter 'T'. Specify it explicitly.".to_string();
    let expected = vec![
        ObservedError {
            file: "ElvisLeftBound.kt".to_string(),
            line: 4,
            column: 17,
            message: "initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
        },
        ObservedError {
            file: "IfBound.kt".to_string(),
            line: 2,
            column: 20,
            message: "initializer type mismatch: expected 'String', actual 'Result<Int>?'."
                .to_string(),
        },
        ObservedError {
            file: "IfInvalidArgument.kt".to_string(),
            line: 3,
            column: 36,
            message: "argument type mismatch: actual type is 'Int', but 'String' was expected."
                .to_string(),
        },
        ObservedError {
            file: "IfNarrowed.kt".to_string(),
            line: 3,
            column: 20,
            message: "initializer type mismatch: expected 'String', actual 'Int'.".to_string(),
        },
        ObservedError {
            file: "IfUnbound.kt".to_string(),
            line: 2,
            column: 31,
            message: cannot_infer.clone(),
        },
        ObservedError {
            file: "IfUnbound.kt".to_string(),
            line: 2,
            column: 74,
            message: cannot_infer.clone(),
        },
        ObservedError {
            file: "WhenInvalidArgument.kt".to_string(),
            line: 4,
            column: 25,
            message: "argument type mismatch: actual type is 'Int', but 'String' was expected."
                .to_string(),
        },
        ObservedError {
            file: "WhenNarrowed.kt".to_string(),
            line: 3,
            column: 20,
            message: "initializer type mismatch: expected 'String', actual 'Int'.".to_string(),
        },
        ObservedError {
            file: "WhenUnbound.kt".to_string(),
            line: 3,
            column: 16,
            message: cannot_infer.clone(),
        },
        ObservedError {
            file: "WhenUnbound.kt".to_string(),
            line: 4,
            column: 17,
            message: cannot_infer.clone(),
        },
        ObservedError {
            file: "WhenUnbound.kt".to_string(),
            line: 5,
            column: 16,
            message: cannot_infer,
        },
    ];
    assert_eq!(krusty.len(), expected.len());
    assert_eq!(reference.len(), expected.len());
    assert_eq!(krusty, expected);
    assert_eq!(reference, expected);
}

#[test]
fn discarded_result_failure_unbound_type_parameter_matches_kotlinc_exactly() {
    let source = "fun box() { Result.failure(RuntimeException(\"x\")) }";
    let result = common::compiler_diagnostics(
        &[("DiscardedResultFailureInference.kt", source)],
        &[common::stdlib_jar()],
    );
    let mut krusty = errors(&result.krusty_stdout);
    krusty.extend(errors(&result.krusty_stderr));
    let reference = errors(&result.reference_stderr);
    assert_eq!(krusty, reference);
    assert_eq!(krusty.len(), 1);
}

#[test]
fn top_level_result_failure_unbound_type_parameter_matches_kotlinc_exactly() {
    let source = "val result = Result.failure(RuntimeException(\"x\"))";
    let result = common::compiler_diagnostics(
        &[("TopLevelResultFailureInference.kt", source)],
        &[common::stdlib_jar()],
    );
    let mut krusty = errors(&result.krusty_stdout);
    krusty.extend(errors(&result.krusty_stderr));
    let reference = errors(&result.reference_stderr);
    assert_eq!(krusty, reference);
    assert_eq!(krusty.len(), 1);
}

#[test]
fn member_result_failure_unbound_type_parameter_matches_kotlinc_exactly() {
    let source = "class C { val result = Result.failure(RuntimeException(\"x\")) }";
    let result = common::compiler_diagnostics(
        &[("MemberResultFailureInference.kt", source)],
        &[common::stdlib_jar()],
    );
    let mut krusty = errors(&result.krusty_stdout);
    krusty.extend(errors(&result.krusty_stderr));
    let reference = errors(&result.reference_stderr);
    assert_eq!(krusty, reference);
    assert_eq!(krusty.len(), 1);
}

#[test]
fn constructor_header_lambda_this_matches_kotlinc() {
    let source = r#"
enum class Choice(val callback: () -> Enum.Companion) {
    RETAIN({ this })
}

open class Base(val callback: () -> Base.Companion) {
    companion object
}

class Derived : Base({ this })
"#;
    let (kotlinc_code, kotlinc_stderr) = common::kotlinc_source_result("EnumEntryThis", source);
    let diagnostics = common::front_end_diagnostics(
        source,
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(
        kotlinc_code, 0,
        "kotlinc rejected enum this: {kotlinc_stderr}"
    );
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn unresolved_imports_match_kotlinc_exactly() {
    let source = "import java.util.Nonexistent\n\
                  import nonexistent.pkg.*\n\
                  fun f() = 1\n";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("ImportFailures.kt", source)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let mut kotlinc_errors = errors(&result.reference_stderr);
    krusty_errors.sort_by_key(|error| (error.line, error.column));
    kotlinc_errors.sort_by_key(|error| (error.line, error.column));
    let expected = vec![
        ObservedError {
            file: "ImportFailures.kt".to_string(),
            line: 1,
            column: 18,
            message: "unresolved reference 'Nonexistent'.".to_string(),
        },
        ObservedError {
            file: "ImportFailures.kt".to_string(),
            line: 2,
            column: 8,
            message: "unresolved reference 'nonexistent'.".to_string(),
        },
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn qualified_type_failure_messages_match_kotlinc() {
    let source = "class Outer\n\
                  fun a(x: deep.pkg.Missing): Int = 0\n\
                  fun b(x: kotlin.Missing?): Int = 0\n\
                  fun c(x: Outer.Nope): Int = 0\n";
    let result = common::compiler_diagnostics(&[("QualifiedTypes.kt", source)], &[]);
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );

    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let krusty_messages = krusty_errors
        .into_iter()
        .map(|error| error.message)
        .collect::<Vec<_>>();
    let kotlinc_messages = errors(&result.reference_stderr)
        .into_iter()
        .map(|error| error.message)
        .collect::<Vec<_>>();
    let expected = vec![
        "unresolved reference 'deep'.".to_string(),
        "unresolved reference 'Missing'.".to_string(),
        "unresolved reference 'Nope'.".to_string(),
    ];
    assert_eq!(krusty_messages, expected);
    assert_eq!(kotlinc_messages, expected);
}

#[test]
fn java_package_private_member_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "p/Api.java".to_string(),
            "package p; public final class Api {\n\
                 String instanceField = \"\";\n\
                 static String staticField = \"\";\n\
                 Api() {}\n\
                 void instanceMethod() {}\n\
                 static void staticMethod() {}\n\
             }"
            .to_string(),
        )],
        &[],
    ) else {
        eprintln!("skipping: JDK unavailable");
        return;
    };
    let source = "package q\n\
                  fun rejected(api: p.Api) {\n\
                      api.instanceField\n\
                      p.Api.staticField\n\
                      api.instanceMethod()\n\
                      p.Api.staticMethod()\n\
                      p.Api()\n\
                      api.instanceField = \"x\"\n\
                      p.Api.staticField = \"x\"\n\
                  }\n";
    let result = common::compiler_diagnostics(
        &[("PackagePrivateMembers.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }

    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let mut kotlinc_errors = errors(&result.reference_stderr);
    krusty_errors.sort_by_key(|error| (error.line, error.column));
    kotlinc_errors.sort_by_key(|error| (error.line, error.column));
    let expected = vec![
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 3,
            column: 5,
            message: "cannot access 'field instanceField: String!': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 4,
            column: 7,
            message: "cannot access 'static field staticField: String!': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 5,
            column: 5,
            message: "cannot access 'fun instanceMethod(): Unit': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 6,
            column: 7,
            message: "cannot access 'static fun staticMethod(): Unit': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 7,
            column: 3,
            message: "cannot access 'constructor(): Api': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 8,
            column: 5,
            message: "cannot access 'field instanceField: String!': it is package-private in 'p.Api'."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateMembers.kt".to_string(),
            line: 9,
            column: 7,
            message: "cannot access 'static field staticField: String!': it is package-private in 'p.Api'."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn protected_java_member_receiver_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/Parent.java".to_string(),
            "package fixtures; public class Parent { protected String value() { return \"hidden\"; } }"
                .to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\nimport fixtures.Parent\nclass Child : Parent() { fun read(parent: Parent): String = parent.value() }";
    let result = common::compiler_diagnostics(
        &[("ProtectedReceiver.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let expected = vec![ObservedError {
        file: "ProtectedReceiver.kt".to_string(),
        line: 3,
        column: 68,
        message: "cannot access 'fun value(): String!': it is protected in 'fixtures.Parent'."
            .to_string(),
    }];
    assert_eq!(krusty_errors, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}

#[test]
fn protected_inherited_java_constructor_diagnostic_matches_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/Parent.java".to_string(),
            "package fixtures; public class Parent { protected static class Category { protected Category() {} } }"
                .to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "import fixtures.Parent\n\
                  class Child : Parent() {\n\
                      fun value(): Any = Category()\n\
                  }";
    let result = common::compiler_diagnostics(
        &[("ProtectedNestedConstructor.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let expected = vec![ObservedError {
        file: "ProtectedNestedConstructor.kt".to_string(),
        line: 3,
        column: 20,
        message: "cannot access 'constructor(): Parent.Category': it is protected in 'fixtures.Parent.Category'."
            .to_string(),
    }];
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty_errors, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}

#[test]
fn protected_java_field_receiver_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/Parent.java".to_string(),
            "package fixtures; public class Parent { protected String field = \"\"; }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\n\
                  import fixtures.Parent\n\
                  class Child : Parent() {\n\
                      fun read(parent: Parent): String = parent.field\n\
                      fun write(parent: Parent) { parent.field = \"\" }\n\
                  }";
    let result = common::compiler_diagnostics(
        &[("ProtectedFieldReceiver.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![
        ObservedError {
            file: "ProtectedFieldReceiver.kt".to_string(),
            line: 4,
            column: 43,
            message: "cannot access 'field field: String!': it is protected in 'fixtures.Parent'."
                .to_string(),
        },
        ObservedError {
            file: "ProtectedFieldReceiver.kt".to_string(),
            line: 5,
            column: 36,
            message: "cannot access 'field field: String!': it is protected in 'fixtures.Parent'."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors.len(), 2);
    assert_eq!(kotlinc_errors.len(), 2);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn java_field_write_rejection_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/Parent.java".to_string(),
            "package fixtures; public class Parent { public final String finalField = \"\"; protected String protectedField = \"\"; }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\n\
                  import fixtures.Parent\n\
                  fun finalWrite(parent: Parent) { parent.finalField = \"\" }\n\
                  fun protectedWrite(parent: Parent) { parent.protectedField = \"\" }";
    let result = common::compiler_diagnostics(
        &[("JavaFieldWriteRejections.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let expected = vec![
        ObservedError {
            file: "JavaFieldWriteRejections.kt".to_string(),
            line: 3,
            column: 41,
            message: "'val' cannot be reassigned.".to_string(),
        },
        ObservedError {
            file: "JavaFieldWriteRejections.kt".to_string(),
            line: 4,
            column: 45,
            message: "cannot access 'field protectedField: String!': it is protected in 'fixtures.Parent'."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors.len(), 2);
    assert_eq!(kotlinc_errors.len(), 2);
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn package_private_java_classifier_constructor_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "javafixture/PackageBox.java".to_string(),
            "package javafixture; class PackageBox { PackageBox(int value) {} }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\n\
                  import javafixture.PackageBox\n\
                  fun use(): Int { PackageBox(1); return 0 }\n";
    let result = common::compiler_diagnostics(
        &[("PackagePrivateClassifier.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let mut kotlinc_errors = errors(&result.reference_stderr);
    krusty_errors.sort_by_key(|error| (error.line, error.column));
    kotlinc_errors.sort_by_key(|error| (error.line, error.column));
    let expected = vec![
        ObservedError {
            file: "PackagePrivateClassifier.kt".to_string(),
            line: 2,
            column: 20,
            message: "cannot access 'class PackageBox : Any': it is package-private in file."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivateClassifier.kt".to_string(),
            line: 3,
            column: 18,
            message: "cannot access 'constructor(p0: Int): PackageBox': it is package-private in 'javafixture.PackageBox'."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn package_private_java_static_field_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "p/Pub.java".to_string(),
            "package p; public class Pub { static int count = 7; }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let result = common::compiler_diagnostics(
        &[
            (
                "ClassifierImportStaticField.kt",
                "package q\nimport p.Pub\nfun classifierImport(): Int = Pub.count\n",
            ),
            (
                "QualifiedStaticField.kt",
                "package q\nfun qualified(): Int = p.Pub.count\n",
            ),
        ],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    let message =
        "cannot access 'static field count: Int': it is package-private in 'p.Pub'.".to_string();
    let expected = vec![
        ObservedError {
            file: "ClassifierImportStaticField.kt".to_string(),
            line: 3,
            column: 35,
            message: message.clone(),
        },
        ObservedError {
            file: "QualifiedStaticField.kt".to_string(),
            line: 2,
            column: 30,
            message,
        },
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
    assert_eq!(krusty_errors.len(), 2);
    assert_eq!(kotlinc_errors.len(), 2);
}

#[test]
fn package_private_java_classifier_public_constructor_diagnostics_match_kotlinc() {
    let Some((java_dir, _)) = common::javac_compile(
        &[(
            "fixtures/PackageType.java".to_string(),
            "package fixtures; class PackageType { public PackageType() {} }".to_string(),
        )],
        &[],
    ) else {
        return;
    };
    let source = "package consumer\n\
                  import fixtures.PackageType\n\
                  fun use(): Any = PackageType()\n";
    let result = common::compiler_diagnostics(
        &[("PackagePrivatePublicConstructor.kt", source)],
        std::slice::from_ref(&java_dir),
    );
    if let Some(root) = java_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let mut kotlinc_errors = errors(&result.reference_stderr);
    krusty_errors.sort_by_key(|error| (error.line, error.column));
    kotlinc_errors.sort_by_key(|error| (error.line, error.column));
    let expected = vec![
        ObservedError {
            file: "PackagePrivatePublicConstructor.kt".to_string(),
            line: 2,
            column: 17,
            message: "cannot access 'class PackageType : Any': it is package-private in file."
                .to_string(),
        },
        ObservedError {
            file: "PackagePrivatePublicConstructor.kt".to_string(),
            line: 3,
            column: 18,
            message: "cannot access 'class PackageType : Any': it is package-private in file."
                .to_string(),
        },
    ];
    assert_eq!(krusty_errors, expected);
    assert_eq!(kotlinc_errors, expected);
}

#[test]
fn shared_diagnostic_wording_matches_kotlinc() {
    let source = "fun breakOutside() { break }\n\
                  fun continueOutside() { continue }\n\
                  fun varargs(vararg first: Int, vararg second: Int) {}\n\
                  open class Base\n\
                  class Derived : Base() { override fun absent() {} }\n\
                  val topLevelThis: Any = this\n\
                  class ConstructorThis(val value: Any = this)";
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("SharedDiagnostics.kt", source)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let mut kotlinc_errors = errors(&result.reference_stderr);
    krusty_errors.sort_by_key(|error| (error.line, error.column));
    kotlinc_errors.sort_by_key(|error| (error.line, error.column));
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        [
            "'break' and 'continue' are only allowed inside loops.",
            "'break' and 'continue' are only allowed inside loops.",
            "multiple vararg parameters are prohibited.",
            "multiple vararg parameters are prohibited.",
            "'absent' overrides nothing.",
            "'this' is not defined in this context.",
            "cannot access '<this>' before the instance has been initialized.",
        ]
    );
}

#[test]
fn prohibited_script_returns_match_kotlinc() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    for (filename, source) in [
        ("ReturnStatement.kts", "return"),
        ("ReturnExpression.kts", "null ?: return"),
    ] {
        let input = krusty::frontend::SourceInput::kotlin_script(source);
        let krusty_diagnostics = common::front_end_diagnostics_inputs(
            &[input],
            std::slice::from_ref(&stdlib),
            Some(jdk.as_path()),
        );
        let (reference_code, reference_stderr) =
            common::kotlinc_named_source_result(filename, source);
        assert_ne!(
            reference_code, 0,
            "kotlinc unexpectedly accepted {source:?}"
        );
        let reference_error = first_error(&reference_stderr)
            .unwrap_or_else(|| panic!("kotlinc emitted no location diagnostic for {source:?}"));
        assert_eq!(
            krusty_diagnostics,
            [reference_error.message],
            "script source: {source:?}"
        );
        assert_eq!(
            krusty_diagnostics,
            ["'return' is prohibited here."],
            "script source: {source:?}"
        );
    }
}

#[test]
fn errors_match_kotlinc_in_text_and_location() {
    let stdlib = common::stdlib_jar();

    // Snippets within krusty's subset that produce a diagnostic kotlinc also produces identically.
    let cases = [
        "fun f(): Int = q",
        // A missing callee is the ordinary UNRESOLVED_REFERENCE diagnostic, not a distinct
        // "unresolved function" error. Cover both function- and constructor-shaped calls.
        "fun use(): Int { noSuchFunction(); return 0 }",
        "fun use(): Any = NoSuchClass(1)",
        // A qualified expression commits to its lexical root before resolving later segments.
        // `java` is the local Int here, not the lower-priority JDK package, so both compilers must
        // diagnose `io` and must not backtrack to `java.io.File`.
        "fun f() { val java = 1; val file = java.io.File(\"A\") }",
        // The same rule applies to a top-level property root: package lookup is considered only when
        // the expression/value scope has no winning declaration named `java`.
        "val java = 1\nfun f() { val file = java.io.File(\"A\") }",
        "class Producer<out T>\nfun bad(value: Producer<in String>) = value",
        "fun f(a: Int): String = a",
        "class Box<T>\nfun <T> bad(x: Box<String>): Box<T> = x",
        "fun f(): String = null",
        "val x: String = null",
        "fun f(x: String): String = x\nfun g(): String = f(null)",
        "fun f() { var x: String = \"\"; x = null }",
        "fun f(): String { return 1 }",
        "fun f(): Int { val x = 1; x = 2; return x }",
        "fun f(x: Int): String { val y: String = x; return y }",
        "val x: String = 1",
        "fun f() { var x: String = \"\"; x = 1 }",
        "fun f(x: Int): Int = x\nfun g(): Int = f()",
        "fun <T> f(x: T): T = x\nfun g(): Int = f<Int>()",
        "fun f(x: Int): Int = x\nfun g(): Int = f ()",
        "fun f(x: Int): Int = x\nfun g(): Int = f(1, 2)",
        "fun f(x: Int): Int = x\nfun g(): Int = f(\"no\")",
        "fun f(): Array<String> = arrayOf()",
        "fun <T> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "suspend fun <T> f(x: T): T = x\nsuspend fun g(): Int = f(1, 2)",
        "inline fun <reified T> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "fun <T : Any> f(x: T): T = x\nfun g(): Int = f(1, 2)",
        "fun <T> f(x: T & Any): T & Any = x\nfun g() { f(null) }",
        "class C<T>(val x: T & Any)\nfun g() { C(null) }",
        "class C<T>(val x: T & Any)\nfun g() { C(x = null) }",
        "class C<T : CharSequence?>(val x: T)\nfun g() { C(null).x.length }",
        "val String.first: Char get() = 'x'\nfun bad(s: String?): Char = s.first",
        "class PairBox<T>(val a: T, val b: T)\nclass Host { val String.pair get() = PairBox(\"x\", null); fun bad(): Int = \"\".pair.b.length }",
        "class C<T : Number>(val a: T, val b: T)\nfun f() = C(1, \"bad\")",
        "class C<T>(val consume: (T) -> Unit)\nfun bad() { val consumeString: (String) -> Unit = { }; val c = C(consumeString); c.consume(1) }",
        "class C<T>(val consume: ((T) -> Unit)?)\nfun bad() { val consumeString: (String) -> Unit = { }; val c = C(consumeString); c.consume!!(1) }",
        "fun <T> inferred(x: T) = x\nfun g(): Int = inferred(1, 2)",
        "fun <T> f(x: T, y: Int = 1): T = x\nfun g(): Int = f(1, 2, 3)",
        "fun f(x: Int): Int = x\nfun f(x: String): Int = 0\nfun g(): Int = f(1, 2)",
        "fun f(x: Int = 1): Int = x\nfun g(): Int = f(1, 2)",
        "fun f(a: Int = 0, b: String): String = b\nfun g(): String = f(a = 1)",
        "fun f(`a`: Int = 0, b: String): String = b\nfun g(): String = f(`a` = 1)",
        "fun f(a: Int, b: String): String = b\nfun g(): String = f(a = 1, c = 2, b = \"ok\")",
        "fun f(a: Int, b: String): String = b\nfun g(): String = f(a = 1, a = 2, b = \"ok\")",
        "fun f(a: Int = 0, b: String, vararg x: Int): Int = 0\nfun g(): Int = f()",
        "fun g(): Int {\nfun f(x: Int): Int = x\nreturn f()\n}",
        "fun g(): Int {\nfun f(x: Int): Int = x\nreturn f(1, 2)\n}",
        "fun g(): Int {\nfun choose(a: Int): Int = a\nfun choose(a: String, b: String, c: String): Int = 0\nreturn choose(1, 2)\n}",
        "class C { fun f(x: Int): Int = x }\nfun g(c: C): Int = c.f()",
        "class C { fun f(x: Int): Int = x }\nfun g(c: C): Int = c.f(1, 2)",
        "class C { fun f(x: Int = 1): Int = x }\nfun g(c: C): Int = c.f(1, 2)",
        "class C { fun <T> choose(a: T): T = a; fun <T> choose(a: T, b: T): T = a }\nfun g(c: C): Int = c.choose(1, 2, 3)",
        "class C { fun choose(a: Int): Int = a; fun choose(a: String, b: String, c: String): Int = 0 }\nfun g(c: C): Int = c.choose(1, 2)",
        "open class Base { fun <T> choose(a: T): T = a }\nclass Child : Base()\nfun g(c: Child): Int = c.choose(1, 2)",
        "class C(val x: Int)\nfun g(): C = C()",
        "class C(val x: Int)\nfun g(): C = C(1, 2)",
        "class C(val x: Int = 1)\nfun g(): C = C(1, 2)",
        "class C(val a: Int) { constructor(a: String, b: String, c: String) : this(0) }\nfun g(): C = C(1, 2)",
        "fun f(vararg x: Int): Int = x.size\nfun g(): Int = f(\"no\")",
        "fun f(x: Int = \"no\"): Int = x",
        "fun f(x: String): Int = x.missing",
        "fun f(x: String): Int = x.`missing name`",
        "fun f(x: String?): Int? = x?.`missing name`",
        "fun f(x: String): Int = x.missing()",
        "class C { fun member(value: Int): Int = value }\nfun f(value: C?): Int = value.member(1)",
        "fun f(value: String?): String = value.substring(1)",
        "fun f(value: String?): String = value. /* gap */ substring(1)",
        "fun String.nonNullExtension(): Int = length\nfun f(value: String?): Int = value.nonNullExtension()",
        "class C(val block: () -> Int)\nfun f(value: C?): Int = value.block()",
        "fun <T : Any> T.nonNullGeneric(): Int = 1\nfun f(value: String?): Int = value.nonNullGeneric()",
        "class GenericHolder<T> { fun read(): Int = 1 }\nfun f(value: GenericHolder<String>?): Int = value.read()",
        "fun f(block: (() -> Int)?): Int = block.invoke()",
        "fun f(value: Any?): Boolean = value.equals(null)",
        "fun f(x: String): String = x.substring(\"no\")",
        "fun f(x: Int): Int = x.substring(1)",
        "fun f(): Int { if (1) return 1; return 0 }",
        "fun f(): Int = when { 1 -> 1; else -> 0 }",
        "class C\ncontext(c: C) fun f(x: Int): Int = x\nfun g(c: C): Int = with(c) { f() }",
        "class C\ncontext(c: C) fun f(x: Int): Int = x\nfun g(c: C): Int = with(c) { f(1, 2) }",
        "class C { fun unaryMinus(): C = this }\nfun g(): C = -C()",
        "class C { fun inc(): C = this }\nfun g(c: C) { var value = c; value++ }",
        "class C { fun plus(other: C): C = this }\nfun g(left: C, right: C): C = left + right",
        "class Parser\nfun Parser.decode(source: String): String = source\nfun Parser.decode(value: Int): String = value.toString()\nfun bad() { val parser = Parser(); val reference = parser::decode }",
        "fun cross(x: String, y: Any): String = \"A\"\nfun cross(x: CharSequence, y: CharSequence): String = \"B\"\nfun bad() { val reference: (String, String) -> String = ::cross }",
        "fun <T> applySame(block: (T) -> T, value: T): T = block(value)\nfun mismatched(x: String, suffix: Char = 'K'): Int = x.length + suffix.code\nfun bad(): Any = applySame(::mismatched, \"O\")",
        "fun foo(x: String, y: Char = 'K'): String = x + y\nfun <T, U> hold(f: (T) -> U): U = hold(f)\nfun bad(): String = hold<Int, String>(::foo)",
        "fun foo(x: Int, y: Char = 'K'): String = x.toString() + y\nfun <T : CharSequence, U> hold(f: (T) -> U): U = hold(f)\nfun bad(): String = hold(::foo)",
        // A deliberately unique type present on NO supplied classpath. Keep the spelling synthetic:
        // diagnostic regressions must not depend on or disclose a class from the scanned project.
        "fun f(p: DefinitelyAbsentClassifier): Int = 0",
        // Declaration bounds use the same normal classifier resolution and must fail before the
        // metadata encoder. This source previously reached emission and panicked there.
        "abstract class C<P>(val p: P) where P : DefinitelyAbsentBoundA, P : DefinitelyAbsentBoundB",
        // A cyclic source hierarchy reports the frontend error at the same source coordinate rather
        // than reaching an unguarded inheritance walker.
        "object DefinitelyCyclicClassifier : DefinitelyCyclicClassifier()",
        // The diagnostic belongs to the edge that participates in the cycle, not an innocent
        // earlier supertype in the same declaration.
        "interface MixedCycle : InnocentSupertype, CyclicPeer\ninterface InnocentSupertype\ninterface CyclicPeer : MixedCycle",
        // An `is` whose TARGET type is unresolved reports the unresolved reference at the type's
        // span — never a compiler-specific "not supported" rejection.
        "fun f(p: Any) = p is DefinitelyAbsentClassifier",
        // … and a failing type ARGUMENT is named at its own span, not the outer generic's.
        "fun f(p: Any) = p is Array<DefinitelyAbsentClassifier>",
        // Ordinary generic arguments retain an outer `Ty::Obj`; nested Error detection must inspect
        // that semantic shape instead of relying on `outer == Ty::Error`.
        "fun f(p: Any) = p is List<DefinitelyAbsentClassifier>",
        // When the OUTER name is the unresolvable one it is named first, not its type argument.
        "fun f(p: Any) = p is DefinitelyAbsentClassifier<String>",
        // A nullable unresolved target resolves to the same reference diagnostic.
        "fun f(p: Any) = p is DefinitelyAbsentClassifier?",
        // The `as` sibling reports identically.
        "fun f(p: Any) = p as DefinitelyAbsentClassifier",
        "fun f(p: Any) = p as List<DefinitelyAbsentClassifier>",
        "fun f(p: Any) = p is (DefinitelyAbsentClassifier) -> String",
        "fun f(p: Any) = p is Function1<DefinitelyAbsentClassifier, String>",
        "fun f(p: Any) = p is Function1<Any?, Any?>",
        "fun f(p: Any) = p is Array",
        "fun f(p: Any) = p is Array<Nothing>",
        "fun f(p: Any) = p as Array",
        "fun f(p: Any) = p as Array<Nothing>",
        // Two unrelated final classifiers have no possible runtime overlap, so the cast is an
        // error rather than bytecode that can only fail. Keep this in exact kotlinc parity coverage.
        "fun box(): String { val s = 1 as String; return s }",
        // Casts permit an erased function shape, so an unresolved parameter remains the primary
        // diagnostic. (`is` is different and is pinned by the unsupported-shape test below.)
        "fun f(p: Any) = p as (DefinitelyAbsentClassifier) -> String",
    ];

    // Each comparison launches both compiler CLIs with the complete stdlib classpath. CPU count is
    // therefore not a safe concurrency budget: ten simultaneous classpath indexes can be killed by
    // the host before a diagnostic is produced, turning a parity test into a resource-race test.
    // Keep this exhaustive comparison serial; the surrounding test binary remains free to schedule
    // independent tests normally.
    let workers = 1;
    let chunk_size = cases.len().div_ceil(workers);
    let mismatches = std::thread::scope(|scope| {
        cases
            .chunks(chunk_size)
            .enumerate()
            .map(|(chunk, sources)| {
                let stdlib = &stdlib;
                scope.spawn(move || {
                    let mut mismatches = Vec::new();
                    for (offset, src) in sources.iter().enumerate() {
                        let i = chunk * chunk_size + offset;
                        let file = format!("t{i}.kt");
                        let result = common::compiler_diagnostics(
                            &[(file.as_str(), src)],
                            std::slice::from_ref(stdlib),
                        );
                        let kr_error = first_error(&result.krusty_stderr)
                            .or_else(|| first_error(&result.krusty_stdout));
                        let kc_error = first_error(&result.reference_stderr);
                        if (result.krusty_code == 0) != (result.reference_code == 0)
                            || kr_error != kc_error
                        {
                            mismatches.push((
                                i,
                                format!(
                                    "diagnostic mismatch for {src:?}\n krusty ({code}): {kr_error:?}\n kotlinc ({reference_code}): {kc_error:?}",
                                    code = result.krusty_code,
                                    reference_code = result.reference_code,
                                ),
                            ));
                        }
                    }
                    mismatches
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut mismatches = mismatches.into_iter().flatten().collect::<Vec<_>>();
    mismatches.sort_by_key(|(index, _)| *index);
    assert!(
        mismatches.is_empty(),
        "{}",
        mismatches
            .into_iter()
            .map(|(_, mismatch)| mismatch)
            .collect::<Vec<_>>()
            .join("\n\n")
    );
}

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
