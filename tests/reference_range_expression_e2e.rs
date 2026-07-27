//! A standalone range expression `a..b` over REFERENCE operands: `Comparable<T>.rangeTo` (stdlib,
//! `"a".."c"` → `ClosedRange<String>`) or a user `operator fun rangeTo`. Mirrors the corpus
//! `ranges/` non-primitive shapes; the fused `x in a..b` form was already supported.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn string_range_binds_and_contains() {
    const SRC: &str = "fun box(): String {\n\
    val r = \"a\"..\"c\"\n\
    if (\"b\" !in r) return \"fail 1\"\n\
    if (\"d\" in r) return \"fail 2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("string range"), "OK");
}

#[test]
fn user_comparable_range_binds_and_contains() {
    const SRC: &str = "class V(val n: Int) : Comparable<V> {\n\
    override fun compareTo(other: V): Int = n.compareTo(other.n)\n\
}\n\
fun box(): String {\n\
    val r = V(1)..V(3)\n\
    if (V(2) !in r) return \"fail 1\"\n\
    if (V(4) in r) return \"fail 2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("user comparable range"), "OK");
}

#[test]
fn user_range_to_operator_range_value() {
    // A USER `operator fun rangeTo` returning a custom range-like object.
    const SRC: &str = "class P(val v: Int) {\n\
    operator fun rangeTo(other: P): Span = Span(v, other.v)\n\
}\n\
class Span(val lo: Int, val hi: Int) {\n\
    operator fun contains(p: P): Boolean = p.v in lo..hi\n\
}\n\
fun box(): String {\n\
    val s = P(1)..P(5)\n\
    if (P(3) !in s) return \"fail 1\"\n\
    if (P(9) in s) return \"fail 2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("user rangeTo operator"), "OK");
}
