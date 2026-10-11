//! Annotation classes as VALUES: `Anno("OK", 42)` constructed, read, compared and rendered.
//!
//! Kotlin defines an annotation instance's `equals`, `hashCode` and `toString` over its MEMBERS
//! rather than by identity, and an ARRAY member is compared, hashed and rendered by CONTENT. That
//! last one is the whole difference from a data class, where an array member is compared by
//! identity — so the two cannot share one synthesis.
//!
//! `hashCode` is a contract a program can read rather than an implementation detail: it is the sum
//! of `(127 * name.hashCode()) xor value.hashCode()` over the members, and the corpus's
//! `annotations/instances/annotationEqHc.kt` computes that sum in Kotlin and compares. The member
//! name's hash is taken at run time so it is the same `String.hashCode` the program's own
//! `name.hashCode()` reaches.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// Construction and member reads, over every member shape an annotation may declare.
#[test]
fn an_annotation_instance_is_constructed_and_its_members_read() {
    let source = "enum class E { E0 }\n\
         annotation class Empty\n\
         annotation class A(val b: Byte, val s: Short, val i: Int, val l: Long,\n\
         \x20                val f: Float, val d: Double, val c: Char, val bool: Boolean)\n\
         annotation class Anno(val s: String, val i: Int, val e: E, val a: A,\n\
         \x20                   val arr: Array<String>, val intArr: IntArray)\n\
         fun box(): String {\n\
         \x20   val a = A(1, 1, 1, 1L, 1.0f, 1.0, 'c', true)\n\
         \x20   val anno = Anno(\"OK\", 42, E.E0, a, arrayOf(\"x\"), intArrayOf(1, 2))\n\
         \x20   if (anno.i != 42) return \"fail i\"\n\
         \x20   if (anno.e != E.E0) return \"fail e\"\n\
         \x20   if (anno.arr.size != 1 || anno.arr[0] != \"x\") return \"fail arr\"\n\
         \x20   if (anno.intArr[1] != 2) return \"fail intArr\"\n\
         \x20   val inner = anno.a\n\
         \x20   if (inner.b != 1.toByte() || inner.l != 1L || inner.c != 'c' || !inner.bool)\n\
         \x20       return \"fail inner\"\n\
         \x20   if (Empty() != Empty()) return \"fail empty\"\n\
         \x20   return anno.s\n\
         }\n";
    expect_box_ok_with_stdlib(source, "AnnotationInstance");
    expect_native_box(source, "AnnotationInstance", "OK");
}

/// `equals` and `hashCode` over the members, with arrays by CONTENT and the contract sum.
///
/// `Float.NaN` and `Double.NaN` are in the members deliberately: an annotation compares them the
/// way their boxes do, so NaN equals itself — the opposite of what `==` on the machine answers.
#[test]
fn an_annotation_instance_compares_and_hashes_by_its_members() {
    let source = "annotation class Bar(val i: Int, val s: String, val f: Float, val d: Double)\n\
         annotation class Foo(val int: Int, val s: String, val arr: Array<String>,\n\
         \x20                  val arr2: IntArray, val bar: Bar)\n\
         fun makeHC(name: String, value: Any) = (127 * name.hashCode()) xor value.hashCode()\n\
         fun box(): String {\n\
         \x20   val one = Foo(42, \"foo\", arrayOf(\"a\", \"b\"), intArrayOf(1, 2),\n\
         \x20       Bar(10, \"bar\", Float.NaN, Double.NaN))\n\
         \x20   val two = Foo(42, \"foo\", arrayOf(\"a\", \"b\"), intArrayOf(1, 2),\n\
         \x20       Bar(10, \"bar\", Float.NaN, Double.NaN))\n\
         \x20   // Distinct arrays with equal contents: an annotation compares them by content.\n\
         \x20   if (one != two) return \"fail equals\"\n\
         \x20   if (one.hashCode() != two.hashCode()) return \"fail hash agreement\"\n\
         \x20   val expected = makeHC(\"i\", 10) + makeHC(\"s\", \"bar\") +\n\
         \x20       makeHC(\"f\", Float.NaN) + makeHC(\"d\", Double.NaN)\n\
         \x20   if (expected != one.bar.hashCode()) return \"fail the contract sum\"\n\
         \x20   if (one == Foo(43, \"foo\", arrayOf(\"a\", \"b\"), intArrayOf(1, 2),\n\
         \x20       Bar(10, \"bar\", Float.NaN, Double.NaN))) return \"fail a differing member\"\n\
         \x20   if (one == Foo(42, \"foo\", arrayOf(\"a\", \"c\"), intArrayOf(1, 2),\n\
         \x20       Bar(10, \"bar\", Float.NaN, Double.NaN))) return \"fail a differing element\"\n\
         \x20   if (one.equals(\"foo\")) return \"fail a value of another type\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "AnnotationEquality");
    expect_native_box(source, "AnnotationEquality", "OK");
}

/// The two zeroes stay distinct and NaN equals itself — the total order, as an annotation's
/// members are compared through their boxes.
#[test]
fn an_annotation_compares_its_floating_point_members_on_the_total_order() {
    let source = "annotation class F(val f: Float, val d: Double)\n\
         fun box(): String {\n\
         \x20   if (F(Float.NaN, Double.NaN) != F(Float.NaN, Double.NaN)) return \"fail NaN\"\n\
         \x20   if (F(0.0f, 0.0) == F(-0.0f, -0.0)) return \"fail zeroes\"\n\
         \x20   if (F(0.0f, 0.0) != F(0.0f, 0.0)) return \"fail same zero\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "AnnotationFloats");
    expect_native_box(source, "AnnotationFloats", "OK");
}
