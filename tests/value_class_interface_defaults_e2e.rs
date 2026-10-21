//! An inherited interface default whose applied supertype substitutes a value class.
//!
//! kotlinc's fake override of `fun foo(x: T)` in a class implementing `IFoo<Z>` has the
//! value-class signature `foo(x: Z)`, so it is named by that signature like any member mentioning
//! a value class (`foo-IQRRRT4`) and takes the carrier (`(I)`). The class writes it as a forwarder
//! that boxes the carrier for the interface's erased `foo(Object)` and unboxes a value-class
//! result; the declaration's own JVM name and descriptor become an erased bridge back to it. A
//! value class also realizes the fake override as a static over its carrier under that same
//! mangled name (`foo--h0D71M(JJ)` for `L : IFoo<L>`, `next-GKOAj6k(I)I` for `A : X<A>`), and a
//! call on the value class itself selects that static.
//!
//! krusty kept the declaration's erased shape (`foo-impl(long, Object)`, `foo(Object)`) for these
//! members, and wrote no typed forwarder at all for an ordinary class.

use super::common;

/// Each listed class is written byte for byte as kotlinc writes it.
fn assert_classes_match_kotlinc(file: &str, source: &str, classes: &[&str]) {
    let built = common::classes_against_kotlinc_module(&[(file, source)]);
    let mismatches = classes
        .iter()
        .filter_map(|&class| {
            let reference = built
                .reference
                .get(class)
                .unwrap_or_else(|| panic!("kotlinc writes {class}"));
            let ours = built
                .krusty
                .get(class)
                .unwrap_or_else(|| panic!("krusty writes {class}"));
            (reference != ours).then(|| common::exact_class_difference(class, reference, ours))
        })
        .collect::<Vec<_>>();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n\n"));
}

/// `defaultInterfaceMethodsInInlineClass`: a value class substituting itself, with a member and an
/// extension default.
const SELF_SUBSTITUTED: &str = "interface IFoo<T> {\n\
\x20   fun foo(x: T): String = \"O\"\n\
\x20   fun T.bar(): String = \"K\"\n\
}\n\
@JvmInline value class L(val x: Long) : IFoo<L>\n\
fun box(): String {\n\
\x20   val z = L(0L)\n\
\x20   return with(z) { foo(z) + z.bar() }\n\
}\n";

/// `genericDefaultInterfaceExtensionFunCall`: an F-bounded extension default over a primitive, a
/// wide and a reference carrier.
const BOUNDED_EXTENSION: &str = "interface IFoo<T : IFoo<T>> {\n\
\x20   fun T.foo(): String = bar()\n\
\x20   fun bar(): String\n\
}\n\
@JvmInline value class Z(val x: Int) : IFoo<Z> { override fun bar(): String = \"OK\" }\n\
@JvmInline value class L(val x: Long) : IFoo<L> { override fun bar(): String = \"OK\" }\n\
@JvmInline value class S(val x: String) : IFoo<S> { override fun bar(): String = x }\n\
fun Z.testZ() { if (Z(42).foo() != \"OK\") throw AssertionError() }\n\
fun L.testL() { if (L(4L).foo() != \"OK\") throw AssertionError() }\n\
fun S.testS() { if (S(\"OK\").foo() != \"OK\") throw AssertionError() }\n\
fun box(): String {\n\
\x20   Z(42).testZ()\n\
\x20   L(4L).testL()\n\
\x20   S(\"\").testS()\n\
\x20   return \"OK\"\n\
}\n";

/// `genericDefaultInterfaceMethodCall` and `boxReturnValueInDefaultMethod`: a value-class
/// parameter and a value-class result.
const PARAMETER_AND_RESULT: &str = "interface IFoo<T : IFoo<T>> {\n\
\x20   fun foo(t: T): String = t.bar()\n\
\x20   fun bar(): String\n\
}\n\
@JvmInline value class Z(val x: Int) : IFoo<Z> { override fun bar(): String = \"OK\" }\n\
interface X<T> {\n\
\x20   operator fun plus(n: Int): T\n\
\x20   fun next(): T = this + 1\n\
}\n\
@JvmInline value class A(val value: Int) : X<A> {\n\
\x20   override operator fun plus(n: Int) = A(value + n)\n\
}\n\
fun box(): String {\n\
\x20   if (Z(1).foo(Z(2)) != \"OK\") return \"fail foo\"\n\
\x20   val res = A(1).next()\n\
\x20   return if (res.value == 2) \"OK\" else \"FAIL $res\"\n\
}\n";

/// Ordinary classes and a value class over every substituted position: a parameter beside a
/// declared value class, a boxed nullable result, a property, a primitive result substituted
/// alongside, a nullable reference carrier, and a `super` call that names the typed forwarder.
const ORDINARY_CLASSES: &str = "@JvmInline value class Z(val x: Int)\n\
@JvmInline value class S(val x: String)\n\
interface I<T> {\n\
\x20   fun g(z: Z, t: T): String = \"g\" + t\n\
\x20   val p: T? get() = null\n\
\x20   fun pair(x: T, y: Int): T = x\n\
\x20   fun opt(x: T?): T? = x\n\
}\n\
interface U<T, R> { fun f(t: T): R? = null }\n\
open class D : I<Z>, U<Z, Int>\n\
class E : D() {\n\
\x20   fun callG(t: Z): String = \"E\" + super.g(Z(1), t)\n\
\x20   override fun pair(x: Z, y: Int): Z = Z(super.pair(x, y).x * y)\n\
}\n\
class N : I<S>\n\
@JvmInline value class W(val x: Long) : I<W>, U<W, Int>\n\
fun box(): String {\n\
\x20   val d = D()\n\
\x20   val i: I<Z> = E()\n\
\x20   val w = W(5L)\n\
\x20   val r = d.g(Z(1), Z(2)) + d.p + d.pair(Z(3), 1).x + d.f(Z(0)) + E().callG(Z(4)) +\n\
\x20       i.pair(Z(3), 2).x + N().opt(S(\"a\"))?.x + N().opt(null) + w.g(Z(1), w) + w.p +\n\
\x20       w.pair(w, 1).x + (w as I<W>).pair(W(6L), 1).x + w.f(w)\n\
\x20   return if (r == \"gZ(x=2)null3nullEgZ(x=4)6anullgW(x=5)null56null\") \"OK\" else r\n\
}\n";

#[test]
fn a_value_class_substituting_itself_names_its_inherited_defaults_by_the_substituted_signature() {
    assert_classes_match_kotlinc("SelfSubstituted.kt", SELF_SUBSTITUTED, &["L"]);
}

#[test]
fn a_value_class_substituting_itself_runs_its_inherited_defaults() {
    common::expect_box_ok_with_stdlib(SELF_SUBSTITUTED, "SelfSubstituted");
}

#[test]
fn an_inherited_extension_default_is_named_for_each_carrier() {
    assert_classes_match_kotlinc("BoundedExtension.kt", BOUNDED_EXTENSION, &["Z", "L", "S"]);
}

#[test]
fn an_inherited_extension_default_runs_for_each_carrier() {
    common::expect_box_ok_with_stdlib(BOUNDED_EXTENSION, "BoundedExtension");
}

#[test]
fn an_inherited_default_with_a_value_class_parameter_or_result_takes_the_carrier() {
    assert_classes_match_kotlinc("ParameterAndResult.kt", PARAMETER_AND_RESULT, &["Z", "A"]);
}

#[test]
fn an_inherited_default_with_a_value_class_parameter_or_result_runs() {
    common::expect_box_ok_with_stdlib(PARAMETER_AND_RESULT, "ParameterAndResult");
}

#[test]
fn an_ordinary_class_writes_the_mangled_typed_forwarder_and_its_erased_bridge() {
    assert_classes_match_kotlinc(
        "OrdinaryClasses.kt",
        ORDINARY_CLASSES,
        &["D", "E", "N", "W"],
    );
}

#[test]
fn an_ordinary_class_answers_through_the_typed_forwarder_and_its_bridge() {
    common::expect_box_ok_with_stdlib(ORDINARY_CLASSES, "OrdinaryClasses");
}
