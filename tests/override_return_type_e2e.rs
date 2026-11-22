//! An overriding function's result must be a subtype of each overridden member's result, seen
//! from the overriding class. kotlinc reports the first mismatching member, in supertype order, at
//! the declared result, or at the function name when the result is inferred, and names the
//! overridden member's owner as the class that substitutes its type arguments (or the declaring
//! class when none does).
use super::common;

#[test]
fn an_override_result_must_be_a_subtype_of_the_overridden_result() {
    let source = "\
package p

interface Source<T> { fun get(): T; fun <R> map(r: R): List<R>; fun peek(): T? = null }
class Explicit : Source<String> {
    override fun get(): Int = 1
    override fun <R> map(r: R): Set<R> = setOf()
}
abstract class Middle : Source<String>
abstract class Inherited : Middle() { override fun peek(): Int = 1 }
open class Root { open fun name(): String = \"\" }
open class Base : Root()
class Unsubstituted : Base() { override fun name(): String? = null }
open class Generic<T> { open fun count(): String = \"\" }
class Applied : Generic<Int>() { override fun count(): Int = 1 }
class Inferred : Root() { override fun name() = 1 }
class Equality { override fun equals(other: Any?): Int = 1 }
interface Optional { fun get(): String? = null }
interface Required { fun get(): String = \"\" }
class First : Optional, Required { override fun get(): Int = 1 }
abstract class Text : CharSequence {
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence? = null
}
class Outer { class Nested : Root() { override fun name(): Int = 1 } }

fun local() {
    val source = object : Source<String> {
        override fun get() = null
        override fun <R> map(r: R): List<R> = listOf()
    }
    val root = object : Root() { override fun name() = 2 }
}

open class Animal
class Cat : Animal()
interface Shelter<T> { fun adopt(): T?; fun all(): List<T> }
class Covariant : Shelter<Animal> {
    override fun adopt(): Cat = Cat()
    override fun all(): ArrayList<Animal> = ArrayList()
}
class Runner : Runnable { override fun run(): Nothing = TODO() }
interface Pick { fun <T> pick(value: T): T }
class Picker : Pick { override fun <U> pick(value: U): U = value }
";
    let sources = [("test.kt", source)];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    let mismatch = |at: &str, actual: &str, expected: &str, owner: &str| {
        format!(
            "test.kt:{at}: return type of '{actual}' is not a subtype of the return type of the \
             overridden member '{expected}' defined in '{owner}'."
        )
    };
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            mismatch("5:25", "fun get(): Int", "fun get(): String", "p.Explicit"),
            mismatch(
                "6:33",
                "fun <R> map(r: R): Set<R>",
                "fun <R> map(r: R): List<R>",
                "p.Explicit"
            ),
            mismatch("9:60", "fun peek(): Int", "fun peek(): String?", "p.Middle"),
            mismatch(
                "12:53",
                "fun name(): String?",
                "fun name(): String",
                "p.Root"
            ),
            mismatch(
                "14:56",
                "fun count(): Int",
                "fun count(): String",
                "p.Applied"
            ),
            mismatch("15:40", "fun name(): Int", "fun name(): String", "p.Root"),
            mismatch(
                "16:52",
                "fun equals(other: Any?): Int",
                "fun equals(other: Any?): Boolean",
                "kotlin.Any"
            ),
            mismatch(
                "19:56",
                "fun get(): Int",
                "fun get(): String?",
                "p.Optional"
            ),
            mismatch(
                "21:63",
                "fun subSequence(startIndex: Int, endIndex: Int): CharSequence?",
                "fun subSequence(startIndex: Int, endIndex: Int): CharSequence",
                "kotlin.CharSequence"
            ),
            mismatch("23:60", "fun name(): Int", "fun name(): String", "p.Root"),
            mismatch(
                "27:22",
                "fun get(): Nothing?",
                "fun get(): String",
                "p.<anonymous>"
            ),
            mismatch("30:47", "fun name(): Int", "fun name(): String", "p.Root"),
        ]
    );
}

/// A Java result is flexible, and so is a Java type argument naming a type parameter: `String!`
/// accepts a `String?` override, `Inv<E!>!` accepts `Inv<E?>`, and the rendering keeps the `!`.
#[test]
fn a_java_result_keeps_its_flexibility() {
    let named = "\
public class Named {
    public String name() { return \"\"; }
    public java.util.List<String> all() { return null; }
}
";
    let factory = "\
public interface Factory<T> {
    T make();
    Inv<T> wrap();
}
";
    let inv = "public class Inv<T> { public Inv(T t) {} }\n";
    let source = "\
class Relaxed : Named() {
    override fun name(): String? = null
    override fun all(): MutableList<String?>? = null
}
class Mismatched : Named() { override fun name(): Int = 1 }
class Wrapper<E> : Factory<E> {
    override fun make(): E? = null
    override fun wrap(): Inv<E?> = Inv<E?>(null)
}
abstract class Strings : Factory<String> { override fun make(): Int = 1 }
";
    let sources = [
        ("Named.java", named),
        ("Factory.java", factory),
        ("Inv.java", inv),
        ("test.kt", source),
    ];
    common::assert_error_blocks_match_kotlinc(&sources, &[]);
    assert_eq!(
        common::reference_error_blocks(&sources, &[]),
        [
            "test.kt:5:51: return type of 'fun name(): Int' is not a subtype of the return type \
             of the overridden member 'fun name(): String!' defined in 'Named'.",
            "test.kt:10:65: return type of 'fun make(): Int' is not a subtype of the return type \
             of the overridden member 'fun make(): String!' defined in 'Strings'.",
        ]
    );
}

/// An inner class's inferred results read the outer class's type parameter through the inner
/// receiver, so `next()` overrides `Iterator<T>.next()` and plain members keep `T` too.
#[test]
fn an_inner_class_result_keeps_the_outer_type_parameter() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/InnerInferredOuterTypeParameter.kt"),
        "InnerInferredOuterTypeParameter",
    );
}
