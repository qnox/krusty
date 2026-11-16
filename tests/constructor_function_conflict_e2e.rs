//! A class's constructors, and those a typealias to a class exposes, are callables named by the
//! class or the alias in its package. kotlinc reports each of them against a same-named function
//! of the same overload shape, at the constructor's signature or at the alias, and reports the
//! function too unless the constructor is one only its class can call.

use super::common;

/// Require krusty's complete errors to be kotlinc's and to read exactly `expected`.
fn assert_errors(sources: &[(&str, &str)], expected: &[&str]) {
    common::assert_error_blocks_match_kotlinc(sources, &[]);
    assert_eq!(common::reference_error_blocks(sources, &[]), expected);
}

#[test]
fn a_class_constructor_conflicts_with_a_function_of_its_shape() {
    let source = "\
package p
class K
fun K() = 1
abstract class A(x: Int)
fun A(x: Int) = 1
annotation class N(val x: Int)
fun N(x: Int) {}
class W(x: Int) { constructor(s: String) : this(1) }
fun W(s: String) = 1
private fun W(x: Int) = 1
class U public constructor(x: Int)
fun U(x: Int): Unit {}
";
    assert_errors(
        &[("test.kt", source)],
        &[
            "test.kt:2:7: conflicting overloads:",
            "| fun K(): Int",
            "test.kt:3:1: conflicting overloads:",
            "| constructor(): K",
            "test.kt:4:17: conflicting overloads:",
            "| fun A(x: Int): Int",
            "test.kt:5:1: conflicting overloads:",
            "| constructor(x: Int): A",
            "test.kt:6:19: conflicting overloads:",
            "| fun N(x: Int): Unit",
            "test.kt:7:1: conflicting overloads:",
            "| constructor(x: Int): N",
            "test.kt:8:8: conflicting overloads:",
            "| fun W(x: Int): Int",
            "test.kt:8:19: conflicting overloads:",
            "| fun W(s: String): Int",
            "test.kt:9:1: conflicting overloads:",
            "| constructor(s: String): W",
            "test.kt:10:9: conflicting overloads:",
            "| constructor(x: Int): W",
            "test.kt:11:16: conflicting overloads:",
            "| fun U(x: Int): Unit",
            "test.kt:12:1: conflicting overloads:",
            "| constructor(x: Int): U",
        ],
    );
}

#[test]
fn a_constructor_only_its_class_calls_reports_the_function_alone() {
    let source = "\
package p
class P private constructor(x: Int)
fun P(x: Int) = 1
sealed class S(x: Int)
fun S(x: Int) = 1
class Q(x: Int) { protected constructor(s: String) : this(1) }
fun Q(s: String) = 1
";
    assert_errors(
        &[("test.kt", source)],
        &[
            "test.kt:2:17: conflicting overloads:",
            "| fun P(x: Int): Int",
            "test.kt:4:15: conflicting overloads:",
            "| fun S(x: Int): Int",
            "test.kt:6:29: conflicting overloads:",
            "| fun Q(s: String): Int",
        ],
    );
}

#[test]
fn a_typealias_names_its_target_constructors_over_its_own_type_parameters() {
    let source = "\
package p
class C(val x: Int)
class G<T>(val t: T)
class D { constructor(a: Int); constructor(b: String) }
typealias CC = C
fun CC(y: Int) = y
typealias GG<X> = G<X>
fun <Y> GG(t: Y) = t
typealias DD = D
fun DD(z: String) = 1
typealias L = G<Int>
fun L(t: Int) = 1
private typealias JL = java.util.ArrayList<String>
fun JL() = 1
";
    assert_errors(
        &[("test.kt", source)],
        &[
            "test.kt:5:1: conflicting overloads:",
            "| fun CC(y: Int): Int",
            "test.kt:6:1: conflicting overloads:",
            "| constructor(x: Int): C",
            "test.kt:7:1: conflicting overloads:",
            "| fun <Y> GG(t: Y): Y",
            "test.kt:8:1: conflicting overloads:",
            "| constructor<X>(t: X): G<X>",
            "test.kt:9:1: conflicting overloads:",
            "| fun DD(z: String): Int",
            "test.kt:10:1: conflicting overloads:",
            "| constructor(b: String): D",
            "test.kt:11:1: conflicting overloads:",
            "| fun L(t: Int): Int",
            "test.kt:12:1: conflicting overloads:",
            "| constructor(t: Int): G<Int>",
            "test.kt:13:1: conflicting overloads:",
            "| fun JL(): Int",
            "test.kt:14:1: conflicting overloads:",
            "| constructor(): ArrayList<String>",
        ],
    );
}

#[test]
fn a_file_private_declaration_conflicts_only_with_what_its_file_sees() {
    let first = "\
package p
class X(val x: Int)
private class Y(val y: Int)
fun Z(z: Int) = 1
private fun Q(q: Int) = 1
class R internal constructor(r: Int)
";
    let second = "\
package p
fun X(x: Int) = 1
fun Y(y: Int) = 1
class Z(val z: Int)
class Q(val q: Int)
fun R(r: Int) = 1
";
    assert_errors(
        &[("a.kt", first), ("b.kt", second)],
        &[
            "a.kt:2:8: conflicting overloads:",
            "| fun X(x: Int): Int",
            "a.kt:3:16: conflicting overloads:",
            "| fun Y(y: Int): Int",
            "a.kt:4:1: conflicting overloads:",
            "| constructor(z: Int): Z",
            "a.kt:5:9: conflicting overloads:",
            "| constructor(q: Int): Q",
            "a.kt:6:18: conflicting overloads:",
            "| fun R(r: Int): Int",
            "b.kt:2:1: conflicting overloads:",
            "| constructor(x: Int): X",
            "b.kt:4:8: conflicting overloads:",
            "| fun Z(z: Int): Int",
            "b.kt:6:1: conflicting overloads:",
            "| constructor(r: Int): R",
        ],
    );
}

#[test]
fn a_different_overload_shape_is_no_conflict() {
    let source = "\
package p
class D(x: Int = 0)
fun D() = 1
class Vr(vararg x: Int)
fun Vr(x: IntArray) = 1
class Rec<T : Comparable<T>>(t: T)
fun <U> Rec(t: U) = 1
interface I
fun I() = 1
typealias II = I
fun II() = 1
object O
fun O() = 1
";
    common::assert_sources_accepted_like_kotlinc(&[("test.kt", source)], &[]);
}

#[test]
fn a_low_priority_or_hidden_declaration_yields_instead_of_conflicting() {
    let source = "\
package p
class C @Suppress(\"INVISIBLE_MEMBER\", \"INVISIBLE_REFERENCE\") @kotlin.internal.LowPriorityInOverloadResolution constructor(x: Int)
fun C(x: Int) = 1
class D(x: Int)
@Suppress(\"INVISIBLE_MEMBER\", \"INVISIBLE_REFERENCE\")
@kotlin.internal.LowPriorityInOverloadResolution
fun D(x: Int) = 1
class H(x: Int)
@Deprecated(\"\", level = DeprecationLevel.HIDDEN)
fun H(x: Int) = 1
class Q @Deprecated(\"\", level = DeprecationLevel.HIDDEN) constructor(x: Int)
fun Q(x: Int) = 1
";
    common::assert_sources_accepted_like_kotlinc(&[("test.kt", source)], &[]);
}

#[test]
fn only_two_low_priority_or_two_visible_declarations_conflict() {
    let source = "\
package p
@Suppress(\"INVISIBLE_MEMBER\", \"INVISIBLE_REFERENCE\")
@kotlin.internal.LowPriorityInOverloadResolution
fun f(x: Int) = 1
@Suppress(\"INVISIBLE_MEMBER\", \"INVISIBLE_REFERENCE\")
@kotlin.internal.LowPriorityInOverloadResolution
fun f(x: Int) = 2
@Deprecated(\"\", level = DeprecationLevel.HIDDEN)
fun g(x: Int) = 1
@Deprecated(\"\", level = DeprecationLevel.HIDDEN)
fun g(x: Int) = 2
@Deprecated(\"\", level = DeprecationLevel.ERROR)
fun h(x: Int) = 1
fun h(x: Int) = 2
";
    assert_errors(
        &[("test.kt", source)],
        &[
            "test.kt:4:1: conflicting overloads:",
            "| fun f(x: Int): Int",
            "test.kt:7:1: conflicting overloads:",
            "| fun f(x: Int): Int",
            "test.kt:13:1: conflicting overloads:",
            "| fun h(x: Int): Int",
            "test.kt:14:1: conflicting overloads:",
            "| fun h(x: Int): Int",
        ],
    );
}
