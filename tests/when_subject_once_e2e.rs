//! A `when` subject is evaluated once, whatever its branches test.
//!
//! `when (next()) { is A -> …; is B -> … }` binds `next()` to a temporary and every type test reads
//! it. krusty bound the temporary but lowered each `is` test over the subject expression again, so
//! the call ran once for the unused temporary and once more per test: a side-effecting subject
//! (a call, a property getter) was observed several times and each test saw a different value.
use super::common;

const SOURCE: &str = "sealed interface Shape\n\
    class Circle(val r: Int) : Shape\n\
    class Square(val side: Int) : Shape\n\
    var calls = 0\n\
    fun next(): Shape { calls++; return if (calls == 1) Square(3) else Circle(9) }\n\
    fun area(): Int = when (next()) {\n\
    \x20   is Circle -> 1\n\
    \x20   is Square -> 2\n\
    }\n\
    fun named(): String = when (next()) {\n\
    \x20   is Circle -> \"circle\"\n\
    \x20   else -> \"other\"\n\
    }\n\
    fun negated(): String = when (next()) {\n\
    \x20   !is Circle -> \"other\"\n\
    \x20   else -> \"circle\"\n\
    }\n\
    var numberReads = 0\n\
    fun number(): Int { numberReads++; return 2 }\n\
    fun ranged(): Int = when (number()) {\n\
    \x20   in 1..3 -> 7\n\
    \x20   else -> 0\n\
    }\n\
    fun outside(): Int = when (number()) {\n\
    \x20   !in 4..6 -> 8\n\
    \x20   else -> 0\n\
    }\n\
    class Holder {\n\
    \x20   var reads = 0\n\
    \x20   val shape: Shape get() { reads++; return Square(reads) }\n\
    \x20   fun side(): Int = when (val s = shape) {\n\
    \x20       is Circle -> s.r\n\
    \x20       is Square -> s.side\n\
    \x20   }\n\
    \x20   fun kind(): Int = when (shape) {\n\
    \x20       is Circle -> 1\n\
    \x20       is Square -> 2\n\
    \x20   }\n\
    }\n\
    fun box(): String {\n\
    \x20   val first = area()\n\
    \x20   if (first != 2 || calls != 1) return \"area $first after $calls calls\"\n\
    \x20   calls = 0\n\
    \x20   val second = named()\n\
    \x20   if (second != \"other\" || calls != 1) return \"named $second after $calls calls\"\n\
    \x20   calls = 0\n\
    \x20   val third = negated()\n\
    \x20   if (third != \"other\" || calls != 1) return \"negated $third after $calls calls\"\n\
    \x20   val ranged = ranged()\n\
    \x20   if (ranged != 7 || numberReads != 1) return \"ranged $ranged after $numberReads reads\"\n\
    \x20   numberReads = 0\n\
    \x20   val outside = outside()\n\
    \x20   if (outside != 8 || numberReads != 1) return \"outside $outside after $numberReads reads\"\n\
    \x20   val holder = Holder()\n\
    \x20   val side = holder.side()\n\
    \x20   if (side != 1 || holder.reads != 1) return \"side $side after ${holder.reads} reads\"\n\
    \x20   val kind = holder.kind()\n\
    \x20   if (kind != 2 || holder.reads != 2) return \"kind $kind after ${holder.reads} reads\"\n\
    \x20   return \"OK\"\n\
    }\n";

#[test]
fn a_when_subject_is_evaluated_once() {
    common::expect_box_same_as_kotlinc(SOURCE, "WhenSubjectOnce");
}
