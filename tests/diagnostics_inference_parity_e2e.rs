//! Exact frontend and inference diagnostic parity with kotlinc.

use super::common;
use super::diagnostics_parity_support::{
    errors, first_error, ObservedError, RECURSIVE_INFERENCE_MESSAGE,
};

#[test]
fn generic_cast_is_accepted_by_both_frontends() {
    let source = "fun <T> materialize(): T = 42 as T";
    let (code, stderr) = common::kotlinc_source_result("GenericCast", source);
    assert_eq!(code, 0, "kotlinc rejected generic cast: {stderr}");
    common::expect_front_end_ok_files_with_stdlib(&[source], "generic cast parity");
}

#[test]
fn inaccessible_extension_precedes_incompatible_callable_reference_shape() {
    let declaration = "package app\n\
                       \n\
                       class Record\n\
                       \n\
                       private fun Record.label(value: Int): String = value.toString()\n";
    let use_site = "package app\n\
                    \n\
                    fun expose(record: Record): (String) -> String = record::label\n";
    let result = common::compiler_diagnostics(
        &[("Decl.kt", declaration), ("Use.kt", use_site)],
        &[common::stdlib_jar()],
    );
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));

    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(
        krusty,
        vec![ObservedError {
            file: "Use.kt".to_string(),
            line: 3,
            column: 58,
            message: "cannot access 'label': it is private in its file".to_string(),
        }]
    );
    assert_eq!(
        errors(&result.reference_stderr),
        vec![ObservedError {
            file: "Use.kt".to_string(),
            line: 3,
            column: 58,
            message: "cannot access 'fun Record.label(value: Int): String': it is private in file."
                .to_string(),
        }]
    );
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
