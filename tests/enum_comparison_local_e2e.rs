//! A block that declares a local and compares it with enum entries is not a subject `when`.

use super::common;

#[test]
fn enum_comparisons_on_a_local_are_not_an_enum_switch() {
    const SOURCE: &str = include_str!("box_regressions/enumComparisonsOnLocalAreNotSubjectWhen.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "enum-comparison-local");
}
