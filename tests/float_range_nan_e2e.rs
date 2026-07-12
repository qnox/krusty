//! `!in` over a floating-point range must NOT be lowered via the De Morgan dual
//! (`value < lo || value > hi`): IEEE order isn't total, so `!(x <= NaN)` is `true` while
//! `x > NaN` is `false`, flipping membership for NaN bounds/values. The float path computes the
//! plain `in` chain and boolean-negates it (matching kotlinc's `!contains`).
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn not_in_float_range_with_nan_bound() {
    // range `0.0..NaN` contains nothing (any `<= NaN` is false), so `x !in` is always true.
    const SRC: &str = "fun box(): String {\n\
        \x20 val nan = Double.NaN\n\
        \x20 val r = 0.0 .. nan\n\
        \x20 val a = (1.0 !in r) && (1.0 !in 0.0 .. nan)\n\
        \x20 val b = (0.0 !in r) && (0.0 !in 0.0 .. nan)\n\
        \x20 val c = (nan !in r) && (nan !in 0.0 .. nan)\n\
        \x20 return if (a && b && c) \"OK\" else \"fail: $a $b $c\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("float !in NaN"), "OK");
}

#[test]
fn in_float_range_with_nan_bound_stays_correct() {
    // `in` (non-negated) was already correct; guard it doesn't regress.
    const SRC: &str = "fun box(): String {\n\
        \x20 val nan = Double.NaN\n\
        \x20 val r = 0.0 .. 10.0\n\
        \x20 val inside = 5.0 in r && 5.0 in 0.0 .. 10.0\n\
        \x20 val outside = !(20.0 in r) && !(20.0 in 0.0 .. 10.0)\n\
        \x20 val nanOut = !(nan in r) && !(nan in 0.0 .. 10.0)\n\
        \x20 return if (inside && outside && nanOut) \"OK\" else \"fail\"\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("float in NaN"), "OK");
}
