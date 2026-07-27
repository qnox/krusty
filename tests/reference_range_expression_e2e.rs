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

#[test]
fn heterogeneous_member_range_to_precedes_extension() {
    const SRC: &str = "class A(val v: Int) {\n\
    operator fun rangeTo(other: B): Span = Span(v + other.v)\n\
}\n\
class B(val v: Int)\n\
class Span(val marker: Int)\n\
operator fun A.rangeTo(other: B): Span = Span(-1)\n\
fun box(): String = if ((A(2)..B(3)).marker == 5) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("heterogeneous member rangeTo"), "OK");
}

#[test]
fn heterogeneous_extension_range_to() {
    const SRC: &str = "class A(val v: Int)\n\
class B(val v: Int)\n\
class Span(val marker: Int)\n\
operator fun A.rangeTo(other: B): Span = Span(v * other.v)\n\
fun box(): String = if ((A(2)..B(4)).marker == 8) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("heterogeneous extension rangeTo"), "OK");
}

#[test]
fn bounded_closed_range_bridge_selects_the_override_over_a_sibling() {
    const SRC: &str = "class V(val n: Int) : Comparable<V> {\n\
    override fun compareTo(other: V): Int = n.compareTo(other.n)\n\
}\n\
class R : ClosedRange<V> {\n\
    override val start: V = V(1)\n\
    override val endInclusive: V = V(3)\n\
    override fun contains(value: V): Boolean = value.n in 1..3\n\
    fun contains(value: String): Boolean = false\n\
}\n\
fun box(): String {\n\
    val range: ClosedRange<V> = R()\n\
    return if (V(2) in range && V(5) !in range) \"OK\" else \"fail\"\n\
}\n";
    assert_eq!(run(SRC).expect("bounded ClosedRange bridge"), "OK");
}

#[test]
fn classpath_extensions_honor_dependent_and_secondary_bounds() {
    const LIB: &str = "package dep\n\
interface Named\n\
fun <T> T.pick(): String where T : Comparable<T>, T : Named = \"bounded\"\n\
fun Any.pick(): String = \"fallback\"\n\
fun <U : CharSequence, T : U> T.twiceLength(): Int = length * 2\n";
    const MAIN: &str = "import dep.*\n\
class Good : Comparable<Good>, Named {\n\
    override fun compareTo(other: Good): Int = 0\n\
}\n\
class Bad : Comparable<Bad> {\n\
    override fun compareTo(other: Bad): Int = 0\n\
}\n\
fun box(): String {\n\
    if (Good().pick() != \"bounded\") return \"good\"\n\
    if (Bad().pick() != \"fallback\") return \"bad\"\n\
    if (\"abc\".twiceLength() != 6) return \"dependent\"\n\
    return \"OK\"\n\
}\n";
    if let Some(out) = common::run_box_against("reference_range_bounds", LIB, MAIN) {
        assert_eq!(out, "OK");
    }
}
