//! Mixed membership on an open-end range uses `rangeUntil` and then `contains`.
//! `3.0f in 1.0..<3.0` is outside; the inclusive `..` form of the same bounds is inside.
use super::common::expect_box_same_as_kotlinc;

#[test]
fn float_in_open_end_double_range_matches_kotlinc() {
    expect_box_same_as_kotlinc(
        "fun box(): String {\n\
         \x20 val end = 3.0f in 1.0..<3.0\n\
         \x20 val stored = (1.0..<3.0).contains(3.0f)\n\
         \x20 val mid = 2.0f in 1.0..<3.0\n\
         \x20 val closed = 3.0f in 1.0..3.0\n\
         \x20 val excluded = 3.0f !in 1.0..<3.0\n\
         \x20 return if (!end && !stored && mid && closed && excluded) \"OK\"\n\
         \x20 else \"end=$end stored=$stored mid=$mid closed=$closed excluded=$excluded\"\n\
         }\n",
        "float_in_open_end_double_range",
    );
}

#[test]
fn shadowed_uniform_double_range_until_matches_kotlinc() {
    expect_box_same_as_kotlinc(
        "class Span {\n\
         \x20 operator fun contains(value: Double): Boolean = value == 1.5\n\
         }\n\
         operator fun Double.rangeUntil(other: Double): Span = Span()\n\
         fun box(): String {\n\
         \x20 val selected = 1.5 in 0.0..<2.0\n\
         \x20 val other = 1.0 in 0.0..<2.0\n\
         \x20 return if (selected && !other) \"OK\" else \"selected=$selected other=$other\"\n\
         }\n",
        "shadowed_uniform_double_range_until",
    );
}
