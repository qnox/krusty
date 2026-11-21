//! A constructor argument written after a spread or array vararg still names its own parameter.

use super::common;

#[test]
fn named_constructor_arguments_follow_a_spread_vararg() {
    const SOURCE: &str = include_str!("box_regressions/constructorVarargBeforeNamedArguments.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "constructor-vararg-before-named");
}
