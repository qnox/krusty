//! kotlinc's `WRONG_NUMBER_OF_TYPE_ARGUMENTS` for a classifier or typealias written in a type
//! position with too few or too many type arguments: in a parameter, property and nested argument
//! type, and in a supertype list, where a superclass constructor call does not infer them
//! (`class C : Alias(0)`). The report names the declaration and sits on the classifier's name when
//! nothing is written, otherwise on the written `<…>` list.

use super::common;

/// Compile `source` with both compilers and require the same complete errors, which must also be
/// `errors`, rendered as `line:column: message`.
fn assert_errors_match_kotlinc(source: &str, errors: &[&str]) {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Arity.kt", source)],
        &[common::stdlib_jar()],
        &[],
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty_errors = common::compiler_errors(&krusty_output);
    let kotlinc_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    let rendered = kotlinc_errors
        .iter()
        .map(|error| format!("{}:{}: {}", error.line, error.column, error.message))
        .collect::<Vec<_>>();
    assert_eq!(rendered, errors);
    assert_ne!(result.reference_code, 0);
    assert_ne!(result.krusty_code, 0);
}

#[test]
fn a_classifier_needs_its_own_type_arguments_in_a_declared_type() {
    assert_errors_match_kotlinc(
        "\
class Box<T>(val t: T)
fun f(b: Box) {}
val l: List<Box>? = null
fun h(b: Box<Int, Int>) {}
class Pair<A, B>
fun p(x: Pair<String>?) = x
",
        &[
            "2:10: one type argument expected for 'class Box<T> : Any'.",
            "3:13: one type argument expected for 'class Box<T> : Any'.",
            "4:13: one type argument expected for 'class Box<T> : Any'.",
            "6:14: 2 type arguments expected for 'class Pair<A, B> : Any'.",
        ],
    );
}

#[test]
fn a_supertype_needs_its_type_arguments_even_with_a_constructor_call() {
    assert_errors_match_kotlinc(
        "\
open class Ref<T>(var x: T)
interface I<T>
class A : I
class B : Ref(0)
class C : Ref<Int, Int>(0)
",
        &[
            "3:11: one type argument expected for 'interface I<T> : Any'.",
            "4:11: one type argument expected for 'class Ref<T> : Any'.",
            "5:14: one type argument expected for 'class Ref<T> : Any'.",
        ],
    );
}

#[test]
fn a_typealias_needs_its_own_type_arguments() {
    assert_errors_match_kotlinc(
        "\
open class Ref<T>(var x: T)
typealias R<T> = Ref<T>
interface I<T>
typealias J<T> = I<T>
fun f(x: R) {}
fun g(x: R<Int, Int>) {}
fun h() { val y: R? = null; val z: List<R<Int, Int>>? = null }
class Test1 : R(0)
class Test2 : R<Int>(0)
class Test3 : J
",
        &[
            "5:10: one type argument expected for 'typealias R<T> = Ref<T>'.",
            "6:11: one type argument expected for 'typealias R<T> = Ref<T>'.",
            "7:18: one type argument expected for 'typealias R<T> = Ref<T>'.",
            "7:42: one type argument expected for 'typealias R<T> = Ref<T>'.",
            "8:15: one type argument expected for 'typealias R<T> = Ref<T>'.",
            "10:15: one type argument expected for 'typealias J<T> = I<T>'.",
        ],
    );
}

#[test]
fn a_local_declaration_type_needs_its_type_arguments() {
    assert_errors_match_kotlinc(
        "\
class Box<T>(val t: T)
interface I<T>
fun f() {
    val b: Box? = null
    val c: Map<String, Box<Int, Int>>? = null
    class L : I
}
",
        &[
            "4:12: one type argument expected for 'class Box<T> : Any'.",
            "5:27: one type argument expected for 'class Box<T> : Any'.",
            "6:15: one type argument expected for 'interface I<T> : Any'.",
        ],
    );
}

#[test]
fn a_type_test_or_cast_target_checks_the_arguments_it_writes() {
    assert_errors_match_kotlinc(
        "\
class Box<T>
fun cast(x: Any) = x as Box<Int, Int>
fun test(x: Any) = x is Box<Int, Int>
fun nested(x: Any) = x is List<Box>
fun safe(x: Any) = x as? Box<String, String>
fun narrowed(x: Box<String>) = x is Box
",
        &[
            "2:28: one type argument expected for 'class Box<T> : Any'.",
            "3:28: one type argument expected for 'class Box<T> : Any'.",
            "4:32: one type argument expected for 'class Box<T> : Any'.",
            "5:29: one type argument expected for 'class Box<T> : Any'.",
        ],
    );
}

/// A declaration whose signature cannot finalize sends the module to diagnostic recovery, which
/// checks every finalized declaration again. Their typealias applications are still applications
/// of the alias, not bare applications of the generic classifier it expands to.
#[test]
fn an_alias_application_keeps_its_alias_when_another_signature_fails() {
    assert_errors_match_kotlinc(
        "\
open class Holder<T>(val t: T)
typealias Names = List<String>
typealias StringHolder = Holder<String>
class Named : StringHolder(\"x\")
fun first(names: Names): String = names[0]
fun Names.second(other: Names) = this + other
fun broken() = undefined
",
        &["7:16: unresolved reference 'undefined'."],
    );
}
