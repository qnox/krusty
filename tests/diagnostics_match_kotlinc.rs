//! krusty's diagnostics should match kotlinc's message and source location. For a set of erroneous
//! snippets, compile with both and assert the first error's file, line, column, and text match exactly.

use std::path::Path;

use super::common;

#[derive(Debug, PartialEq, Eq)]
struct ObservedError {
    file: String,
    line: usize,
    column: usize,
    message: String,
}

/// Extract the first `path:line:column: error: message` record. Compare the basename because both
/// compilers receive the same path, while their renderers may independently canonicalize its prefix.
fn first_error(output: &str) -> Option<ObservedError> {
    output.lines().find_map(|rendered| {
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
}

#[test]
fn generic_cast_is_accepted_by_both_frontends() {
    let source = "fun <T> materialize(): T = 42 as T";
    let (code, stderr) = common::kotlinc_source_result("GenericCast", source);
    assert_eq!(code, 0, "kotlinc rejected generic cast: {stderr}");
    common::expect_front_end_ok_files_with_stdlib(&[source], "generic cast parity");
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
    assert!(
        stderr.contains("'U' does not refer to a type parameter of 'C'."),
        "unexpected kotlinc diagnostic: {stderr}"
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
    assert!(diagnostics.is_empty(), "krusty: {diagnostics:?}");
}

#[test]
fn errors_match_kotlinc_in_text_and_location() {
    let stdlib = common::stdlib_jar();

    // Snippets within krusty's subset that produce a diagnostic kotlinc also produces identically.
    let cases = [
        "fun f(): Int = q",
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

    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(10)
        .min(cases.len());
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
    assert!(
        result.reference_stderr.contains("argument type mismatch"),
        "unexpected kotlinc diagnostic: {}",
        result.reference_stderr
    );

    let krusty_error =
        first_error(&result.krusty_stderr).or_else(|| first_error(&result.krusty_stdout));
    assert_ne!(result.krusty_code, 0, "krusty unexpectedly accepted @Exact");
    assert_eq!(krusty_error, first_error(&result.reference_stderr));
}

#[test]
fn kotlin_internal_exact_can_require_two_arguments_to_have_the_same_type() {
    let source = concat!(
        "@Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n",
        "fun <T> same(first: @kotlin.internal.Exact T, second: @kotlin.internal.Exact T) {}\n",
        "fun use(first: String, second: CharSequence) = same(first, second)",
    );
    let (code, stderr) = common::kotlinc_source_result("ExactPair", source);
    assert_ne!(code, 0, "two @Exact parameters accepted different types");
    assert!(
        stderr.contains("argument type mismatch"),
        "unexpected kotlinc diagnostic: {stderr}"
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
