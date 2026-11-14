//! kotlinc's `CYCLIC_GENERIC_UPPER_BOUND`: type parameters whose bounds name one another. A
//! classifier reports each bound that closes a cycle; a callable reports each type parameter on
//! one. A bound that only mentions a parameter (`Comparable<X>`) is a legal recursive bound.

use super::common;

/// Compile `source` with both compilers and require the same complete errors and no warnings,
/// which must also be `expected` (`line:column: message`).
fn assert_matches_kotlinc(source: &str, expected: &[&str]) {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Cycle.kt", source)],
        &[common::stdlib_jar()],
        &[],
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty_errors = common::compiler_errors(&krusty_output);
    let kotlinc_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    assert_eq!(
        common::compiler_warnings(&krusty_output),
        common::compiler_warnings(&result.reference_stderr),
        "warnings\n{krusty_output}"
    );
    let rendered = kotlinc_errors
        .iter()
        .map(|error| format!("{}:{}: {}", error.line, error.column, error.message))
        .collect::<Vec<_>>();
    assert_eq!(rendered, expected);
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

fn cyclic(position: &str, names: &str) -> String {
    format!("{position}: type parameter has cyclic upper bounds: {names}.")
}

#[test]
fn classifier_bounds_report_each_bound_closing_a_cycle() {
    let source = "\
class A<B : B>
class C<X : Y, Y : X>
class D<P : Q, Q : R, R : P>
class Where<M, N> where M : N, N : M
class Mixed<P : Q, Q : P, R : P>
class Reversed<Y : X, X : Y>
class Nullable<T : U, U : T?>
interface I<T : T>
class Legal1<X : Comparable<X>>
class Legal2<U : List<U>>
class Legal3<T : U, U : Comparable<T>>
";
    let expected = [
        cyclic("1:13", "B"),
        cyclic("2:13", "X, Y"),
        cyclic("2:20", "X, Y"),
        cyclic("3:13", "P, Q, R"),
        cyclic("3:20", "P, Q, R"),
        cyclic("3:27", "P, Q, R"),
        cyclic("4:29", "M, N"),
        cyclic("4:36", "M, N"),
        cyclic("5:17", "P, Q"),
        cyclic("5:24", "P, Q"),
        cyclic("6:20", "Y, X"),
        cyclic("6:27", "Y, X"),
        cyclic("7:20", "T, U"),
        cyclic("7:27", "T, U"),
        cyclic("8:17", "T"),
    ];
    let expected = expected.iter().map(String::as_str).collect::<Vec<_>>();
    assert_matches_kotlinc(source, &expected);
}

#[test]
fn callable_bounds_report_each_type_parameter_on_a_cycle() {
    let source = "\
fun <T : T> f(t: T) {}
fun <S : S?> g(s: S) {}
fun <A, B> w1() where A : B, B : A {}
fun <A> w2() where A : A {}
fun <T : U, U : T> h() {}
val <V : V> V.p: Int get() = 1
fun <X : Comparable<X>> legal(x: X) {}
";
    let expected = [
        cyclic("1:6", "T"),
        cyclic("2:6", "S"),
        cyclic("3:6", "A, B"),
        cyclic("3:9", "A, B"),
        cyclic("4:6", "A"),
        cyclic("5:6", "T, U"),
        cyclic("5:13", "T, U"),
        cyclic("6:6", "V"),
    ];
    let expected = expected.iter().map(String::as_str).collect::<Vec<_>>();
    assert_matches_kotlinc(source, &expected);
}

#[test]
fn local_declarations_report_the_same_cycles() {
    let source = "\
fun main() {
    class Loc<B : B>
    class Loc2<X : Y, Y : X>
    fun <T : U, U : T> inner(t: T) {}
    val a: Loc<Int>? = null
    println(listOf(a, null))
}
";
    let expected = [
        cyclic("2:19", "B"),
        cyclic("3:20", "X, Y"),
        cyclic("3:27", "X, Y"),
        cyclic("4:10", "T, U"),
        cyclic("4:17", "T, U"),
    ];
    let expected = expected.iter().map(String::as_str).collect::<Vec<_>>();
    assert_matches_kotlinc(source, &expected);
}

#[test]
fn a_cyclic_bound_does_not_loop_when_types_are_joined() {
    let source = "\
class Bar<B : B>
class Pair2<X : Y, Y : X>
fun join(a: Bar<Int>, b: Bar<String>) = listOf(a, b)
fun joinPairs(a: Pair2<Int, Int>, b: Pair2<String, String>) = listOf(a, b)
fun <T : T> joinCallable(t: T, s: String) = listOf(t, s)
";
    let expected = [
        cyclic("1:15", "B"),
        cyclic("2:17", "X, Y"),
        cyclic("2:24", "X, Y"),
        cyclic("5:6", "T"),
    ];
    let expected = expected.iter().map(String::as_str).collect::<Vec<_>>();
    assert_matches_kotlinc(source, &expected);
}
