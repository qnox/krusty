//! A source `&&`/`||` is laid out as short-circuit jumps, as kotlinc lays out `ANDAND`/`OROR`: in a
//! condition each operand branches on its own, and as a value both operands jump to one shared
//! `iconst_0`. The Boolean is never materialized and tested again. The same `if (a) b else false`
//! written by hand keeps kotlinc's materialized form, so the two must stay distinguishable.
use super::common;

#[test]
fn short_circuit_operators_are_byte_identical_to_kotlinc() {
    let src = "\
fun both(a: Int, b: Int): Int {
    if (a > 0 && b > 0) return 1
    return 0
}

fun either(a: String, b: String): Int {
    if (a == \"x\" || b == \"y\") return 1
    return 0
}

fun nested(a: Int, b: Int, c: Int): Int {
    if ((a > 0 || b > 0) && c > 0) return 1
    return 0
}

fun negated(a: Int, b: Int): Int {
    if (!(a > 0 && b > 0)) return 1
    return 0
}

fun bounded(a: Int): Int {
    var n = 0
    while (n < a && n < 10) n += 2
    return n
}

fun value(a: Int, b: Int): Boolean = a > 0 && b > 0

fun valueEither(a: Int, b: Int): Boolean = a > 0 || b > 0

fun falseLeft(a: Int): Boolean = false && a > 0

fun falseRight(a: Int): Boolean = a > 0 && false

fun trueLeft(a: Int): Boolean = true || a > 0

fun trueRight(a: Int): Boolean = a > 0 || true

fun handWritten(a: Int, b: Int): Int {
    if (if (a > 0) b > 0 else false) return 1
    return 0
}
";
    match common::byte_diff_against_kotlinc_cp(
        "ShortCircuit",
        src,
        "ShortCircuitKt",
        &[common::stdlib_jar()],
    ) {
        None => eprintln!("skip (ShortCircuit: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(why)) => panic!("{why}"),
    }
}

/// The right operand runs only when the left one does not decide the result.
#[test]
fn short_circuit_operators_skip_the_right_operand() {
    let src = "\
var log = \"\"

fun t(name: String, value: Boolean): Boolean {
    log += name
    return value
}

fun box(): String {
    if (t(\"a\", false) && t(\"b\", true)) return \"FAIL and\"
    if (!(t(\"c\", true) || t(\"d\", true))) return \"FAIL or\"
    val v = t(\"e\", true) && t(\"f\", false)
    if (v) return \"FAIL value\"
    val w = t(\"g\", false) || t(\"h\", true)
    if (!w) return \"FAIL value or\"
    var n = 0
    while (t(\"i\", n < 2) && t(\"j\", true)) n++
    return if (log == \"acefghijiji\") \"OK\" else \"FAIL log: \" + log
}
";
    common::expect_box_same_as_kotlinc(src, "ShortCircuitOrder");
}
