//! Unbound top-level function references `::foo` passed to a function-typed parameter. Lowered to the
//! same `invokedynamic` + `LambdaMetafactory` machinery as a lambda, with the impl method handle
//! pointing directly at the referenced function. Round-tripped against the JVM under `-Xverify:all`.

use super::common;

#[test]
fn callable_refs_run() {
    const SRC: &str = "fun inc(n: Int): Int = n + 1\n\
fun twice(n: Int): Int = n * 2\n\
fun apply1(f: (Int) -> Int, x: Int): Int = f(x)\n\
fun box(): String {\n\
if (apply1(::inc, 41) != 42) return \"f1\"\n\
if (apply1(::twice, 21) != 42) return \"f2\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "C");
}

#[test]
fn bound_member_ref_flows_to_classpath_map() {
    const SRC: &str = "class C(val base: Int) {\n\
fun inc(x: Int) = x + 1\n\
fun add(a: Int, b: Int) = a + b + base\n\
}\n\
fun box(): String {\n\
val c = C(10)\n\
if (c.inc(5) != 6) return \"f1\"\n\
if (c.add(2, 3) != 15) return \"f2\"\n\
val r = listOf(1, 2, 3).map(c::inc)\n\
if (r != listOf(2, 3, 4)) return \"f3:$r\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "BoundMapRef");
}

#[test]
fn property_ref_keeps_api_and_fits_function_shape() {
    const SRC: &str = "class C(val n: Int)\n\
fun apply1(f: (C) -> Int, c: C): Int = f(c)\n\
fun box(): String {\n\
val p = C::n\n\
if (p.get(C(3)) != 3) return \"get\"\n\
if (p.name != \"n\") return \"name:${p.name}\"\n\
val f: (C) -> Int = p\n\
if (f(C(4)) != 4) return \"fun\"\n\
if (apply1(p, C(5)) != 5) return \"hof\"\n\
if (listOf(C(6)).map(p)[0] != 6) return \"map\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "PropertyRefShape");
}

#[test]
fn class_literal_type_is_provider_backed() {
    const SRC: &str = "class C\n\
fun box(): String {\n\
val c = C::class\n\
return if (c.name.endsWith(\"C\")) \"OK\" else c.name\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "ClassLiteralShape");
}

/// A callable reference / class literal on a NULLABLE receiver type (`A?::foo`, `A?::class`). The `?`
/// only marks the receiver type nullable; the reference is the same callable. Previously the parser
/// emitted "expected an expression" at `?::`.
#[test]
fn nullable_receiver_callable_ref_runs() {
    const SRC: &str = "class A { fun foo(): String = \"OK\" }\n\
fun box(): String {\n\
    val r: (A) -> String = A?::foo\n\
    val a = A()\n\
    if (r(a) != \"OK\") return \"f1\"\n\
    if (A?::class.simpleName != \"A\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "CR");
}

/// An unbound member reference on a GENERIC class with explicit type arguments (`A<String>::foo`).
/// The type arguments erase, so it references `A::foo`. Previously the parser misread `A<String>` as a
/// less-than comparison chain and reported `unresolved reference 'A'`.
#[test]
fn generic_class_unbound_member_ref_runs() {
    const SRC: &str = "class A<T>(val t: T) { fun foo(): T = t }\n\
fun box(): String = (A<String>::foo).let { it(A(\"OK\")) }\n";
    common::expect_box_ok_with_stdlib(SRC, "CR");
}

/// ADAPTED bound member references: a reference to a member with a trailing `vararg` and/or a default
/// parameter, used where a lower-arity functional type is expected (`C(7)::mv` as `(Int) -> String`).
/// The lowerer's synthesized adapter fills the empty vararg / default via the member's `$default` stub.
#[test]
fn adapted_bound_member_ref_runs() {
    const SRC: &str = "// WITH_STDLIB\n\
class C(val e: Int) {\n\
    fun mv(i: Int, vararg s: String): String = if (i == e && s.isEmpty()) \"\" else \"bad\"\n\
    fun md(i: Int, s: String = \"d\"): String = if (i == e && s == \"d\") \"\" else \"bad\"\n\
    fun mb(i: Int, s: String = \"d\", vararg t: String): String = if (i == e && s == \"d\" && t.isEmpty()) \"\" else \"bad\"\n\
}\n\
fun test(f: (Int) -> String, p: Int): String = f(p)\n\
fun box(): String {\n\
    if (test(C(7)::mv, 7) != \"\") return \"f1\"\n\
    if (test(C(7)::md, 7) != \"\") return \"f2\"\n\
    if (test(C(7)::mb, 7) != \"\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "CR");
}
