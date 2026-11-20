//! A suppressed visibility error does not promote an invisible member over a visible extension.

use super::common;

#[test]
fn a_suppressed_invisible_member_ranks_below_a_visible_extension() {
    const SOURCE: &str =
        include_str!("box_regressions/suppressedInvisibleMemberKeepsExtensionPriority.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "suppressed-invisible-member");
}
