//! `if (this is B)` narrows one receiver; a receiver lambda inside the branch has its own.

use super::common;

#[test]
fn a_receiver_lambda_keeps_its_own_this_inside_a_narrowed_receiver_branch() {
    const SOURCE: &str = include_str!("box_regressions/receiverNarrowingInReceiverLambda.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "receiver-narrowing-receiver-lambda");
}
