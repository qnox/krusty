//! A type parameter whose erased upper bound is a value class is realized as that value class.
//!
//! kotlinc's type mapper erases `T : IC` to `IC`'s own representation and keeps the occurrence's
//! nullability on the bound, so `T : IC?` is realized exactly as `IC?` is (the carrier when `IC?`
//! stays unboxed, the box otherwise), and `T : Int?` as `Integer`. Its value-class mangle reads the
//! same erased bound, so such a signature is `-<hash>`-suffixed like one that names the value class
//! directly. Each class below is compared byte for byte with kotlinc.

use super::common;

fn assert_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc_cp(name, src, class, &[common::stdlib_jar()]) {
        Some(Ok(())) => {}
        Some(Err(diff)) => panic!("{diff}"),
        None => panic!("{name}: reference toolchain unavailable"),
    }
}

/// `T : IC?` over a non-null reference carrier is the unboxed carrier; over a nullable carrier it is
/// the box. Both results take the return mangle of `IC?` (`:LIC?;`), not of `IC`.
#[test]
fn an_interface_result_bounded_by_a_nullable_value_class_erases_like_it() {
    let src = "@JvmInline value class Carrier(val x: Any)\n\
               @JvmInline value class Boxed(val x: Any?)\n\
               interface Source<out T : Carrier?, out B : Boxed?> {\n\
               \x20   fun carrier(): T\n\
               \x20   fun boxed(): B\n\
               }\n";
    assert_identical("NullableValueClassBound", src, "Source");
}

/// A generic value class whose underlying is a nullable type parameter (`val x: T?`) carries the
/// nullable bound, so its own nullable form stays boxed.
#[test]
fn a_nullable_type_parameter_underlying_keeps_its_nullability() {
    let src = "@JvmInline value class Holder<T : Any>(val x: T?)\n\
               interface Source<out T : Holder<String>?> {\n\
               \x20   fun get(): T\n\
               }\n";
    assert_identical("NullableGenericUnderlying", src, "Source");
}

/// Function type parameters: the bound's value class or primitive keeps the occurrence's
/// nullability (`T : Int?` is `Integer`), and the mangle hashes the erased bound. The library is
/// compiled by kotlinc, so every call below links only if krusty names and describes each method
/// exactly as kotlinc declared it.
#[test]
fn function_type_parameters_link_against_kotlinc_declarations() {
    const LIB: &str = "package lib\n\
        @JvmInline value class I(val i: Int)\n\
        @JvmInline value class A(val a: Any)\n\
        @JvmInline value class N(val a: Any?)\n\
        class Fns {\n\
        \x20   fun <T : I> nonNull(x: T): T = x\n\
        \x20   fun <T : I?> nullableBound(x: T): T = x\n\
        \x20   fun <T : I> nullableUse(x: T?): T? = x\n\
        \x20   fun <T : A?> reference(x: T): T = x\n\
        \x20   fun <T : N?> boxed(x: T): T = x\n\
        \x20   fun <T : Int?> primitive(x: T): T = x\n\
        }\n";
    const MAIN: &str = "import lib.*\n\
        fun box(): String {\n\
        \x20   val f = Fns()\n\
        \x20   if (f.nonNull(I(1)).i != 1) return \"nonNull\"\n\
        \x20   if (f.nullableBound<I?>(null) != null) return \"nullableBound\"\n\
        \x20   if (f.nullableUse<I>(I(2))?.i != 2) return \"nullableUse\"\n\
        \x20   if (f.reference(A(\"a\")).a != \"a\") return \"reference\"\n\
        \x20   if (f.boxed(N(null)) != N(null)) return \"boxed\"\n\
        \x20   if (f.primitive<Int?>(3) != 3) return \"primitive\"\n\
        \x20   return \"OK\"\n\
        }\n";
    let output = common::expect_box_run_against_kotlinc(LIB, MAIN)
        .expect("reference compiler and JVM toolchain are required for dependency linking");
    assert_eq!(output, "OK");
}

/// A mangle suffix is `-` plus seven base64url characters, and those characters include `-`
/// (`memberFun--ndakOA`). A call through an interface must keep the declaration's single suffix.
#[test]
fn a_hash_that_contains_a_dash_is_not_mangled_twice() {
    let src = "@JvmInline value class S(val x: String)\n\
               interface IFoo { fun memberFun(s1: S, s2: String): String }\n\
               object FooImpl : IFoo { override fun memberFun(s1: S, s2: String): String = s1.x + s2 }\n\
               fun box(): String {\n\
               \x20   val foo: IFoo = FooImpl\n\
               \x20   return foo.memberFun(S(\"O\"), \"K\")\n\
               }\n";
    common::expect_box_same_as_kotlinc(src, "DashedHash");
}

/// `val x: T?` over `T : Int` is carried as `Integer`, so reading it yields a reference that is
/// null-checked before it is unboxed, never an `int`.
#[test]
fn a_nullable_occurrence_of_a_primitive_bounded_type_parameter_is_its_wrapper() {
    let src = "@JvmInline value class Z1<T : Int>(val x: T?)\n\
               fun wrap(n: Int): Z1<Int>? = if (n < 0) null else Z1(n)\n\
               fun box(): String {\n\
               \x20   if (wrap(42)!!.x != 42) return \"x\"\n\
               \x20   if (Z1<Int>(null).x != null) return \"null\"\n\
               \x20   return \"OK\"\n\
               }\n";
    common::expect_box_same_as_kotlinc(src, "NullablePrimitiveBound");
}

/// Same-arity overloads keep their selected declaration's mangle and descriptor. The nullable
/// underlying of `NullableBox` keeps that bound boxed; `TextBox` uses its reference carrier.
#[test]
fn same_arity_value_class_bound_overloads_keep_exact_realizations() {
    let src = "@JvmInline value class NullableBox(val x: Any?)\n\
               @JvmInline value class TextBox(val x: String)\n\
               interface Choices {\n\
               \x20   fun <T : NullableBox?> pick(value: T): T = value\n\
               \x20   fun pick(value: TextBox): TextBox = value\n\
               }\n\
               class Implementation : Choices\n\
               fun box(): String {\n\
               \x20   val choices: Choices = Implementation()\n\
               \x20   val nullable = choices.pick<NullableBox?>(NullableBox(null))\n\
               \x20   val text = choices.pick(TextBox(\"OK\"))\n\
               \x20   return if (nullable?.x == null) text.x else \"nullable\"\n\
               }\n";
    common::expect_box_same_as_kotlinc(src, "BoundOverloads");
}
